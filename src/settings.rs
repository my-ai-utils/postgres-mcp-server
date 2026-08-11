use std::sync::Arc;

use my_postgres::PostgresSettings;
use serde::*;

/// Prefixes the MCP endpoints may not take: the admin API, swagger and the
/// static-files root are served by other middlewares, and a mount that shadowed
/// one of them would be a silently broken UI.
const RESERVED_PREFIXES: [&str; 2] = ["/api", "/swagger"];

#[derive(Debug, Serialize, Deserialize, Clone, my_settings_reader::SettingsModel)]
pub struct SettingsModel {
    /// One entry per MCP endpoint. Each gets its own path, its own Postgres
    /// connection and its own write-access window.
    ///
    /// Required, like every field below it: a settings file that does not
    /// declare it fails to parse and the server does not start. Nothing here
    /// has a defensible implied value — a missing connection string is a typo,
    /// not a request for a default.
    pub databases: Vec<DatabaseSettingsModel>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DatabaseSettingsModel {
    /// HTTP path the MCP endpoint answers on, e.g. `/mcp` or `/mcp-reporting`.
    pub path: String,
    /// Standard `tokio_postgres` connection string.
    pub conn_string: String,
    /// What this database is. Shown in the UI and handed to the agent as this
    /// endpoint's MCP instructions — with several databases mounted it is the
    /// only thing telling the model which one it is talking to, which is why it
    /// is required rather than nice-to-have.
    pub description: String,
}

/// Which Postgres **server** a mount talks to, read off its connection string.
///
/// Several `databases:` entries routinely point at the same server — different
/// databases on one cluster — and the difference matters: `max_connections`, the
/// backend count and the host's load are properties of the *server*, so grouping
/// by it is what makes those numbers add up instead of being repeated per mount.
/// Whether two mounts share a server is not something the operator states
/// anywhere; it is derivable, so it is derived.
///
/// **Only host, port and the SSH target are kept — never the credentials.** The
/// admin API is unauthenticated, and this travels to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEndpoint {
    /// Lowercased: `host=LOCALHOST` and `host=localhost` are one server.
    pub host: String,
    pub port: u16,
    /// The `ssh=user@host:port` target, when the connection is tunnelled.
    ///
    /// Part of the identity, not decoration: two mounts can both say
    /// `host=localhost` and still be different servers because they are tunnelled
    /// somewhere else. Ignoring it would merge unrelated clusters into one card.
    pub ssh: Option<String>,
    /// The database on that server.
    ///
    /// Not part of [`Self::key`] — the server is the grouping — but the thing that
    /// makes [`Self::database_key`] able to tell "two mounts, two databases" from
    /// "two mounts, one database". Every per-database metric this server collects
    /// (`pg_stat_database`, table sizes) belongs to the *database*, so two mounts
    /// onto one database read the very same counters, and anything that treats them
    /// as independent will count that database twice.
    pub dbname: Option<String>,
}

/// Postgres' own default, applied when the connection string omits `port`.
const DEFAULT_PORT: u16 = 5432;

/// What a connection string with no readable host is labelled.
const UNKNOWN_HOST: &str = "unknown";

impl ServerEndpoint {
    /// Reads the endpoint out of a `tokio_postgres` connection string.
    ///
    /// **Both accepted forms are handled**, because the driver accepts both and
    /// getting this wrong is not a cosmetic failure: a URI would leave every mount
    /// with the same placeholder host, and every configured database would collapse
    /// into one bogus server in the UI.
    ///
    /// - keyword/value — `host=db.internal port=5432 dbname=crm user=u password=p`
    /// - URI — `postgres://u:p@db.internal:5432/crm?sslmode=require`
    ///
    /// Deliberately tolerant otherwise: a string it cannot read yields a
    /// placeholder rather than an error. It runs on a string the driver has already
    /// accepted and is used only to *group* and *label*, so refusing to boot over it
    /// would be a worse outcome than one card labelled `unknown`.
    pub fn parse(conn_string: &str) -> Self {
        let trimmed = conn_string.trim();

        let lower = trimmed.to_lowercase();

        if lower.starts_with("postgres://") || lower.starts_with("postgresql://") {
            return Self::parse_uri(trimmed);
        }

        Self::parse_keyword_value(trimmed)
    }

