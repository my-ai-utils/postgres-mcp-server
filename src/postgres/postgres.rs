use std::{sync::Arc, time::Duration};

use my_json5::json_writer::{JsonArrayWriter, RawJsonObject};
use my_postgres::{
    MyPostgres, RequestContext,
    sql::{SqlData, SqlValues},
    sql_select::SelectEntity,
    tokio_postgres::{Row, types::Type},
};
use rust_extensions::date_time::DateTimeAsMicroseconds;

pub struct PostgresAccess {
    postgres: MyPostgres,
}

/// The rendered JSON plus the row count, which the caller records in the
/// requests log. `execute_sql_as_vec` goes through the extended protocol
/// (`connection.query`), so a write without `RETURNING` comes back as an empty
/// vec — `rows` is "rows returned", never "rows affected".
pub struct SqlResponseResult {
    pub json: String,
    pub rows: usize,
}

impl PostgresAccess {
    /// One connection per mounted database. `app_name` lands in the Postgres
    /// `application_name`, and `settings` resolves the connection string of this
    /// mount alone — see [`crate::settings::DbConnectionSettings`].
    pub async fn new(
        app_name: String,
        settings: Arc<crate::settings::DbConnectionSettings>,
    ) -> Self {
        Self {
            postgres: MyPostgres::from_settings(app_name, settings)
                .build()
                .await,
        }
    }

    /// Runs SQL the **server itself** issues — the statistics queries in
    /// [`crate::db_stats`] — and maps the rows into `TEntity`.
    ///
    /// Deliberately a separate path from [`Self::do_request`], not a flag on it:
    ///
    /// - it never consults the write gate, because these statements are not the
    ///   agent's and there is no user to grant anything (they are all plain
    ///   catalog reads, and the blunt keyword pass would refuse half of them for
    ///   mentioning `pg_stat_user_tables.n_tup_upd` anyway);
    /// - it is never written to the request log, because a 5-second poller would
    ///   evict the agent's own 100 entries within minutes and leave the operator
    ///   staring at a log of the server talking to itself;
    /// - it carries its own, shorter timeout, so a wedged database delays one
    ///   collection tick instead of holding it for the full 10 seconds the tool
    ///   allows.
    ///
    /// `process_name` is `&'static str` because every caller is a fixed,
    /// hand-written query — it labels the request in the driver's own
    /// diagnostics, and there is no user input to put there.
    ///
    /// `timeout` is handed to the driver, **and** the whole call is bounded at
    /// twice that. The second bound is not belt-and-braces: `sql_request_time_out`
    /// applies to one attempt, and against an unreachable host the driver retries
    /// underneath it — measured at ~85 seconds for a single call, which blew
    /// through the collection timer's own iteration timeout and left the slow
    /// sections stuck on "pending" forever instead of reporting that the database
    /// was down. The outer bound turns "wedged collector" into "one tick reported a
    /// timeout".
    ///
    /// The two are deliberately not equal. A genuinely slow query is the driver's
    /// to cancel, because it cancels server-side and hands the connection back
    /// clean; dropping the future instead would return a connection with unread
    /// results to the pool. So the outer bound sits far enough above the driver's
    /// that it only ever fires when the driver itself is stuck — i.e. when there is
    /// no connection to dirty.
    pub async fn query_typed<TEntity: SelectEntity + Send + Sync + 'static>(
        &self,
        process_name: &'static str,
        sql: &str,
        timeout: Duration,
    ) -> Result<Vec<TEntity>, String> {
        let sql_data = SqlData {
            sql: sql.to_string(),
            values: SqlValues::Empty,
        };

        // Bound to a local: the future below borrows it, so an inline temporary
        // would not live long enough to be awaited.
        let ctx = RequestContext {
            started: DateTimeAsMicroseconds::now(),
            process_name: Arc::new(process_name.to_string()),
            sql_request_time_out: timeout,
            is_debug: false,
        };

        let query = self.postgres.execute_sql_as_vec(sql_data, &ctx);

        match tokio::time::timeout(timeout * 2, query).await {
            Ok(result) => result.map_err(|err| format!("{:?}", err)),
            Err(_) => Err(format!(
                "'{}' gave up after {}s — the driver did not return, which usually means the \
                 database is unreachable and it is still retrying.",
                process_name,
                (timeout * 2).as_secs()
            )),
        }
    }

    /// Runs a fixed statement that returns no rows, on the operator's behalf.
    ///
    /// Separate from both other paths on purpose:
    ///
    /// - not [`Self::do_request`], because there is no agent here and no write window
    ///   to consult — the operator clicked a button, which *is* the authorisation
    ///   this server has;
    /// - not [`Self::query_typed`], because that one is for reads that produce rows.
    ///
    /// `sql` is `&'static str` and that is the safety property: every caller is a
    /// statement written out in full in this repository, so there is no input to
    /// escape and no way for a request body to reach the parser. Anything that needs
    /// a value from the client does not belong on this path.
    pub async fn execute_statement(
        &self,
        sql: &'static str,
        timeout: Duration,
    ) -> Result<(), String> {
        let sql_data = SqlData {
            sql: sql.to_string(),
            values: SqlValues::Empty,
        };

        // Bounded the same way as the statistics queries: the driver retries beneath
        // its own timeout, so without an outer deadline an unreachable database would
        // leave the operator's click hanging for a minute and a half.
        match tokio::time::timeout(timeout * 2, self.postgres.execute_sql(sql_data)).await {
            Ok(result) => result.map(|_| ()).map_err(|err| format!("{:?}", err)),
            Err(_) => Err(format!(
                "The statement did not return within {}s — the database is most likely \
                 unreachable.",
                (timeout * 2).as_secs()
            )),
        }
    }

    pub async fn do_request(&self, sql: String) -> Result<SqlResponseResult, String> {
        let sql_data = SqlData {
            sql: sql.to_string(),
            values: SqlValues::Empty,
        };
        let items: Vec<SqlResponse> = self
            .postgres
            .execute_sql_as_vec(
                sql_data,
                &RequestContext {
                    started: DateTimeAsMicroseconds::now(),
                    process_name: Arc::new(sql),
                    sql_request_time_out: Duration::from_secs(10),
                    is_debug: false,
                },
            )
            .await
            .map_err(|err| format!("{:?}", err))?;

        let mut columns_arr = my_json5::json_writer::JsonArrayWriter::new();
        if let Some(first) = items.first() {
            for c in &first.columns {
                columns_arr = columns_arr.write(c.as_str());
            }
        }

        let rows = items.len();

        let mut rows_arr = my_json5::json_writer::JsonArrayWriter::new();
        for itm in items {
            rows_arr = rows_arr.write(RawJsonObject::AsString(itm.values_json));
        }

        let result = my_json5::json_writer::JsonObjectWriter::new()
            .write("columns", RawJsonObject::AsString(columns_arr.build()))
            .write("rows", RawJsonObject::AsString(rows_arr.build()));

        Ok(SqlResponseResult {
            json: result.build(),
            rows,
        })
    }
}

