//! Where the disk time is going.
//!
//! Three separate questions, from three separate views, because "the database is
//! slow on I/O" has three separate causes:
//!
//! 1. **Which table is being read off disk** — `pg_statio_user_tables`, per
//!    database. `pg_stat_database.blk_read_time` says the database waited; this says
//!    what it waited for, split into heap, index and TOAST.
//! 2. **How much is being written per byte of data** — `pg_stat_wal`, cluster-wide
//!    (14+). Full-page images are the usual surprise: the first write to a page
//!    after each checkpoint copies the whole 8 kB page into WAL, so frequent
//!    checkpoints multiply write volume without anyone changing more rows.
//! 3. **Who is writing the buffers** — the checkpointer, the background writer, or
//!    the backends themselves. Backends writing their own buffers means they are
//!    stalling to make room, which is the difference between "the disk is busy" and
//!    "queries are waiting for the disk".
//!
//! # `blks_read` is not "read from disk"
//!
//! Everywhere in this module, a "read" means *not served from Postgres' shared
//! buffers*. The page may still have come from the operating system's page cache
//! without the disk moving at all. That is why the timing columns matter more than
//! the block counts: time is what was actually lost.

use std::collections::HashMap;
use std::time::Duration;

use my_postgres::tokio_postgres::Row;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::postgres::{PostgresAccess, opt_i64, opt_string, stats_row};

use super::ServerCapabilities;

/// `pg_stat_wal` arrived in 14.
const PG14: i32 = 140000;

/// 17 split the checkpointer's counters out of `pg_stat_bgwriter` into
/// `pg_stat_checkpointer`, and moved `buffers_backend` to `pg_stat_io` entirely.
const PG17: i32 = 170000;

/// Postgres' block size in bytes. Configurable at compile time, but 8 kB on every
/// build anyone runs; used only to render block counts as a size, never to compute
/// anything that is reported as exact.
const BLOCK_BYTES: i64 = 8192;

/// How many tables the read list carries.
const TOP_N: usize = 15;

/// Rows pulled before ranking by *recent* reads — see [`super::statements`] for the
/// same reasoning: a table that only started missing cache in the last minute can sit
/// well down the lifetime list.
const FETCH_N: usize = 60;

/// One table's block accounting, as the view reports it (cumulative).
#[derive(Debug, Clone)]
struct TableIoSample {
    schema_name: Option<String>,
    table_name: Option<String>,
    heap_read: Option<i64>,
    heap_hit: Option<i64>,
    idx_read: Option<i64>,
    idx_hit: Option<i64>,
    toast_read: Option<i64>,
    total_read: Option<i64>,
}

impl TableIoSample {
    fn read_row(row: &Row) -> Self {
        Self {
            schema_name: opt_string(row, "schema_name"),
            table_name: opt_string(row, "table_name"),
            heap_read: opt_i64(row, "heap_read"),
            heap_hit: opt_i64(row, "heap_hit"),
            idx_read: opt_i64(row, "idx_read"),
            idx_hit: opt_i64(row, "idx_hit"),
            toast_read: opt_i64(row, "toast_read"),
            total_read: opt_i64(row, "total_read"),
        }
    }

    fn key(&self) -> Option<String> {
        Some(format!(
            "{}.{}",
            self.schema_name.as_ref()?,
            self.table_name.as_ref()?
        ))
    }
}

stats_row!(TableIoSample);

/// What a table cost in reads, lifetime and since the previous tick.
#[derive(Debug, Clone)]
pub struct TableIo {
    pub schema_name: Option<String>,
    pub table_name: Option<String>,
    /// Heap + index + TOAST blocks not served from shared buffers, since the reset.
    pub total_read_blocks: Option<i64>,
    pub heap_read_blocks: Option<i64>,
    pub index_read_blocks: Option<i64>,
    pub toast_read_blocks: Option<i64>,
    /// Share of this table's block accesses that shared buffers satisfied. The number
    /// that separates "big table, all cached" from "big table, read off disk".
    pub cache_hit_ratio: Option<f64>,
    /// Blocks read since the previous tick, and the same as bytes.
    pub delta_read_blocks: Option<i64>,
    pub delta_read_bytes: Option<i64>,
    /// Bytes per wall-clock second over that window — the "damage right now" figure.
    pub read_bytes_per_sec: Option<f64>,
}