    fn parse_keyword_value(conn_string: &str) -> Self {
        let mut host = None;
        let mut port = None;
        let mut ssh = None;
        let mut dbname = None;

        for token in conn_string.split_whitespace() {
            let Some((key, value)) = token.split_once('=') else {
                continue;
            };

            let value = value.trim().trim_matches('\'').trim_matches('"');

            if value.is_empty() {
                continue;
            }

            match key.trim().to_lowercase().as_str() {
                // `hostaddr` is deliberately not consulted: libpq uses it to skip
                // DNS for a `host` that is still the server's name, so preferring
                // whichever came first in the string would give the same server two
                // identities depending on how the operator wrote it.
                "host" if host.is_none() => host = Some(value.to_lowercase()),
                "port" => port = value.parse::<u16>().ok(),
                "ssh" => ssh = Some(value.to_string()),
                "dbname" => dbname = Some(value.to_string()),
                _ => {}
            }
        }

        Self {
            host: host.unwrap_or_else(|| UNKNOWN_HOST.to_string()),
            port: port.unwrap_or(DEFAULT_PORT),
            ssh,
            dbname,
        }
    }

    /// `postgres://[user[:password]@]host[:port][/dbname][?params]`
    ///
    /// The password may itself contain `@`, so the authority is split at the **last**
    /// `@` before the path — splitting at the first would hand back a fragment of the
    /// credentials as the hostname.
    fn parse_uri(conn_string: &str) -> Self {
        let after_scheme = conn_string
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or("");

        // Query parameters can contain '/' and '@', so they come off first.
        let (before_query, query) = match after_scheme.split_once('?') {
            Some((before, query)) => (before, Some(query)),
            None => (after_scheme, None),
        };

        let (authority, dbname) = match before_query.split_once('/') {
            Some((authority, database)) => (authority, Some(database)),
            None => (before_query, None),
        };

        let authority = match authority.rsplit_once('@') {
            Some((_credentials, host)) => host,
            None => authority,
        };

        // A multi-host URI ("host1:5432,host2:5432") is failover, not one server;
        // the first is the one this identity describes, and the rest are alternates.
        let authority = authority.split(',').next().unwrap_or(authority);

        // A bracketed IPv6 literal keeps its colons: [::1]:5432.
        let (host, port) = match authority.strip_prefix('[') {
            Some(rest) => match rest.split_once(']') {
                Some((host, tail)) => (host, tail.strip_prefix(':')),
                None => (rest, None),
            },
            None => match authority.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            },
        };

        // `ssh=` has no place in a URI, but a URI can still carry it as a query
        // parameter, and the tunnel is part of the identity wherever it is written.
        let ssh = query.and_then(|query| {
            query.split('&').find_map(|pair| {
                pair.split_once('=')
                    .filter(|(key, _)| key.eq_ignore_ascii_case("ssh"))
                    .map(|(_, value)| value.to_string())
            })
        });

        Self {
            host: if host.is_empty() {
                UNKNOWN_HOST.to_string()
            } else {
                host.to_lowercase()
            },
            port: port.and_then(|port| port.parse::<u16>().ok()).unwrap_or(DEFAULT_PORT),
            ssh,
            dbname: dbname.filter(|db| !db.is_empty()).map(|db| db.to_string()),
        }
    }

    /// Stable id for this server, used as the grouping key and by the UI to
    /// remember which server was selected. The database is **not** part of it — that
    /// is the whole point of grouping.
    pub fn key(&self) -> String {
        match &self.ssh {
            Some(ssh) => format!("ssh://{}/{}:{}", ssh, self.host, self.port),
            None => format!("{}:{}", self.host, self.port),
        }
    }

    /// Identity of the **database**: server plus database name.
    ///
    /// `None` when the connection string does not name a database, which is legal —
    /// libpq then falls back to the user name — and in that case two such mounts
    /// cannot be proven to be the same database from configuration alone, so callers
    /// must treat them as distinct rather than guessing.
    pub fn database_key(&self) -> Option<String> {
        let dbname = self.dbname.as_ref()?;

        Some(format!("{}/{}", self.key(), dbname))
    }

    /// What the operator sees. The default port is left off — `db.internal` reads
    /// better than `db.internal:5432` and carries the same information.
    pub fn label(&self) -> String {
        let endpoint = if self.port == DEFAULT_PORT {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        };

        match &self.ssh {
            Some(ssh) => format!("{} (via ssh {})", endpoint, ssh),
            None => endpoint,
        }
    }
}

