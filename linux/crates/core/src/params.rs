use crate::query::Value;
use crate::sql_dialect::placeholder_for;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedParameters {
    pub sql: String,
    pub names: Vec<String>,
    pub bindings: Vec<String>,
}

impl NamedParameters {
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParameterKind {
    #[default]
    Auto,
    Text,
    Integer,
    Decimal,
    Boolean,
    Null,
}

impl ParameterKind {
    pub const ALL: [Self; 6] = [
        Self::Auto,
        Self::Text,
        Self::Integer,
        Self::Decimal,
        Self::Boolean,
        Self::Null,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Text => "Text",
            Self::Integer => "Integer",
            Self::Decimal => "Decimal",
            Self::Boolean => "Boolean",
            Self::Null => "Null",
        }
    }

    pub fn from_index(index: u32) -> Self {
        Self::ALL.get(index as usize).copied().unwrap_or(Self::Auto)
    }

    pub fn index(self) -> u32 {
        Self::ALL.iter().position(|kind| *kind == self).unwrap_or(0) as u32
    }
}

pub fn parse_parameter_value(kind: ParameterKind, text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    match kind {
        ParameterKind::Null => Ok(Value::Null),
        ParameterKind::Text => Ok(Value::Text(text.to_string())),
        ParameterKind::Integer => trimmed
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| format!("{trimmed} is not a whole number")),
        ParameterKind::Decimal => trimmed
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(Value::Float)
            .ok_or_else(|| format!("{trimmed} is not a decimal number")),
        ParameterKind::Boolean => match trimmed.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" => Ok(Value::Bool(true)),
            "false" | "f" | "0" | "no" => Ok(Value::Bool(false)),
            _ => Err(format!("{trimmed} is not true or false")),
        },
        ParameterKind::Auto => Ok(infer_parameter_value(text)),
    }
}

fn infer_parameter_value(text: &str) -> Value {
    let trimmed = text.trim();
    if let Ok(value) = trimmed.parse::<i64>() {
        return Value::Int(value);
    }
    if (trimmed.contains('.') || trimmed.contains('e') || trimmed.contains('E'))
        && let Ok(value) = trimmed.parse::<f64>()
        && value.is_finite()
    {
        return Value::Float(value);
    }
    Value::Text(text.to_string())
}

pub fn extract_named_parameters(sql: &str, driver_id: &str) -> NamedParameters {
    let mut out = String::with_capacity(sql.len());
    let mut names: Vec<String> = Vec::new();
    let mut bindings: Vec<String> = Vec::new();
    let bytes = sql.as_bytes();
    let mut index = 0usize;

    while index < bytes.len() {
        let rest = &sql[index..];

        if let Some(length) = skip_span(rest, driver_id) {
            out.push_str(&sql[index..index + length]);
            index += length;
            continue;
        }

        if bytes[index] == b':' {
            if rest.starts_with("::") || rest.starts_with(":=") {
                out.push_str(&sql[index..index + 2]);
                index += 2;
                continue;
            }
            if let Some(name) = parameter_name(&rest[1..]) {
                let position = bindings.len();
                bindings.push(name.clone());
                if !names.contains(&name) {
                    names.push(name.clone());
                }
                out.push_str(&placeholder_for(driver_id, position));
                index += 1 + name.len();
                continue;
            }
        }

        let character = rest.chars().next().unwrap_or(':');
        out.push(character);
        index += character.len_utf8();
    }

    NamedParameters {
        sql: out,
        names,
        bindings,
    }
}

fn parameter_name(rest: &str) -> Option<String> {
    let mut end = 0usize;
    for (offset, character) in rest.char_indices() {
        let valid = if offset == 0 {
            character.is_ascii_alphabetic() || character == '_'
        } else {
            character.is_ascii_alphanumeric() || character == '_'
        };
        if !valid {
            break;
        }
        end = offset + character.len_utf8();
    }
    (end > 0).then(|| rest[..end].to_string())
}

fn skip_span(rest: &str, driver_id: &str) -> Option<usize> {
    if rest.starts_with("--") {
        return Some(rest.find('\n').map_or(rest.len(), |offset| offset + 1));
    }
    if let Some(body) = rest.strip_prefix("/*") {
        return Some(body.find("*/").map_or(rest.len(), |offset| offset + 4));
    }
    if rest.starts_with('\'') {
        return Some(quoted_length(rest, '\'', true));
    }
    if let Some(tag_length) = dollar_quote_length(rest, driver_id) {
        return Some(tag_length);
    }
    match (driver_id, rest.as_bytes().first()) {
        ("mysql" | "clickhouse", Some(b'`')) => Some(quoted_length(rest, '`', false)),
        ("mssql", Some(b'[')) => Some(bracket_length(rest)),
        (_, Some(b'"')) => Some(quoted_length(rest, '"', true)),
        _ => None,
    }
}

