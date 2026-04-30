use std::{sync::Arc, time::Duration};

use my_json5::json_writer::RawJsonObject;
use my_postgres::{
    MyPostgres, RequestContext,
    sql::{SqlData, SqlValues},
    sql_select::SelectEntity,
};
use rust_extensions::date_time::DateTimeAsMicroseconds;

pub struct PostgresAccess {
    postgres: MyPostgres,
}

impl PostgresAccess {
    pub async fn new(settings: Arc<crate::settings::SettingsReader>) -> Self {
        Self {
            postgres: MyPostgres::from_settings(crate::app::APP_NAME, settings)
                .build()
                .await,
        }
    }

    pub async fn do_request(&self, sql: String) -> Result<String, String> {
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

        let mut rows_arr = my_json5::json_writer::JsonArrayWriter::new();
        for itm in items {
            rows_arr = rows_arr.write(RawJsonObject::AsString(itm.values_json));
        }

        let result = my_json5::json_writer::JsonObjectWriter::new()
            .write("columns", RawJsonObject::AsString(columns_arr.build()))
            .write("rows", RawJsonObject::AsString(rows_arr.build()));

        Ok(result.build())
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

            if let Ok(v) = row.try_get::<_, Option<i8>>(index) {
                match v {
                    Some(v) => values = values.write(v),
                    None => values = values.write_null_element(),
                }
                continue;
            }
            if let Ok(v) = row.try_get::<_, Option<i16>>(index) {
                match v {
                    Some(v) => values = values.write(v),
                    None => values = values.write_null_element(),
                }
                continue;
            }
            if let Ok(v) = row.try_get::<_, Option<i32>>(index) {
                match v {
                    Some(v) => values = values.write(v),
                    None => values = values.write_null_element(),
                }
                continue;
            }
            if let Ok(v) = row.try_get::<_, Option<i64>>(index) {
                match v {
                    Some(v) => values = values.write(v),
                    None => values = values.write_null_element(),
                }
                continue;
            }
            if let Ok(v) = row.try_get::<_, Option<f32>>(index) {
                match v {
                    Some(v) => values = values.write(v),
                    None => values = values.write_null_element(),
                }
                continue;
            }
            if let Ok(v) = row.try_get::<_, Option<f64>>(index) {
                match v {
                    Some(v) => values = values.write(v),
                    None => values = values.write_null_element(),
                }
                continue;
            }
            if let Ok(v) = row.try_get::<_, Option<bool>>(index) {
                match v {
                    Some(v) => values = values.write(v),
                    None => values = values.write_null_element(),
                }
                continue;
            }

            match row.try_get::<_, Option<String>>(index) {
                Ok(Some(v)) => values = values.write(v),
                Ok(None) => values = values.write_null_element(),
                Err(_) => values = values.write_null_element(),
            }
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