/// One validated `path -> database` mount.
///
/// Built once at startup. The path is normalized here so that everything
/// downstream — the middleware mount, the request log, the HTTP api and the UI
/// — compares and displays the exact same string.
#[derive(Debug, Clone)]
pub struct DatabaseMount {
    pub path: String,
    pub conn_string: String,
    pub description: String,
}

impl DatabaseMount {
    pub fn server(&self) -> ServerEndpoint {
        ServerEndpoint::parse(self.conn_string.as_str())
    }
}

impl SettingsModel {
    /// Resolves the settings file into the list of databases to mount, in the
    /// order it declares them.
    ///
    /// Every failure here is a misconfiguration the operator has to fix, so it
    /// comes back as an error and stops the boot rather than mounting a subset
    /// — a server that came up with one of three databases missing would look
    /// healthy right up to the first query against it.
    pub fn get_mounts(&self) -> Result<Vec<DatabaseMount>, String> {
        if self.databases.is_empty() {
            return Err("'databases' is empty — declare at least one { path, conn_string, \
                        description }."
                .to_string());
        }

        let mut result = Vec::with_capacity(self.databases.len());

        for db in &self.databases {
            result.push(DatabaseMount::new(
                db.path.as_str(),
                db.conn_string.as_str(),
                db.description.as_str(),
            )?);
        }

        // Two mounts on one path would hand the first middleware in the chain
        // every request and leave the second one dead — and since paths are
        // matched case-insensitively, "/Mcp" and "/mcp" are the same mount.
        for (index, mount) in result.iter().enumerate() {
            if result[..index]
                .iter()
                .any(|earlier| paths_are_equal(earlier.path.as_str(), mount.path.as_str()))
            {
                return Err(format!(
                    "Path '{}' is configured more than once. Each database needs its own path.",
                    mount.path
                ));
            }
        }

        Ok(result)
    }

    /// The connection string currently configured for `path`, read from the
    /// live settings. `None` when that path is no longer in the file.
    fn find_conn_string(&self, path: &str) -> Option<String> {
        self.databases
            .iter()
            .find(|db| paths_are_equal(normalize_path(db.path.as_str()).as_str(), path))
            .map(|db| db.conn_string.trim().to_string())
    }
}

impl DatabaseMount {
    fn new(path: &str, conn_string: &str, description: &str) -> Result<Self, String> {
        let path = normalize_path(path);
        let conn_string = conn_string.trim().to_string();
        let description = description.trim().to_string();

        if path.is_empty() || path == "/" {
            return Err(
                "A database path must be something like '/mcp' — '/' is taken by the UI."
                    .to_string(),
            );
        }

        let lower_path = path.to_lowercase();

        if let Some(reserved) = RESERVED_PREFIXES.iter().find(|reserved| {
            lower_path == **reserved || lower_path.starts_with(&format!("{}/", reserved))
        }) {
            return Err(format!(
                "Path '{}' is reserved by the server ('{}' and everything under it).",
                path, reserved
            ));
        }

        if conn_string.is_empty() {
            return Err(format!("Database '{}' has an empty conn_string.", path));
        }

        // `my-postgres` parses the connection string itself and only understands the
        // keyword/value form; handed a URI it panics on a bare `Option::unwrap()`
        // deep inside the driver, before the port is even bound. Caught here so the
        // operator gets an error naming the entry and the fix, rather than a
        // backtrace pointing into a dependency.
        let lower = conn_string.to_lowercase();

        if lower.starts_with("postgres://") || lower.starts_with("postgresql://") {
            return Err(format!(
                "Database '{}' uses the URI connection-string form, which this server's Postgres \
                 driver does not support — it panics on it at start-up. Write it as keywords \
                 instead: host=… port=… dbname=… user=… password=…",
                path
            ));
        }

        if description.is_empty() {
            return Err(format!(
                "Database '{}' has an empty description. It is what tells the agent which \
                 database this endpoint is bound to.",
                path
            ));
        }

        Ok(Self {
            path,
            conn_string,
            description,
        })
    }
}

