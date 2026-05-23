use serde_json::{json, Value};
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

pub(super) fn decode_row_value_fallback(row: &tokio_postgres::Row, idx: usize, ty: &Type) -> Value {
    match *ty {
        Type::BOOL => row
            .try_get::<_, Option<bool>>(idx)
            .ok()
            .flatten()
            .map(Value::Bool)
            .unwrap_or(Value::Null),
        Type::INT2 => row
            .try_get::<_, Option<i16>>(idx)
            .ok()
            .flatten()
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        Type::INT4 => row
            .try_get::<_, Option<i32>>(idx)
            .ok()
            .flatten()
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        Type::INT8 => row
            .try_get::<_, Option<i64>>(idx)
            .ok()
            .flatten()
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        Type::FLOAT4 => row
            .try_get::<_, Option<f32>>(idx)
            .ok()
            .flatten()
            .and_then(|v| serde_json::Number::from_f64(v as f64).map(Value::Number))
            .unwrap_or(Value::Null),
        Type::FLOAT8 => row
            .try_get::<_, Option<f64>>(idx)
            .ok()
            .flatten()
            .and_then(|v| serde_json::Number::from_f64(v).map(Value::Number))
            .unwrap_or(Value::Null),
        Type::JSON | Type::JSONB => row
            .try_get::<_, Option<Json<Value>>>(idx)
            .ok()
            .flatten()
            .map(|v| v.0)
            .unwrap_or(Value::Null),
        _ => {
            if let Ok(Some(s)) = row.try_get::<_, Option<String>>(idx) {
                return Value::String(s);
            }
            if let Ok(Some(v)) = row.try_get::<_, Option<i64>>(idx) {
                return json!(v);
            }
            if let Ok(Some(v)) = row.try_get::<_, Option<f64>>(idx) {
                if let Some(n) = serde_json::Number::from_f64(v) {
                    return Value::Number(n);
                }
            }
            Value::String(format!("<unhandled_type:{}>", ty.name()))
        }
    }
}