/// Cluster-wide write accounting.
#[derive(Debug, Clone)]
pub struct WriteIo {
    /// WAL produced since the reset (14+).
    pub wal_bytes: Option<i64>,
    pub wal_records: Option<i64>,
    /// Full-page images: whole 8 kB pages copied into WAL on the first write after a
    /// checkpoint. A high share of WAL is the signature of checkpoints that are too
    /// frequent for the write rate.
    pub wal_full_page_images: Option<i64>,
    pub wal_bytes_per_sec: Option<f64>,
    /// Checkpoints that ran because `checkpoint_timeout` came round.
    pub checkpoints_timed: Option<i64>,
    /// Checkpoints forced early because WAL hit `max_wal_size`. A ratio of these
    /// above roughly a third of all checkpoints is the classic sign that
    /// `max_wal_size` is too small — and each forced checkpoint restarts the
    /// full-page-image cost above.
    pub checkpoints_requested: Option<i64>,
    pub buffers_written_by_checkpointer: Option<i64>,
    pub buffers_written_by_bgwriter: Option<i64>,
    /// Buffers written by the query backends themselves, because no clean buffer was
    /// free. This is a query stalling to do the writer's job. `None` on 17+, where
    /// the counter moved to `pg_stat_io`.
    pub buffers_written_by_backends: Option<i64>,
}

/// The disk-I/O section.
#[derive(Debug, Clone)]
pub struct DiskIo {
    /// False when `track_io_timing` is off, which makes every *timing* figure on the
    /// page unavailable; the block counts here still work.
    pub io_timing_enabled: bool,
    /// Ranked by recent reads once there is a previous tick to compare with,
    /// otherwise by lifetime reads.
    pub tables: Vec<TableIo>,
    pub writes: Option<WriteIo>,
    /// Why the write half is missing, when it is.
    pub writes_unavailable: Option<String>,
}

/// The previous tick's per-table counters, so the next one can report what moved.
#[derive(Debug, Clone)]
pub struct DiskIoSnapshot {
    taken_at: DateTimeAsMicroseconds,
    reads_by_table: HashMap<String, i64>,
    wal_bytes: Option<i64>,
}

/// `pg_statio_user_tables` covers ordinary tables in this database. TOAST is counted
/// with its index, since a TOAST fetch always walks the index first and splitting
/// them tells the operator nothing they can act on.
fn tables_sql() -> String {
    format!(
        r#"
SELECT
    s.schemaname::text                                                       AS schema_name,
    s.relname::text                                                          AS table_name,
    s.heap_blks_read                                                         AS heap_read,
    s.heap_blks_hit                                                          AS heap_hit,
    s.idx_blks_read                                                          AS idx_read,
    s.idx_blks_hit                                                           AS idx_hit,
    COALESCE(s.toast_blks_read, 0) + COALESCE(s.tidx_blks_read, 0)           AS toast_read,
    COALESCE(s.heap_blks_read, 0) + COALESCE(s.idx_blks_read, 0)
        + COALESCE(s.toast_blks_read, 0) + COALESCE(s.tidx_blks_read, 0)     AS total_read
FROM pg_statio_user_tables s
ORDER BY total_read DESC
LIMIT {}
"#,
        FETCH_N
    )
}

/// Both halves of the write story in one row.
///
/// `wal_bytes` is `numeric` in the catalog, which this server's row reader has no
/// mapping for — cast to `int8` here rather than letting it arrive as the
/// "[unsupported pg type]" placeholder. int8 tops out at 9 exabytes of WAL, so the
/// cast cannot overflow in practice.
fn writes_sql(server_version_num: i32) -> Option<String> {
    if server_version_num < PG14 {
        return None;
    }

    // 17 renamed the checkpointer's counters and moved them to their own view.
    let checkpoints = if server_version_num >= PG17 {
        r#"(SELECT c.num_timed FROM pg_stat_checkpointer c)        AS checkpoints_timed,
    (SELECT c.num_requested FROM pg_stat_checkpointer c)           AS checkpoints_requested,
    (SELECT c.buffers_written FROM pg_stat_checkpointer c)         AS buffers_checkpointer,
    -- Moved to pg_stat_io in 17; NULL rather than a zero that would read as
    -- "backends never had to write their own buffers".
    NULL::int8                                                     AS buffers_backend"#
    } else {
        r#"(SELECT b.checkpoints_timed FROM pg_stat_bgwriter b)    AS checkpoints_timed,
    (SELECT b.checkpoints_req FROM pg_stat_bgwriter b)             AS checkpoints_requested,
    (SELECT b.buffers_checkpoint FROM pg_stat_bgwriter b)          AS buffers_checkpointer,
    (SELECT b.buffers_backend FROM pg_stat_bgwriter b)             AS buffers_backend"#
    };

    Some(format!(
        r#"
SELECT
    (SELECT w.wal_bytes::int8 FROM pg_stat_wal w)                  AS wal_bytes,
    (SELECT w.wal_records FROM pg_stat_wal w)                      AS wal_records,
    (SELECT w.wal_fpi FROM pg_stat_wal w)                          AS wal_fpi,
    (SELECT b.buffers_clean FROM pg_stat_bgwriter b)               AS buffers_bgwriter,
    {}
"#,
        checkpoints
    ))
}

