use super::errors::ExecError;
use serde_json::Value;
use tokio_postgres::types::{Json, ToSql, Type};

pub(super) enum QueryParam {
    Null(AnyNull),
    Bool(bool),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    Float32(f32),
    Float(f64),
    Text(String),
    Json(Json<Value>),
}

#[derive(Debug)]
pub(super) struct AnyNull;

impl ToSql for AnyNull {
    fn to_sql(
        &self,
        _ty: &Type,
        _out: &mut bytes::BytesMut,
    ) -> Result<tokio_postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
        Ok(tokio_postgres::types::IsNull::Yes)
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }

    tokio_postgres::types::to_sql_checked!();
}

pub(super) fn build_params(
    values: &[Value],
    expected_types: &[Type],
) -> Result<Vec<QueryParam>, ExecError> {
    let mut params = Vec::with_capacity(values.len());
    for (idx, v) in values.iter().enumerate() {
        let ty = expected_types.get(idx).unwrap_or(&Type::TEXT);
        let p = match v {
            Value::Null => QueryParam::Null(AnyNull),
            Value::Array(_) | Value::Object(_) if *ty == Type::JSON || *ty == Type::JSONB => {
                QueryParam::Json(Json(v.clone()))
            }
            _ if *ty == Type::BOOL => QueryParam::Bool(parse_bool(v, idx + 1)?),
            _ if *ty == Type::INT2 => QueryParam::Int16(parse_i16(v, idx + 1)?),
            _ if *ty == Type::INT4 => QueryParam::Int32(parse_i32(v, idx + 1)?),
            _ if *ty == Type::INT8 => QueryParam::Int64(parse_i64(v, idx + 1)?),
            _ if *ty == Type::FLOAT4 => QueryParam::Float32(parse_f32(v, idx + 1)?),
            _ if *ty == Type::FLOAT8 => QueryParam::Float(parse_f64(v, idx + 1)?),
            _ if *ty == Type::NUMERIC => QueryParam::Float(parse_f64(v, idx + 1)?),
            _ if *ty == Type::JSON || *ty == Type::JSONB => QueryParam::Json(Json(v.clone())),
            _ => QueryParam::Text(parse_text(v)),
        };
        params.push(p);
    }
    Ok(params)
}

pub(super) fn build_param_refs(params: &[QueryParam]) -> Vec<&(dyn ToSql + Sync)> {
    params
        .iter()
        .map(|p| match p {
            QueryParam::Null(v) => v as &(dyn ToSql + Sync),
            QueryParam::Bool(v) => v as &(dyn ToSql + Sync),
            QueryParam::Int16(v) => v as &(dyn ToSql + Sync),
            QueryParam::Int32(v) => v as &(dyn ToSql + Sync),
            QueryParam::Int64(v) => v as &(dyn ToSql + Sync),
            QueryParam::Float32(v) => v as &(dyn ToSql + Sync),
            QueryParam::Float(v) => v as &(dyn ToSql + Sync),
            QueryParam::Text(v) => v as &(dyn ToSql + Sync),
            QueryParam::Json(v) => v as &(dyn ToSql + Sync),
        })
        .collect()
}

pub(super) fn parse_bool(v: &Value, pos: usize) -> Result<bool, ExecError> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::String(s) => s
            .parse::<bool>()
            .map_err(|_| ExecError::InvalidParams(format!("param ${pos} cannot parse as bool"))),
        _ => Err(ExecError::InvalidParams(format!(
            "param ${pos} cannot parse as bool"
        ))),
    }
}

pub(super) fn parse_i16(v: &Value, pos: usize) -> Result<i16, ExecError> {
    let n = parse_i64(v, pos)?;
    i16::try_from(n)
        .map_err(|_| ExecError::InvalidParams(format!("param ${pos} out of range for int2")))
}

pub(super) fn parse_i32(v: &Value, pos: usize) -> Result<i32, ExecError> {
    let n = parse_i64(v, pos)?;
    i32::try_from(n)
        .map_err(|_| ExecError::InvalidParams(format!("param ${pos} out of range for int4")))
}

pub(super) fn parse_i64(v: &Value, pos: usize) -> Result<i64, ExecError> {
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else if let Some(u) = n.as_u64() {
                i64::try_from(u).map_err(|_| {
                    ExecError::InvalidParams(format!("param ${pos} out of range for int8"))
                })
            } else {
                Err(ExecError::InvalidParams(format!(
                    "param ${pos} cannot parse as int8"
                )))
            }
        }
        Value::String(s) => s
            .parse::<i64>()
            .map_err(|_| ExecError::InvalidParams(format!("param ${pos} cannot parse as int8"))),
        _ => Err(ExecError::InvalidParams(format!(
            "param ${pos} cannot parse as int8"
        ))),
    }
}

pub(super) fn parse_f32(v: &Value, pos: usize) -> Result<f32, ExecError> {
    let n = parse_f64(v, pos)?;
    Ok(n as f32)
}

pub(super) fn parse_f64(v: &Value, pos: usize) -> Result<f64, ExecError> {
    match v {
        Value::Number(n) => n.as_f64().ok_or_else(|| {
            ExecError::InvalidParams(format!("param ${pos} cannot parse as float8"))
        }),
        Value::String(s) => s
            .parse::<f64>()
            .map_err(|_| ExecError::InvalidParams(format!("param ${pos} cannot parse as float8"))),
        _ => Err(ExecError::InvalidParams(format!(
            "param ${pos} cannot parse as float8"
        ))),
    }
}

pub(super) fn parse_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub(super) fn validate_param_count(expected: usize, actual: usize) -> Result<(), ExecError> {
    if expected == actual {
        return Ok(());
    }
    Err(ExecError::InvalidParams(format!(
        "placeholder count mismatch: sql requires {expected}, params provided {actual}"
    )))
}
