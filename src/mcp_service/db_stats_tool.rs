use std::sync::Arc;

use my_ai_agent::{ToolDefinition, macros::ApplyJsonSchema};
use serde::*;

use crate::app::DbContext;
use crate::db_stats::DbStatsModel;
use mcp_server_middleware::McpToolCall;

/// Sections the agent may ask for. Anything unrecognized — including an empty
/// string, which is what a client that omits the argument sends — means all of
/// them, because a typo should cost a wide answer rather than an error.
const SECTION_TABLES: &str = "tables";
const SECTION_LOAD: &str = "load";
const SECTION_ACTIVITY: &str = "activity";
const SECTION_HEALTH: &str = "health";
const SECTION_IO: &str = "io";
const SECTION_MINUTES: &str = "minutes";

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct DbStatsToolCallRequest {
    #[property(
        description: "Which section to return. One of: 'tables' (largest tables with their size, row estimates, scan counts and last vacuum), 'load' (the statements consuming the most execution time), 'activity' (connections and the longest-running queries), 'health' (database-wide counters and throughput), 'io' (which tables are read off disk, plus WAL and checkpoint write volume), 'minutes' (the last minute's statement count, average execution time and longest query), or 'all'. Defaults to 'all'."
    )]
    pub section: String,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct DbStatsToolCallResponse {
    #[property( description: "Statistics as json")]
    pub stats_as_json: String,
}

/// The `db_stats` tool of a single MCP endpoint.
///
/// Bound to one [`DbContext`] exactly like [`super::PostgresMcpService`], and for
/// the same reason: the numbers describe one database, the tool takes no database
/// argument, and nothing in the response names another mount.
///
/// The tool reads the collector's cache and never issues SQL of its own. So a
/// tool call costs nothing on the database, cannot be used to hammer it, and is
/// not affected by — nor does it appear in — the request log. The trade is
/// staleness, which is bounded and stated in the tool description: seconds for
/// activity, up to a minute for sizes.
pub struct DbStatsMcpService {
    db: Arc<DbContext>,
}

impl DbStatsMcpService {
    pub fn new(db: Arc<DbContext>) -> Self {
        Self { db }
    }
}

impl ToolDefinition for DbStatsMcpService {
    const FUNC_NAME: &'static str = "db_stats";
    const DESCRIPTION: &'static str = "Read collected statistics for the database described in the server instructions: table sizes and row estimates, the statements consuming the most execution time, connection counts and long-running queries, database-wide throughput counters, and disk I/O (which tables are being read off disk, plus WAL and checkpoint write volume). Useful before writing a query against an unfamiliar schema — it says which tables are large, which have no index usage, and which are bloated. Served from a cache refreshed in the background, so it costs the database nothing: activity is a few seconds old, sizes and statement timings up to a minute. Every section reports its own availability; 'unavailable' with a reason means this server or this account cannot produce it (for example pg_stat_statements not being installed). Note that Postgres exposes no host CPU metric at all — 'busyBackends' and 'execMsPerSec' are execution-time proxies, not CPU percentages.";
}

#[async_trait::async_trait]
impl McpToolCall<DbStatsToolCallRequest, DbStatsToolCallResponse> for DbStatsMcpService {
    async fn execute_tool_call(
        &self,
        model: DbStatsToolCallRequest,
    ) -> Result<DbStatsToolCallResponse, String> {
        let snapshot = self.db.stats.get();
        let stats = DbStatsModel::new(snapshot.as_ref());

        let json = serde_json::to_value(&stats)
            .map_err(|err| format!("Could not render the statistics: {}", err))?;

        let json = narrow_to_section(json, model.section.trim());

        Ok(DbStatsToolCallResponse {
            stats_as_json: json.to_string(),
        })
    }
}

/// Drops the sections the agent did not ask for.
///
/// `server` always survives: it carries the version and the privileges, without
/// which a section that came back empty is indistinguishable from one the account
/// is not allowed to read. Narrowing exists to keep the answer inside the model's
/// context, and dropping the one field that explains the rest would defeat that.
fn narrow_to_section(json: serde_json::Value, section: &str) -> serde_json::Value {
    let keep = match section {
        SECTION_TABLES => "tables",
        SECTION_LOAD => "load",
        SECTION_ACTIVITY => "activity",
        SECTION_HEALTH => "health",
        SECTION_IO => "diskIo",
        SECTION_MINUTES => "throughput",
        _ => return json,
    };

    let serde_json::Value::Object(mut map) = json else {
        return json;
    };

    map.retain(|key, _| {
        matches!(
            key.as_str(),
            "server" | "collectedAt" | "slowCollectedAt" | "lastError"
        ) || key == keep
    });

    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json() -> serde_json::Value {
        serde_json::json!({
            "collectedAt": "2026-08-11T00:00:00+00:00",
            "slowCollectedAt": "2026-08-11T00:00:00+00:00",
            "lastError": null,
            "server": { "state": "ready" },
            "activity": { "state": "ready" },
            "health": { "state": "ready" },
            "load": { "state": "ready" },
            "tables": { "state": "ready" },
            "diskIo": { "state": "ready" },
            "throughput": { "calls": 1 },
        })
    }

    fn keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = value
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.to_string())
            .collect();
        keys.sort();
        keys
    }

    #[test]
    fn a_named_section_keeps_itself_and_the_context_fields() {
        let narrowed = narrow_to_section(sample_json(), "tables");

        assert_eq!(
            keys(&narrowed),
            vec!["collectedAt", "lastError", "server", "slowCollectedAt", "tables"]
        );
    }

    #[test]
    fn an_empty_or_unknown_section_returns_everything() {
        // A client that omits the argument sends "", and a typo must not be an
        // error the agent has to recover from.
        for section in ["", "all", "ALL", "sizes"] {
            let narrowed = narrow_to_section(sample_json(), section);

            assert!(
                narrowed.as_object().unwrap().contains_key("tables"),
                "section '{}' should have returned everything",
                section
            );
            assert!(narrowed.as_object().unwrap().contains_key("load"));
        }
    }

    #[test]
    fn every_documented_section_name_narrows() {
        for (section, expected) in [
            (SECTION_TABLES, "tables"),
            (SECTION_LOAD, "load"),
            (SECTION_ACTIVITY, "activity"),
            (SECTION_HEALTH, "health"),
            (SECTION_IO, "diskIo"),
            (SECTION_MINUTES, "throughput"),
        ] {
            let narrowed = narrow_to_section(sample_json(), section);
            let narrowed = narrowed.as_object().unwrap();

            assert!(narrowed.contains_key(expected));
            // The other three sections are gone.
            assert_eq!(narrowed.len(), 5, "section '{}'", section);
        }
    }
}
