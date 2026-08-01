use tablepro_core::{ColumnInfo, Value};

/// Collapse newlines / carriage returns to spaces, then squash any
/// resulting consecutive whitespace runs to a single space. Applied
/// at cell-edit commit time for non-JSON columns so a multi-line
/// clipboard paste into a single-line cell never reaches the SQL
/// layer with embedded `\n` — driver behaviour for that case is
/// type-specific (text columns store literally; numeric / date
/// columns parse-fail) and worth normalising up front.
pub(super) fn normalize_single_line_input(text: &str) -> String {
    let replaced: String = text
        .chars()
        .map(|c| if matches!(c, '\n' | '\r') { ' ' } else { c })
        .collect();
    replaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn parse_input_for_column(text: &str, col: Option<&ColumnInfo>) -> Result<Value, String> {
    let Some(col) = col else {
        return Ok(Value::Text(text.to_string()));
    };
    if text.is_empty() {
        if col.nullable || col.default_value.is_some() {
            return Ok(Value::Null);
        }
        return Err(crate::tr!("Field is required"));
    }
    let dt = col.data_type.to_ascii_lowercase();
    let trimmed = text.trim();
    match classify_type(&dt) {
        TypeKind::Bool => parse_bool_value(trimmed),
        TypeKind::Int => parse_int_value(trimmed),
        TypeKind::Float => parse_float_value(trimmed),
        TypeKind::Decimal => parse_decimal_value(trimmed),
        TypeKind::Uuid => parse_uuid_value(trimmed),
        TypeKind::Json => parse_json_value(trimmed),
        TypeKind::TimestampTz => parse_timestamptz_value(trimmed),
        TypeKind::DateTime => parse_datetime_value(trimmed),
        TypeKind::Date => parse_date_value(trimmed),
        TypeKind::Time => parse_time_value(trimmed),
        TypeKind::Text => Ok(Value::Text(text.to_string())),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TypeKind {
    Bool,
    Int,
    Float,
    Decimal,
    Uuid,
    Json,
    TimestampTz,
    DateTime,
    Date,
    Time,
    Text,
}

/// Map a lowercased `data_type` string to a coarse `TypeKind`. Order
/// of checks matters because several SQL types share substrings — for
/// example `timestamptz` / `timestamp with time zone` must be matched
/// before bare `timestamp`, and `tinyint(1)` (MySQL bool) must be
/// matched before generic `tinyint` / `int` patterns.
pub(super) fn classify_type(dt: &str) -> TypeKind {
    // Postgres `format_type()` returns "bit(1)" for length-1 BIT
    // columns (not the bare "bit" the original guard expected).
    // Both forms classify as Bool so the cell renders as a checkbox
    // rather than a text editor that rejects "true"/"false" with
    // "Invalid integer".
    if matches!(dt, "bool" | "boolean" | "bit" | "bit(1)" | "tinyint(1)") {
        return TypeKind::Bool;
    }
    if dt.contains("uuid") {
        return TypeKind::Uuid;
    }
    if dt.contains("json") {
        return TypeKind::Json;
    }
    if dt.contains("timestamptz") || dt.contains("with time zone") {
        return TypeKind::TimestampTz;
    }
    if dt.contains("timestamp") || dt.contains("datetime") {
        return TypeKind::DateTime;
    }
    if dt == "date" || (dt.starts_with("date") && !dt.contains("datetime") && !dt.contains("time")) {
        return TypeKind::Date;
    }
    if dt == "time" || dt.starts_with("time(") || dt == "time without time zone" {
        return TypeKind::Time;
    }
    if matches!(dt, "decimal" | "numeric" | "money") || dt.starts_with("decimal(") || dt.starts_with("numeric(") {
        return TypeKind::Decimal;
    }
    if matches!(dt, "float" | "double" | "real" | "double precision") || dt.starts_with("float(") {
        return TypeKind::Float;
    }
    if matches!(
        dt,
        "int"
            | "int2"
            | "int4"
            | "int8"
            | "integer"
            | "smallint"
            | "bigint"
            | "tinyint"
            | "mediumint"
            | "serial"
            | "bigserial"
            | "smallserial"
    ) || dt.starts_with("int(")
        || dt.starts_with("integer(")
        || dt.starts_with("smallint(")
        || dt.starts_with("bigint(")
        || dt.starts_with("tinyint(")
        || dt.starts_with("mediumint(")
    {
        return TypeKind::Int;
    }
    TypeKind::Text
}

pub(super) fn parse_bool_value(text: &str) -> Result<Value, String> {
    match text.to_ascii_lowercase().as_str() {
        "true" | "t" | "1" | "yes" | "y" | "on" => Ok(Value::Bool(true)),
        "false" | "f" | "0" | "no" | "n" | "off" => Ok(Value::Bool(false)),
        _ => Err(crate::tr!("Invalid boolean. Use true/false, yes/no, or 1/0.")),
    }
}

pub(super) fn parse_int_value(text: &str) -> Result<Value, String> {
    text.parse::<i64>()
        .map(Value::Int)
        .map_err(|_| crate::tr!("Invalid integer"))
}

pub(super) fn parse_float_value(text: &str) -> Result<Value, String> {
    text.parse::<f64>()
        .map(Value::Float)
        .map_err(|_| crate::tr!("Invalid number"))
}

pub(super) fn parse_decimal_value(text: &str) -> Result<Value, String> {
    text.parse::<rust_decimal::Decimal>()
        .map(Value::Decimal)
        .map_err(|_| crate::tr!("Invalid decimal"))
}

pub(super) fn parse_uuid_value(text: &str) -> Result<Value, String> {
    uuid::Uuid::parse_str(text)
        .map(Value::Uuid)
        .map_err(|_| crate::tr!("Invalid UUID. Expected 8-4-4-4-12 hex digits."))
}

pub(super) fn parse_json_value(text: &str) -> Result<Value, String> {
    serde_json::from_str::<serde_json::Value>(text)
        .map(Value::Json)
        .map_err(|e| crate::tr!("Invalid JSON: {error}").replace("{error}", &e.to_string()))
}

pub(super) fn parse_timestamptz_value(text: &str) -> Result<Value, String> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(text) {
        return Ok(Value::TimestampTz(dt.with_timezone(&chrono::Utc)));
    }
    Err(crate::tr!(
        "Invalid timestamp. Use ISO 8601, e.g. 2024-01-15T14:30:00Z."
    ))
}

pub(super) fn parse_datetime_value(text: &str) -> Result<Value, String> {
    let formats = [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
    ];
    for fmt in &formats {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(text, fmt) {
            return Ok(Value::DateTime(dt));
        }
    }
    Err(crate::tr!("Invalid datetime. Use YYYY-MM-DD HH:MM:SS."))
}

pub(super) fn parse_date_value(text: &str) -> Result<Value, String> {
    chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map(Value::Date)
        .map_err(|_| crate::tr!("Invalid date. Use YYYY-MM-DD."))
}

pub(super) fn parse_time_value(text: &str) -> Result<Value, String> {
    let formats = ["%H:%M:%S", "%H:%M:%S%.f", "%H:%M"];
    for fmt in &formats {
        if let Ok(t) = chrono::NaiveTime::parse_from_str(text, fmt) {
            return Ok(Value::Time(t));
        }
    }
    Err(crate::tr!("Invalid time. Use HH:MM:SS."))
}

#[cfg(test)]
mod tests {
    use super::{TypeKind, classify_type, parse_input_for_column};
    use tablepro_core::{ColumnInfo, Value};

    fn col(data_type: &str, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            name: "x".into(),
            data_type: data_type.into(),
            nullable,
            primary_key: false,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        }
    }

    fn col_with_default(data_type: &str, default: &str) -> ColumnInfo {
        let mut c = col(data_type, false);
        c.default_value = Some(default.into());
        c
    }

    #[test]
    fn classify_disambiguates_overlapping_types() {
        assert_eq!(classify_type("tinyint(1)"), TypeKind::Bool);
        assert_eq!(classify_type("tinyint"), TypeKind::Int);
        assert_eq!(classify_type("uuid"), TypeKind::Uuid);
        assert_eq!(classify_type("jsonb"), TypeKind::Json);
        assert_eq!(classify_type("timestamptz"), TypeKind::TimestampTz);
        assert_eq!(classify_type("timestamp with time zone"), TypeKind::TimestampTz);
        assert_eq!(classify_type("timestamp without time zone"), TypeKind::DateTime);
        assert_eq!(classify_type("timestamp"), TypeKind::DateTime);
        assert_eq!(classify_type("datetime"), TypeKind::DateTime);
        assert_eq!(classify_type("date"), TypeKind::Date);
        assert_eq!(classify_type("time"), TypeKind::Time);
        assert_eq!(classify_type("integer"), TypeKind::Int);
        assert_eq!(classify_type("int4"), TypeKind::Int);
        assert_eq!(classify_type("bigint"), TypeKind::Int);
        assert_eq!(classify_type("decimal(10,2)"), TypeKind::Decimal);
        assert_eq!(classify_type("numeric"), TypeKind::Decimal);
        assert_eq!(classify_type("double precision"), TypeKind::Float);
        assert_eq!(classify_type("real"), TypeKind::Float);
        assert_eq!(classify_type("text"), TypeKind::Text);
        assert_eq!(classify_type("varchar(255)"), TypeKind::Text);
        // "interval" must NOT be classified as Int even though it
        // contains "int".
        assert_eq!(classify_type("interval"), TypeKind::Text);
    }

    #[test]
    fn empty_on_nullable_yields_null() {
        let r = parse_input_for_column("", Some(&col("text", true))).unwrap();
        assert!(matches!(r, Value::Null));
    }

    #[test]
    fn empty_on_not_null_with_default_yields_null() {
        let r = parse_input_for_column("", Some(&col_with_default("timestamp", "now()"))).unwrap();
        assert!(matches!(r, Value::Null));
    }

    #[test]
    fn empty_on_not_null_no_default_is_rejected() {
        let r = parse_input_for_column("", Some(&col("text", false)));
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("required"));
    }

    #[test]
    fn parses_int_decimal_float_bool() {
        assert!(matches!(
            parse_input_for_column("42", Some(&col("integer", false))).unwrap(),
            Value::Int(42)
        ));
        assert!(matches!(
            parse_input_for_column("3.14", Some(&col("real", false))).unwrap(),
            Value::Float(_)
        ));
        assert!(matches!(
            parse_input_for_column("99.99", Some(&col("decimal(10,2)", false))).unwrap(),
            Value::Decimal(_)
        ));
        assert!(matches!(
            parse_input_for_column("yes", Some(&col("boolean", false))).unwrap(),
            Value::Bool(true)
        ));
        assert!(matches!(
            parse_input_for_column("0", Some(&col("tinyint(1)", false))).unwrap(),
            Value::Bool(false)
        ));
    }

    #[test]
    fn parses_uuid_json_date_time_datetime_timestamptz() {
        let uuid = parse_input_for_column("550e8400-e29b-41d4-a716-446655440000", Some(&col("uuid", false))).unwrap();
        assert!(matches!(uuid, Value::Uuid(_)));

        let json = parse_input_for_column(r#"{"a":1}"#, Some(&col("jsonb", false))).unwrap();
        assert!(matches!(json, Value::Json(_)));

        let date = parse_input_for_column("2024-01-15", Some(&col("date", false))).unwrap();
        assert!(matches!(date, Value::Date(_)));

        let time = parse_input_for_column("14:30:00", Some(&col("time", false))).unwrap();
        assert!(matches!(time, Value::Time(_)));
        let time_short = parse_input_for_column("14:30", Some(&col("time", false))).unwrap();
        assert!(matches!(time_short, Value::Time(_)));

        let datetime = parse_input_for_column("2024-01-15 14:30:00", Some(&col("timestamp", false))).unwrap();
        assert!(matches!(datetime, Value::DateTime(_)));
        let datetime_t = parse_input_for_column("2024-01-15T14:30:00", Some(&col("datetime", false))).unwrap();
        assert!(matches!(datetime_t, Value::DateTime(_)));

        let ts = parse_input_for_column("2024-01-15T14:30:00Z", Some(&col("timestamptz", false))).unwrap();
        assert!(matches!(ts, Value::TimestampTz(_)));
    }

    #[test]
    fn rejects_invalid_type_specific_input() {
        assert!(parse_input_for_column("not-a-number", Some(&col("integer", false))).is_err());
        assert!(parse_input_for_column("not-a-uuid", Some(&col("uuid", false))).is_err());
        assert!(parse_input_for_column("{not json", Some(&col("jsonb", false))).is_err());
        assert!(parse_input_for_column("2024/01/15", Some(&col("date", false))).is_err());
        assert!(parse_input_for_column("13:00:99", Some(&col("time", false))).is_err());
        assert!(parse_input_for_column("not-a-date", Some(&col("timestamp", false))).is_err());
        assert!(parse_input_for_column("maybe", Some(&col("boolean", false))).is_err());
    }

    #[test]
    fn unknown_type_falls_through_to_text() {
        let r = parse_input_for_column("anything goes here", Some(&col("varchar(255)", false))).unwrap();
        assert!(matches!(r, Value::Text(_)));
    }

    #[test]
    fn null_sentinel_typed_literally_is_text() {
        let r = parse_input_for_column("<NULL>", Some(&col("text", true))).unwrap();
        match r {
            Value::Text(s) => assert_eq!(s, "<NULL>"),
            other => panic!("expected Text(\"<NULL>\") got {other:?}"),
        }
    }
}