struct WriteIoSample {
    wal_bytes: Option<i64>,
    wal_records: Option<i64>,
    wal_fpi: Option<i64>,
    buffers_bgwriter: Option<i64>,
    checkpoints_timed: Option<i64>,
    checkpoints_requested: Option<i64>,
    buffers_checkpointer: Option<i64>,
    buffers_backend: Option<i64>,
}

impl WriteIoSample {
    fn read_row(row: &Row) -> Self {
        Self {
            wal_bytes: opt_i64(row, "wal_bytes"),
            wal_records: opt_i64(row, "wal_records"),
            wal_fpi: opt_i64(row, "wal_fpi"),
            buffers_bgwriter: opt_i64(row, "buffers_bgwriter"),
            checkpoints_timed: opt_i64(row, "checkpoints_timed"),
            checkpoints_requested: opt_i64(row, "checkpoints_requested"),
            buffers_checkpointer: opt_i64(row, "buffers_checkpointer"),
            buffers_backend: opt_i64(row, "buffers_backend"),
        }
    }
}

stats_row!(WriteIoSample);

/// Same rule as everywhere else here: a counter that went backwards means the
/// statistics were reset, and the honest answer for that window is "not known".
fn delta(current: Option<i64>, previous: Option<i64>) -> Option<i64> {
    let (current, previous) = (current?, previous?);

    if current < previous {
        return None;
    }

    Some(current - previous)
}

fn hit_ratio(hit: i64, read: i64) -> Option<f64> {
    let total = hit + read;

    if total <= 0 {
        return None;
    }

    Some(hit as f64 / total as f64)
}

/// Turns the raw samples into the published section and the snapshot the next tick
/// diffs against.
fn build(
    samples: Vec<TableIoSample>,
    writes: Option<WriteIoSample>,
    previous: Option<&DiskIoSnapshot>,
    io_timing_enabled: bool,
    writes_unavailable: Option<String>,
) -> (DiskIo, DiskIoSnapshot) {
    let taken_at = DateTimeAsMicroseconds::now();

    let window_secs = previous
        .map(|previous| {
            taken_at.duration_since(previous.taken_at).get_full_micros() as f64 / 1_000_000.0
        })
        .filter(|secs| *secs >= 1.0);

    let mut reads_by_table = HashMap::with_capacity(samples.len());

    let mut tables: Vec<TableIo> = samples
        .into_iter()
        .map(|sample| {
            let key = sample.key();

            let earlier = key
                .as_ref()
                .zip(previous)
                .and_then(|(key, previous)| previous.reads_by_table.get(key).copied());

            if let (Some(key), Some(total)) = (key, sample.total_read) {
                reads_by_table.insert(key, total);
            }

            let delta_read_blocks = delta(sample.total_read, earlier);

            let hits = sample.heap_hit.unwrap_or(0) + sample.idx_hit.unwrap_or(0);
            let reads = sample.heap_read.unwrap_or(0) + sample.idx_read.unwrap_or(0);

            TableIo {
                schema_name: sample.schema_name,
                table_name: sample.table_name,
                total_read_blocks: sample.total_read,
                heap_read_blocks: sample.heap_read,
                index_read_blocks: sample.idx_read,
                toast_read_blocks: sample.toast_read,
                cache_hit_ratio: hit_ratio(hits, reads),
                delta_read_blocks,
                delta_read_bytes: delta_read_blocks.map(|blocks| blocks * BLOCK_BYTES),
                read_bytes_per_sec: delta_read_blocks.zip(window_secs).map(|(blocks, secs)| {
                    (blocks * BLOCK_BYTES) as f64 / secs
                }),
            }
        })
        .collect();

    // Rank by what moved, with the lifetime total as the tiebreak — which is the
    // whole ordering on the first tick, where nothing has a delta yet. Sorted
    // unconditionally rather than only when some delta exists: leaning on the query's
    // ORDER BY for that case would make the ranking depend on which SQL the caller
    // happened to run.
    tables.sort_by(|left, right| {
        let key = |table: &TableIo| {
            (
                table.delta_read_blocks.unwrap_or(0),
                table.total_read_blocks.unwrap_or(0),
            )
        };

        key(right).cmp(&key(left))
    });

    tables.truncate(TOP_N);

    let wal_bytes = writes.as_ref().and_then(|writes| writes.wal_bytes);

    let published_writes = writes.map(|writes| WriteIo {
        wal_bytes: writes.wal_bytes,
        wal_records: writes.wal_records,
        wal_full_page_images: writes.wal_fpi,
        wal_bytes_per_sec: delta(writes.wal_bytes, previous.and_then(|p| p.wal_bytes))
            .zip(window_secs)
            .map(|(bytes, secs)| bytes as f64 / secs),
        checkpoints_timed: writes.checkpoints_timed,
        checkpoints_requested: writes.checkpoints_requested,
        buffers_written_by_checkpointer: writes.buffers_checkpointer,
        buffers_written_by_bgwriter: writes.buffers_bgwriter,
        buffers_written_by_backends: writes.buffers_backend,
    });

    (
        DiskIo {
            io_timing_enabled,
            tables,
            writes: published_writes,
            writes_unavailable,
        },
        DiskIoSnapshot {
            taken_at,
            reads_by_table,
            wal_bytes,
        },
    )
}