fn quoted_length(rest: &str, quote: char, doubling_escapes: bool) -> usize {
    let mut characters = rest.char_indices().skip(1);
    while let Some((offset, character)) = characters.next() {
        if character != quote {
            continue;
        }
        if doubling_escapes && rest[offset + character.len_utf8()..].starts_with(quote) {
            characters.next();
            continue;
        }
        return offset + character.len_utf8();
    }
    rest.len()
}

fn bracket_length(rest: &str) -> usize {
    match rest.find(']') {
        Some(offset) => offset + 1,
        None => rest.len(),
    }
}

fn dollar_quote_length(rest: &str, driver_id: &str) -> Option<usize> {
    if driver_id != "postgres" || !rest.starts_with('$') {
        return None;
    }
    let close = rest[1..].find('$')? + 1;
    let tag = &rest[..close + 1];
    let mut tag_characters = tag[1..close].chars();
    if let Some(first) = tag_characters.next()
        && !(first.is_ascii_alphabetic() || first == '_')
    {
        return None;
    }
    if tag_characters.any(|c| !c.is_ascii_alphanumeric() && c != '_') {
        return None;
    }
    let body = &rest[close + 1..];
    match body.find(tag) {
        Some(offset) => Some(close + 1 + offset + tag.len()),
        None => Some(rest.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_named_parameters_per_dialect() {
        let sql = "SELECT * FROM users WHERE name = :name AND age > :age";
        let postgres = extract_named_parameters(sql, "postgres");
        assert_eq!(postgres.sql, "SELECT * FROM users WHERE name = $1 AND age > $2");
        assert_eq!(postgres.names, vec!["name", "age"]);
        assert_eq!(postgres.bindings, vec!["name", "age"]);

        let mysql = extract_named_parameters(sql, "mysql");
        assert_eq!(mysql.sql, "SELECT * FROM users WHERE name = ? AND age > ?");

        let mssql = extract_named_parameters(sql, "mssql");
        assert_eq!(mssql.sql, "SELECT * FROM users WHERE name = @P1 AND age > @P2");
    }

    #[test]
    fn repeated_names_bind_once_per_occurrence() {
        let parsed = extract_named_parameters("SELECT :id, :id FROM t WHERE id = :id", "postgres");
        assert_eq!(parsed.sql, "SELECT $1, $2 FROM t WHERE id = $3");
        assert_eq!(parsed.names, vec!["id"]);
        assert_eq!(parsed.bindings, vec!["id", "id", "id"]);
    }

    #[test]
    fn leaves_postgres_casts_and_assignments_alone() {
        let parsed = extract_named_parameters("SELECT '1'::int, x := 2 FROM t", "postgres");
        assert!(parsed.is_empty());
        assert_eq!(parsed.sql, "SELECT '1'::int, x := 2 FROM t");
    }

    #[test]
    fn leaves_existing_positional_placeholders_alone() {
        let parsed = extract_named_parameters("SELECT * FROM t WHERE a = $1 AND b = ?", "postgres");
        assert!(parsed.is_empty());
        assert_eq!(parsed.sql, "SELECT * FROM t WHERE a = $1 AND b = ?");
    }

    #[test]
    fn ignores_placeholders_inside_literals_and_comments() {
        let sql = "SELECT ':not_a_param', \"col:not\" -- :nope\n, /* :nope2 */ x FROM t WHERE y = :yes";
        let parsed = extract_named_parameters(sql, "postgres");
        assert_eq!(parsed.names, vec!["yes"]);
        assert!(parsed.sql.contains("':not_a_param'"));
        assert!(parsed.sql.contains("\"col:not\""));
        assert!(parsed.sql.contains("-- :nope"));
        assert!(parsed.sql.contains("/* :nope2 */"));
        assert!(parsed.sql.ends_with("y = $1"));
    }

    #[test]
    fn ignores_placeholders_inside_dialect_quoting() {
        let mysql = extract_named_parameters("SELECT `col:not` FROM t WHERE a = :a", "mysql");
        assert_eq!(mysql.names, vec!["a"]);
        assert_eq!(mysql.sql, "SELECT `col:not` FROM t WHERE a = ?");

        let mssql = extract_named_parameters("SELECT [col:not] FROM t WHERE a = :a", "mssql");
        assert_eq!(mssql.names, vec!["a"]);
        assert_eq!(mssql.sql, "SELECT [col:not] FROM t WHERE a = @P1");
    }

    #[test]
    fn ignores_placeholders_inside_dollar_quoted_bodies() {
        let sql = "CREATE FUNCTION f() RETURNS int AS $body$ SELECT :nope $body$ LANGUAGE sql";
        let parsed = extract_named_parameters(sql, "postgres");
        assert!(parsed.is_empty());
        assert_eq!(parsed.sql, sql);
    }

    #[test]
    fn adjacent_positional_placeholders_are_not_dollar_quotes() {
        let parsed = extract_named_parameters("SELECT $1$2 FROM t WHERE a = :a", "postgres");
        assert_eq!(parsed.names, vec!["a"]);
        assert_eq!(parsed.sql, "SELECT $1$2 FROM t WHERE a = $1");
    }

    #[test]
    fn ignores_doubled_quotes_inside_literals() {
        let parsed = extract_named_parameters("SELECT 'it''s :not' , :yes FROM t", "postgres");
        assert_eq!(parsed.names, vec!["yes"]);
        assert_eq!(parsed.sql, "SELECT 'it''s :not' , $1 FROM t");
    }

    #[test]
    fn unterminated_literal_does_not_produce_parameters() {
        let parsed = extract_named_parameters("SELECT 'open :nope", "postgres");
        assert!(parsed.is_empty());
        assert_eq!(parsed.sql, "SELECT 'open :nope");
    }

    #[test]
    fn array_slices_are_not_parameters() {
        let parsed = extract_named_parameters("SELECT arr[1:3] FROM t", "postgres");
        assert!(parsed.is_empty());
    }

    #[test]
    fn multibyte_text_keeps_its_bytes() {
        let parsed = extract_named_parameters("SELECT 'héllo →' , :naïve FROM t", "postgres");
        assert_eq!(parsed.names, vec!["na"]);
        assert!(parsed.sql.contains("'héllo →'"));
    }

    #[test]
    fn explicit_kinds_parse_or_report_a_reason() {
        assert_eq!(
            parse_parameter_value(ParameterKind::Integer, " 42 "),
            Ok(Value::Int(42))
        );
        assert!(parse_parameter_value(ParameterKind::Integer, "x").is_err());
        assert_eq!(
            parse_parameter_value(ParameterKind::Decimal, "1.5"),
            Ok(Value::Float(1.5))
        );
        assert!(parse_parameter_value(ParameterKind::Decimal, "nan").is_err());
        assert_eq!(
            parse_parameter_value(ParameterKind::Boolean, "TRUE"),
            Ok(Value::Bool(true))
        );
        assert!(parse_parameter_value(ParameterKind::Boolean, "maybe").is_err());
        assert_eq!(parse_parameter_value(ParameterKind::Null, "ignored"), Ok(Value::Null));
        assert_eq!(
            parse_parameter_value(ParameterKind::Text, "42"),
            Ok(Value::Text("42".into()))
        );
    }

    #[test]
    fn auto_infers_numbers_and_keeps_everything_else_as_text() {
        assert_eq!(parse_parameter_value(ParameterKind::Auto, "7"), Ok(Value::Int(7)));
        assert_eq!(parse_parameter_value(ParameterKind::Auto, "-7"), Ok(Value::Int(-7)));
        assert_eq!(parse_parameter_value(ParameterKind::Auto, "2.5"), Ok(Value::Float(2.5)));
        assert_eq!(
            parse_parameter_value(ParameterKind::Auto, "true"),
            Ok(Value::Text("true".into()))
        );
        assert_eq!(
            parse_parameter_value(ParameterKind::Auto, "' OR 1=1 --"),
            Ok(Value::Text("' OR 1=1 --".into()))
        );
    }

    #[test]
    fn parameter_kind_index_round_trips() {
        for kind in ParameterKind::ALL {
            assert_eq!(ParameterKind::from_index(kind.index()), kind);
        }
        assert_eq!(ParameterKind::from_index(99), ParameterKind::Auto);
    }
}