pub struct SqlResponse {
    columns: Vec<String>,
    values_json: String,
}

impl SelectEntity for SqlResponse {
    fn from(row: &my_postgres::tokio_postgres::Row) -> Self {
        let mut columns = Vec::with_capacity(row.columns().len());
        let mut values = my_json5::json_writer::JsonArrayWriter::new();

        for (index, column) in row.columns().iter().enumerate() {
            columns.push(column.name().to_string());
            values = write_value(values, row, index, column.type_());
        }

        Self {
            columns,
            values_json: values.build(),
        }
    }

    fn fill_select_fields(_select_builder: &mut my_postgres::sql::SelectBuilder) {}

    fn get_order_by_fields() -> Option<&'static str> {
        None
    }

    fn get_group_by_fields() -> Option<&'static str> {
        None
    }
}

fn write_value(values: JsonArrayWriter, row: &Row, index: usize, ty: &Type) -> JsonArrayWriter {
    macro_rules! direct {
        ($t:ty) => {
            match row.try_get::<_, Option<$t>>(index) {
                Ok(Some(v)) => values.write(v),
                _ => values.write_null_element(),
            }
        };
    }
    macro_rules! as_string {
        ($t:ty, |$v:ident| $body:expr) => {
            match row.try_get::<_, Option<$t>>(index) {
                Ok(Some($v)) => values.write($body),
                _ => values.write_null_element(),
            }
        };
    }

    match *ty {
        Type::BOOL => direct!(bool),
        Type::INT2 => direct!(i16),
        Type::INT4 => direct!(i32),
        Type::INT8 => direct!(i64),
        Type::OID => as_string!(u32, |v| v as i64),
        Type::FLOAT4 => direct!(f32),
        Type::FLOAT8 => direct!(f64),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => direct!(String),
        Type::UUID => as_string!(uuid::Uuid, |v| v.to_string()),
        Type::TIMESTAMP => {
            as_string!(chrono::NaiveDateTime, |v| v
                .format("%Y-%m-%dT%H:%M:%S%.f")
                .to_string())
        }
        Type::TIMESTAMPTZ => {
            as_string!(chrono::DateTime<chrono::Utc>, |v| v.to_rfc3339())
        }
        Type::DATE => as_string!(chrono::NaiveDate, |v| v.to_string()),
        Type::TIME => as_string!(chrono::NaiveTime, |v| v.to_string()),
        Type::JSON | Type::JSONB => match row.try_get::<_, Option<serde_json::Value>>(index) {
            Ok(Some(v)) => values.write(RawJsonObject::AsString(v.to_string())),
            _ => values.write_null_element(),
        },
        Type::BYTEA => as_string!(Vec<u8>, |v| bytes_to_hex(&v)),
        _ => values.write(format!("[unsupported pg type: {}]", ty.name())),
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("\\x");
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}
