#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CellEditorKind {
    Text,
    Date,
    Int,
    Float,
    Json,
}

pub(super) fn classify_editor_kind(data_type: &str) -> CellEditorKind {
    if is_date_type(data_type) {
        CellEditorKind::Date
    } else if is_json_type(data_type) {
        CellEditorKind::Json
    } else if is_int_type(data_type) {
        CellEditorKind::Int
    } else if is_float_type(data_type) {
        CellEditorKind::Float
    } else {
        CellEditorKind::Text
    }
}

pub(super) fn is_bool_type(data_type: &str) -> bool {
    let dt = data_type.to_ascii_lowercase();
    matches!(dt.as_str(), "bool" | "boolean" | "bit" | "tinyint(1)")
}

pub(super) fn is_date_type(data_type: &str) -> bool {
    let dt = data_type.to_ascii_lowercase();
    dt == "date" || (dt.starts_with("date") && !dt.contains("datetime") && !dt.contains("time"))
}

pub(super) fn is_int_type(data_type: &str) -> bool {
    let dt = data_type.to_ascii_lowercase();
    matches!(
        dt.as_str(),
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
        || dt.starts_with("mediumint(")
}

pub(super) fn is_float_type(data_type: &str) -> bool {
    let dt = data_type.to_ascii_lowercase();
    matches!(dt.as_str(), "float" | "double" | "real" | "double precision") || dt.starts_with("float(")
}

pub(super) fn is_json_type(data_type: &str) -> bool {
    let dt = data_type.to_ascii_lowercase();
    dt.contains("json")
}

pub(super) fn is_bytes_type(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("blob") || lower.contains("bytea") || lower == "binary" || lower == "varbinary"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_type_detection() {
        assert!(is_bytes_type("BYTEA"));
        assert!(is_bytes_type("blob"));
        assert!(is_bytes_type("LONGBLOB"));
        assert!(is_bytes_type("mediumblob"));
        assert!(is_bytes_type("tinyblob"));
        assert!(is_bytes_type("VARBINARY"));
        assert!(is_bytes_type("binary"));
        assert!(!is_bytes_type("text"));
        assert!(!is_bytes_type("integer"));
    }
}
