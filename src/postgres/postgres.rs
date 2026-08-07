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