pub async fn collect_disk_io(
    postgres: &PostgresAccess,
    capabilities: &ServerCapabilities,
    previous: Option<&DiskIoSnapshot>,
    timeout: Duration,
) -> Result<(DiskIo, DiskIoSnapshot), String> {
    let tables: Vec<TableIoSample> = postgres
        .query_typed("db_stats/table_io", tables_sql().as_str(), timeout)
        .await?;

    let (writes, writes_unavailable) = match writes_sql(capabilities.server_version_num) {
        None => (
            None,
            Some(format!(
                "WAL and checkpoint accounting needs pg_stat_wal, which arrived in Postgres 14; \
                 this server reports {}.",
                capabilities.server_version
            )),
        ),
        Some(sql) => match postgres
            .query_typed::<WriteIoSample>("db_stats/write_io", sql.as_str(), timeout)
            .await
        {
            // Cluster-wide views, unlike the per-database one above: a restricted
            // account can be refused here while the table list still works, so the
            // failure is reported against the write half alone rather than taking the
            // whole section down.
            Err(err) => (None, Some(err)),
            Ok(rows) => (rows.into_iter().next(), None),
        },
    };

    Ok(build(
        tables,
        writes,
        previous,
        capabilities.track_io_timing,
        writes_unavailable,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(name: &str, heap_read: i64, heap_hit: i64, idx_read: i64) -> TableIoSample {
        TableIoSample {
            schema_name: Some("public".to_string()),
            table_name: Some(name.to_string()),
            heap_read: Some(heap_read),
            heap_hit: Some(heap_hit),
            idx_read: Some(idx_read),
            idx_hit: Some(0),
            toast_read: Some(0),
            total_read: Some(heap_read + idx_read),
        }
    }

    fn snapshot_at(secs: i64, reads: &[(&str, i64)], wal_bytes: Option<i64>) -> DiskIoSnapshot {
        DiskIoSnapshot {
            taken_at: DateTimeAsMicroseconds::new(secs * 1_000_000),
            reads_by_table: reads
                .iter()
                .map(|(name, blocks)| (format!("public.{}", name), *blocks))
                .collect(),
            wal_bytes,
        }
    }

    #[test]
    fn the_first_tick_ranks_by_lifetime_reads_and_has_no_deltas() {
        let (io, snapshot) = build(
            vec![table("small", 10, 990, 0), table("big", 5_000, 100, 500)],
            None,
            None,
            true,
            None,
        );

        assert_eq!(io.tables[0].table_name.as_deref(), Some("big"));
        assert_eq!(io.tables[0].delta_read_blocks, None);
        assert_eq!(io.tables[0].read_bytes_per_sec, None);
        assert_eq!(snapshot.reads_by_table.len(), 2);
    }

    #[test]
    fn a_table_that_started_missing_cache_recently_outranks_a_bigger_lifetime_total() {
        // "big" has read far more overall but nothing since the last tick.
        let previous = snapshot_at(0, &[("big", 5_500), ("small", 10)], None);

        let (io, _) = build(
            vec![table("small", 2_010, 990, 0), table("big", 5_000, 100, 500)],
            None,
            Some(&previous),
            true,
            None,
        );

        assert_eq!(io.tables[0].table_name.as_deref(), Some("small"));
        assert_eq!(io.tables[0].delta_read_blocks, Some(2_000));
        // Blocks are 8 kB.
        assert_eq!(io.tables[0].delta_read_bytes, Some(2_000 * 8192));
    }

    #[test]
    fn the_cache_hit_ratio_separates_a_big_cached_table_from_a_big_uncached_one() {
        let (io, _) = build(
            vec![table("cached", 10, 9_990, 0), table("uncached", 9_000, 1_000, 0)],
            None,
            None,
            true,
            None,
        );

        let cached = io
            .tables
            .iter()
            .find(|t| t.table_name.as_deref() == Some("cached"))
            .unwrap();
        let uncached = io
            .tables
            .iter()
            .find(|t| t.table_name.as_deref() == Some("uncached"))
            .unwrap();

        assert_eq!(cached.cache_hit_ratio, Some(0.999));
        assert_eq!(uncached.cache_hit_ratio, Some(0.1));
    }

    #[test]
    fn a_stats_reset_drops_the_delta_rather_than_going_negative() {
        let previous = snapshot_at(0, &[("t", 900_000)], Some(1_000_000));

        let (io, _) = build(
            vec![table("t", 5, 10, 0)],
            None,
            Some(&previous),
            true,
            None,
        );

        assert_eq!(io.tables[0].delta_read_blocks, None);
        assert_eq!(io.tables[0].read_bytes_per_sec, None);
        // The lifetime figure is still what the view says.
        assert_eq!(io.tables[0].total_read_blocks, Some(5));
    }

    #[test]
    fn wal_throughput_comes_from_the_difference_between_two_reads() {
        let now = DateTimeAsMicroseconds::now();
        let previous = DiskIoSnapshot {
            taken_at: DateTimeAsMicroseconds::new(now.unix_microseconds - 10_000_000),
            reads_by_table: HashMap::new(),
            wal_bytes: Some(1_000_000),
        };

        let writes = WriteIoSample {
            wal_bytes: Some(11_000_000),
            wal_records: Some(500),
            wal_fpi: Some(100),
            buffers_bgwriter: Some(10),
            checkpoints_timed: Some(4),
            checkpoints_requested: Some(1),
            buffers_checkpointer: Some(200),
            buffers_backend: Some(7),
        };

        let (io, _) = build(Vec::new(), Some(writes), Some(&previous), true, None);

        // 10 MB over ~10 seconds.
        let per_sec = io.writes.as_ref().unwrap().wal_bytes_per_sec.unwrap();
        assert!(
            (per_sec - 1_000_000.0).abs() < 60_000.0,
            "expected ~1 MB/s, got {}",
            per_sec
        );
    }

    #[test]
    fn a_server_without_pg_stat_wal_reports_why_rather_than_zeros() {
        assert!(writes_sql(130000).is_none());
        assert!(writes_sql(140000).is_some());
    }

    #[test]
    fn postgres_17_reads_the_checkpointer_from_its_own_view() {
        let before = writes_sql(160000).unwrap();
        let after = writes_sql(170000).unwrap();

        assert!(before.contains("b.checkpoints_timed FROM pg_stat_bgwriter"));
        assert!(after.contains("pg_stat_checkpointer"));
        // buffers_backend moved to pg_stat_io in 17 — reported as unknown, not zero.
        assert!(after.contains("NULL::int8"));
    }

    #[test]
    fn io_timing_being_off_does_not_stop_the_block_counts() {
        let (io, _) = build(vec![table("t", 100, 900, 0)], None, None, false, None);

        assert!(!io.io_timing_enabled);
        assert_eq!(io.tables[0].total_read_blocks, Some(100));
    }
}
