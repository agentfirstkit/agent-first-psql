use serde_json::{Value, json};
use tokio_postgres::types::{Json, Type};

pub(super) fn row_json_size(row: &Value) -> usize {
    serde_json::to_vec(row).map(|b| b.len()).unwrap_or(0)
}

pub(super) fn row_to_json_fallback(row: &tokio_postgres::Row) -> Value {
    let mut map = serde_json::Map::new();
    for (idx, col) in row.columns().iter().enumerate() {
        let value = decode_row_value_fallback(row, idx, col.type_());
        map.insert(col.name().to_string(), value);
    }
    Value::Object(map)
}

/// Marker emitted when a decoder for a known type fails at runtime (e.g.,
/// unexpected binary encoding). Distinct from `<unhandled_type:T>` so an
/// agent can tell "decoder broke" from "type lacks a decoder."
fn decode_error(ty: &Type) -> Value {
    Value::String(format!("<decode_error:{}>", ty.name()))
}

/// Decode a known typed column or return `<decode_error:T>` on failure.
/// `null` only flows through when PG actually reports the column NULL.
fn decode_typed<T, F>(row: &tokio_postgres::Row, idx: usize, ty: &Type, map: F) -> Value
where
    T: for<'a> tokio_postgres::types::FromSql<'a>,
    F: FnOnce(T) -> Value,
{
    match row.try_get::<_, Option<T>>(idx) {
        Ok(None) => Value::Null,
        Ok(Some(v)) => map(v),
        Err(_) => decode_error(ty),
    }
}

pub(super) fn decode_row_value_fallback(row: &tokio_postgres::Row, idx: usize, ty: &Type) -> Value {
    match *ty {
        Type::BOOL => decode_typed::<bool, _>(row, idx, ty, Value::Bool),
        Type::INT2 => decode_typed::<i16, _>(row, idx, ty, |v| json!(v)),
        Type::INT4 => decode_typed::<i32, _>(row, idx, ty, |v| json!(v)),
        Type::INT8 => decode_typed::<i64, _>(row, idx, ty, |v| json!(v)),
        Type::FLOAT4 => decode_typed::<f32, _>(row, idx, ty, |v| {
            serde_json::Number::from_f64(v as f64)
                .map(Value::Number)
                .unwrap_or(Value::Null)
        }),
        Type::FLOAT8 => decode_typed::<f64, _>(row, idx, ty, |v| {
            serde_json::Number::from_f64(v)
                .map(Value::Number)
                .unwrap_or(Value::Null)
        }),
        Type::JSON | Type::JSONB => decode_typed::<Json<Value>, _>(row, idx, ty, |v| v.0),
        Type::BYTEA => decode_typed::<Vec<u8>, _>(row, idx, ty, |bytes| {
            // Encode as the standard PostgreSQL `\\x` hex string so a round
            // trip through psql or another client preserves the bytes.
            let mut s = String::with_capacity(2 + bytes.len() * 2);
            s.push_str("\\x");
            for b in bytes {
                s.push_str(&format!("{b:02x}"));
            }
            Value::String(s)
        }),
        Type::TEXT_ARRAY | Type::VARCHAR_ARRAY | Type::NAME_ARRAY => {
            decode_typed::<Vec<Option<String>>, _>(row, idx, ty, |items| {
                Value::Array(
                    items
                        .into_iter()
                        .map(|v| match v {
                            Some(s) => Value::String(s),
                            None => Value::Null,
                        })
                        .collect(),
                )
            })
        }
        Type::INT2_ARRAY => decode_typed::<Vec<Option<i16>>, _>(row, idx, ty, |items| {
            Value::Array(
                items
                    .into_iter()
                    .map(|v| match v {
                        Some(n) => json!(n),
                        None => Value::Null,
                    })
                    .collect(),
            )
        }),
        Type::INT4_ARRAY => decode_typed::<Vec<Option<i32>>, _>(row, idx, ty, |items| {
            Value::Array(
                items
                    .into_iter()
                    .map(|v| match v {
                        Some(n) => json!(n),
                        None => Value::Null,
                    })
                    .collect(),
            )
        }),
        Type::INT8_ARRAY => decode_typed::<Vec<Option<i64>>, _>(row, idx, ty, |items| {
            Value::Array(
                items
                    .into_iter()
                    .map(|v| match v {
                        Some(n) => json!(n),
                        None => Value::Null,
                    })
                    .collect(),
            )
        }),
        _ => {
            if let Ok(Some(s)) = row.try_get::<_, Option<String>>(idx) {
                return Value::String(s);
            }
            if let Ok(Some(v)) = row.try_get::<_, Option<i64>>(idx) {
                return json!(v);
            }
            if let Ok(Some(v)) = row.try_get::<_, Option<f64>>(idx)
                && let Some(n) = serde_json::Number::from_f64(v)
            {
                return Value::Number(n);
            }
            Value::String(format!("<unhandled_type:{}>", ty.name()))
        }
    }
}