/// `mcp` / `/mcp/` / ` /mcp ` all mean `/mcp`.
pub fn normalize_path(path: &str) -> String {
    let path = path.trim().trim_end_matches('/');

    if path.starts_with('/') {
        return path.to_string();
    }

    format!("/{}", path)
}

/// Mount paths are compared the way the MCP middleware routes them —
/// case-insensitively.
pub fn paths_are_equal(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

/// [`PostgresSettings`] for a single mount.
///
/// The connection string is looked up in the *live* settings by path on every
/// (re)connect, so editing `~/.postgres-mcp-server` is picked up without a
/// restart. A path that has since disappeared from the file falls back to what
/// it was at startup — handing the driver an empty string instead would turn a
/// typo in the settings file into a stream of unexplained connection errors.
pub struct DbConnectionSettings {
    settings: Arc<SettingsReader>,
    path: String,
    conn_string_at_startup: String,
}

impl DbConnectionSettings {
    pub fn new(settings: Arc<SettingsReader>, mount: &DatabaseMount) -> Self {
        Self {
            settings,
            path: mount.path.clone(),
            conn_string_at_startup: mount.conn_string.clone(),
        }
    }
}

#[async_trait::async_trait]
impl PostgresSettings for DbConnectionSettings {
    async fn get_connection_string(&self) -> String {
        let from_settings = self
            .settings
            .use_settings(|settings| settings.find_conn_string(self.path.as_str()))
            .await;

        from_settings
            .filter(|itm| !itm.is_empty())
            .unwrap_or_else(|| self.conn_string_at_startup.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(path: &str, conn_string: &str) -> DatabaseSettingsModel {
        DatabaseSettingsModel {
            path: path.to_string(),
            conn_string: conn_string.to_string(),
            description: "Test database".to_string(),
        }
    }

    #[test]
    fn mounts_keep_the_declared_order_and_normalize_paths() {
        let settings = SettingsModel {
            databases: vec![db("/first", "host=first"), db("second", "host=second")],
        };

        let mounts = settings.get_mounts().unwrap();

        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].path, "/first");
        // A path without the leading slash is normalized rather than refused.
        assert_eq!(mounts[1].path, "/second");
    }

    #[test]
    fn an_empty_databases_list_is_an_error() {
        let settings = SettingsModel {
            databases: Vec::new(),
        };

        assert!(settings.get_mounts().is_err());
    }

    #[test]
    fn duplicate_paths_are_refused_case_insensitively() {
        let settings = SettingsModel {
            databases: vec![db("/mcp", "host=a"), db("/MCP/", "host=b")],
        };

        assert!(settings.get_mounts().is_err());
    }

    #[test]
    fn reserved_paths_are_refused() {
        for path in ["/", "/api", "/api/Settings", "/swagger"] {
            let settings = SettingsModel {
                databases: vec![db(path, "host=a")],
            };

            assert!(
                settings.get_mounts().is_err(),
                "path '{}' should be refused",
                path
            );
        }
    }

    #[test]
    fn an_empty_conn_string_is_refused() {
        let settings = SettingsModel {
            databases: vec![db("/mcp", "   ")],
        };

        assert!(settings.get_mounts().is_err());
    }

    #[test]
    fn an_empty_description_is_refused() {
        let settings = SettingsModel {
            databases: vec![DatabaseSettingsModel {
                path: "/mcp".to_string(),
                conn_string: "host=a".to_string(),
                description: "  ".to_string(),
            }],
        };

        assert!(settings.get_mounts().is_err());
    }

    #[test]
    fn a_settings_file_without_databases_does_not_parse() {
        // No `#[serde(default)]` anywhere in the model: a missing key is a
        // misconfiguration and has to stop the boot, not be filled in silently.
        let err = my_settings_reader::serde_yaml::from_str::<SettingsModel>("postgres_conn_string: host=legacy\n");
        assert!(err.is_err());

        // ...and neither does an entry that forgot one of the three fields.
        let err = my_settings_reader::serde_yaml::from_str::<SettingsModel>(
            "databases:\n- path: /mcp\n  conn_string: host=a\n",
        );
        assert!(err.is_err());
    }

    #[test]
    fn two_databases_on_one_cluster_share_a_server_key() {
        let crm = ServerEndpoint::parse("host=db.internal port=5432 user=u password=p dbname=crm");
        let billing =
            ServerEndpoint::parse("host=db.internal user=u password=p dbname=billing");

        // The second omits the port, which defaults to the first's.
        assert_eq!(crm.key(), billing.key());
        assert_eq!(crm.label(), "db.internal");
    }

    #[test]
    fn a_different_host_or_port_is_a_different_server() {
        let a = ServerEndpoint::parse("host=db-a user=u password=p dbname=d");
        let b = ServerEndpoint::parse("host=db-b user=u password=p dbname=d");
        let c = ServerEndpoint::parse("host=db-a port=5433 user=u password=p dbname=d");

        assert_ne!(a.key(), b.key());
        assert_ne!(a.key(), c.key());
        assert_eq!(c.label(), "db-a:5433");
    }

    #[test]
    fn host_case_does_not_split_a_server() {
        assert_eq!(
            ServerEndpoint::parse("host=DB.Internal dbname=d").key(),
            ServerEndpoint::parse("host=db.internal dbname=d").key()
        );
    }

    #[test]
    fn the_ssh_tunnel_is_part_of_the_identity() {
        // Both say localhost, but they are tunnelled to different machines — merging
        // them would put two unrelated clusters on one card.
        let prod = ServerEndpoint::parse("ssh=deploy@prod:22 host=localhost dbname=d");
        let stage = ServerEndpoint::parse("ssh=deploy@stage:22 host=localhost dbname=d");
        let direct = ServerEndpoint::parse("host=localhost dbname=d");

        assert_ne!(prod.key(), stage.key());
        assert_ne!(prod.key(), direct.key());
        assert!(prod.label().contains("via ssh deploy@prod:22"));
    }

    #[test]
    fn the_endpoint_never_carries_credentials() {
        // This travels to an unauthenticated admin API.
        let endpoint = ServerEndpoint::parse("host=db.internal user=admin password=s3cret dbname=d");

        let rendered = format!("{} {} {:?}", endpoint.key(), endpoint.label(), endpoint);

        assert!(!rendered.contains("s3cret"));
        assert!(!rendered.contains("admin"));
    }

    #[test]
    fn a_uri_connection_string_is_refused_with_an_actionable_message() {
        // Left to reach the driver it panics on Option::unwrap() before the port is
        // bound — a backtrace into a dependency instead of "entry X is wrong".
        let settings = SettingsModel {
            databases: vec![db("/mcp", "postgres://u:p@db.internal:5432/crm")],
        };

        let err = settings.get_mounts().unwrap_err();

        assert!(err.contains("/mcp"), "must name the entry: {}", err);
        assert!(err.contains("host="), "must say what to write instead: {}", err);
    }

    #[test]
    fn the_uri_form_is_read_like_the_keyword_form() {
        // The driver accepts both. Reading only one would leave every URI mount with
        // the placeholder host — collapsing every configured database into a single
        // bogus server.
        let uri = ServerEndpoint::parse("postgres://u:p@db.internal:6432/crm");
        let kv = ServerEndpoint::parse("host=db.internal port=6432 user=u password=p dbname=crm");

        assert_eq!(uri.key(), kv.key());
        assert_eq!(uri.dbname.as_deref(), Some("crm"));
        assert_eq!(uri.database_key(), kv.database_key());
    }

    #[test]
    fn the_postgresql_scheme_is_the_same_as_postgres() {
        assert_eq!(
            ServerEndpoint::parse("postgresql://h/db").key(),
            ServerEndpoint::parse("postgres://h/db").key()
        );
    }

    #[test]
    fn a_uri_defaults_the_port_and_survives_a_missing_database() {
        let endpoint = ServerEndpoint::parse("postgres://db.internal");

        assert_eq!(endpoint.host, "db.internal");
        assert_eq!(endpoint.port, 5432);
        assert_eq!(endpoint.dbname, None);
        // Without a database name two mounts cannot be *proven* to be one database.
        assert_eq!(endpoint.database_key(), None);
    }

    #[test]
    fn a_password_containing_an_at_sign_does_not_become_the_host() {
        // Splitting at the first '@' would return "pass@db.internal" -> host "pass".
        let endpoint = ServerEndpoint::parse("postgres://user:p@ss@db.internal:5432/crm");

        assert_eq!(endpoint.host, "db.internal");
        assert_eq!(endpoint.port, 5432);
        assert_eq!(endpoint.dbname.as_deref(), Some("crm"));
    }

    #[test]
    fn uri_query_parameters_do_not_leak_into_the_database_name() {
        let endpoint = ServerEndpoint::parse("postgres://u:p@h:5432/crm?sslmode=require&ssh=d@b:22");

        assert_eq!(endpoint.dbname.as_deref(), Some("crm"));
        assert_eq!(endpoint.ssh.as_deref(), Some("d@b:22"));
        assert!(endpoint.key().starts_with("ssh://d@b:22/"));
    }

    #[test]
    fn a_bracketed_ipv6_host_keeps_its_colons() {
        let endpoint = ServerEndpoint::parse("postgres://u@[::1]:5433/crm");

        assert_eq!(endpoint.host, "::1");
        assert_eq!(endpoint.port, 5433);
    }

    #[test]
    fn a_multi_host_uri_identifies_the_first_host() {
        // Failover alternates are not a second server; the identity describes where
        // this mount is pointed first.
        let endpoint = ServerEndpoint::parse("postgres://u@primary:5432,standby:5432/crm");

        assert_eq!(endpoint.host, "primary");
    }

    #[test]
    fn two_mounts_on_one_database_share_a_database_key_but_two_databases_do_not() {
        // This is the pair that decides whether a per-database metric gets counted
        // once or twice.
        let read = ServerEndpoint::parse("host=db.internal dbname=crm user=readonly password=p");
        let write = ServerEndpoint::parse("host=db.internal dbname=crm user=writer password=p");
        let other = ServerEndpoint::parse("host=db.internal dbname=billing user=u password=p");

        assert_eq!(read.database_key(), write.database_key());
        assert_ne!(read.database_key(), other.database_key());
        // ...while all three still group under one server.
        assert_eq!(read.key(), other.key());
    }

    #[test]
    fn hostaddr_does_not_become_a_second_identity_for_one_server() {
        // libpq uses hostaddr to skip DNS while `host` is still the server's name.
        // Treating it as the identity would split one server in two, or merge two.
        let with_addr =
            ServerEndpoint::parse("host=db.internal hostaddr=10.0.0.7 dbname=crm user=u");
        let without = ServerEndpoint::parse("host=db.internal dbname=crm user=u");

        assert_eq!(with_addr.key(), without.key());
    }

    #[test]
    fn a_hostless_string_is_labelled_rather_than_refused() {
        // The driver validated it already; this is only used to group and label, so
        // failing the boot here would be worse than one card saying "unknown".
        let endpoint = ServerEndpoint::parse("dbname=d user=u password=p");

        assert_eq!(endpoint.host, "unknown");
        assert_eq!(endpoint.port, 5432);
    }

    #[test]
    fn quoted_and_padded_values_are_read() {
        let endpoint = ServerEndpoint::parse("host='db.internal'  port=\"6432\" dbname=d");

        assert_eq!(endpoint.host, "db.internal");
        assert_eq!(endpoint.port, 6432);
    }

    #[test]
    fn conn_string_lookup_follows_the_normalized_path() {
        let settings = SettingsModel {
            databases: vec![db("reporting/", "host=reporting")],
        };

        assert_eq!(
            settings.find_conn_string("/reporting"),
            Some("host=reporting".to_string())
        );
        assert_eq!(settings.find_conn_string("/gone"), None);
    }
}
