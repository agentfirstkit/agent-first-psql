use super::errors::ExecError;
use serde_json::{Value, json};
use tokio_postgres::types::{Json, Type};

pub(super) fn fallback_columns_supported(stmt: &tokio_postgres::Statement) -> bool {
    stmt.columns()
        .iter()
        .all(|column| fallback_type_supported(column.type_()))
}

fn fallback_type_supported(ty: &Type) -> bool {
    matches!(
        *ty,
        Type::BOOL
            | Type::INT2
            | Type::INT4
            | Type::INT8
            | Type::FLOAT4
            | Type::FLOAT8
            | Type::JSON
            | Type::JSONB
            | Type::BYTEA
            | Type::TEXT
            | Type::VARCHAR
            | Type::BPCHAR
            | Type::NAME
            | Type::TEXT_ARRAY
            | Type::VARCHAR_ARRAY
            | Type::NAME_ARRAY
            | Type::INT2_ARRAY
            | Type::INT4_ARRAY
            | Type::INT8_ARRAY
    )
}

pub(super) fn row_to_json_fallback(row: &tokio_postgres::Row) -> Result<Value, ExecError> {
    let mut map = serde_json::Map::new();
    for (idx, column) in row.columns().iter().enumerate() {
        map.insert(
            column.name().to_string(),
            decode_row_value(row, idx, column.type_())?,
        );
    }
    Ok(Value::Object(map))
}

fn decode_typed<T, F>(
    row: &tokio_postgres::Row,
    idx: usize,
    ty: &Type,
    map: F,
) -> Result<Value, ExecError>
where
    T: for<'a> tokio_postgres::types::FromSql<'a>,
    F: FnOnce(T) -> Value,
{
    match row.try_get::<_, Option<T>>(idx) {
        Ok(None) => Ok(Value::Null),
        Ok(Some(value)) => Ok(map(value)),
        Err(error) => Err(ExecError::Internal(format!(
            "failed to decode fallback type {}: {error}",
            ty.name()
        ))),
    }
}

fn decode_row_value(row: &tokio_postgres::Row, idx: usize, ty: &Type) -> Result<Value, ExecError> {
    match *ty {
        Type::BOOL => decode_typed::<bool, _>(row, idx, ty, Value::Bool),
        Type::INT2 => decode_typed::<i16, _>(row, idx, ty, |value| json!(value)),
        Type::INT4 => decode_typed::<i32, _>(row, idx, ty, |value| json!(value)),
        Type::INT8 => decode_typed::<i64, _>(row, idx, ty, |value| json!(value)),
        Type::FLOAT4 => decode_typed::<f32, _>(row, idx, ty, |value| {
            serde_json::Number::from_f64(value as f64)
                .map(Value::Number)
                .unwrap_or(Value::Null)
        }),
        Type::FLOAT8 => decode_typed::<f64, _>(row, idx, ty, |value| {
            serde_json::Number::from_f64(value)
                .map(Value::Number)
                .unwrap_or(Value::Null)
        }),
        Type::JSON | Type::JSONB => decode_typed::<Json<Value>, _>(row, idx, ty, |value| value.0),
        Type::BYTEA => decode_typed::<Vec<u8>, _>(row, idx, ty, |bytes| {
            let mut value = String::with_capacity(2 + bytes.len() * 2);
            value.push_str("\\x");
            for byte in bytes {
                value.push_str(&format!("{byte:02x}"));
            }
            Value::String(value)
        }),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => {
            decode_typed::<String, _>(row, idx, ty, Value::String)
        }
        Type::TEXT_ARRAY | Type::VARCHAR_ARRAY | Type::NAME_ARRAY => {
            decode_typed::<Vec<Option<String>>, _>(row, idx, ty, optional_array)
        }
        Type::INT2_ARRAY => decode_typed::<Vec<Option<i16>>, _>(row, idx, ty, optional_array),
        Type::INT4_ARRAY => decode_typed::<Vec<Option<i32>>, _>(row, idx, ty, optional_array),
        Type::INT8_ARRAY => decode_typed::<Vec<Option<i64>>, _>(row, idx, ty, optional_array),
        _ => Err(ExecError::Internal(format!(
            "fallback row decoder does not support PostgreSQL type {}",
            ty.name()
        ))),
    }
}

fn optional_array<T: serde::Serialize>(items: Vec<Option<T>>) -> Value {
    json!(items)
}
