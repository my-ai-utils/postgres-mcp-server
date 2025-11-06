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

    pub async fn do_request(&self, sql: String) -> String {
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
            .unwrap();

        let mut result = my_json5::json_writer::JsonArrayWriter::new();

        for itm in items {
            result = result.write(itm.into_json_value());
        }

        result.build()
    }
}

pub struct SqlResponse {
    result: String,
}

impl SqlResponse {
    pub fn into_json_value(self) -> RawJsonObject<'static> {
        RawJsonObject::AsString(self.result)
    }
}

impl SelectEntity for SqlResponse {
    fn from(row: &my_postgres::tokio_postgres::Row) -> Self {
        let mut result = my_json5::json_writer::JsonObjectWriter::new();
        let mut index = 0;
        for column in row.columns() {
            index += 1;
            let name = column.name();

            let value: Result<i8, my_postgres::tokio_postgres::Error> = row.try_get(index - 1);
            if let Ok(value) = value {
                result = result.write(name, value);
                continue;
            }

            let value: Result<i16, my_postgres::tokio_postgres::Error> = row.try_get(index - 1);
            if let Ok(value) = value {
                result = result.write(name, value);
                continue;
            }

            let value: Result<i32, my_postgres::tokio_postgres::Error> = row.try_get(index - 1);
            if let Ok(value) = value {
                result = result.write(name, value);
                continue;
            }

            let value: Result<i64, my_postgres::tokio_postgres::Error> = row.try_get(index - 1);
            if let Ok(value) = value {
                result = result.write(name, value);
                continue;
            }

            let value: Result<f32, my_postgres::tokio_postgres::Error> = row.try_get(index - 1);
            if let Ok(value) = value {
                result = result.write(name, value);
                continue;
            }

            let value: Result<f64, my_postgres::tokio_postgres::Error> = row.try_get(index - 1);
            if let Ok(value) = value {
                result = result.write(name, value);
                continue;
            }

            let value: Result<bool, my_postgres::tokio_postgres::Error> = row.try_get(index - 1);
            if let Ok(value) = value {
                result = result.write(name, value);
                continue;
            }

            let value: String = row.get(index - 1);

            result = result.write(name, value);
        }

        Self {
            result: result.build(),
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
