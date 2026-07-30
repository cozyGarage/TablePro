use tablepro_core::{QueryResult, Value};

pub const DEFAULT_SENSITIVE_PATTERNS: &[&str] = &[
    "*pan*",
    "*card*",
    "*cvv*",
    "*cvc*",
    "*ssn*",
    "*iban*",
    "*secret*",
    "*token*",
    "*password*",
    "*passwd*",
    "*api_key*",
    "*apikey*",
    "*private_key*",
];

const REDACTED: &str = "***REDACTED***";

pub fn column_is_sensitive(column_name: &str, patterns: &[String]) -> bool {
    let name = column_name.to_lowercase();
    patterns.iter().any(|pat| {
        let p = pat.to_lowercase();
        glob::Pattern::new(&p).map(|g| g.matches(&name)).unwrap_or(false)
    })
}

/// Mask sensitive columns in a result set. Non-null values become the
/// redaction sentinel; Null stays Null.
pub fn apply_masking(mut result: QueryResult, patterns: &[String]) -> QueryResult {
    if patterns.is_empty() {
        return result;
    }
    let sensitive: Vec<bool> = result
        .columns
        .iter()
        .map(|c| column_is_sensitive(&c.name, patterns))
        .collect();
    if !sensitive.iter().any(|s| *s) {
        return result;
    }
    for row in &mut result.rows {
        for (i, cell) in row.iter_mut().enumerate() {
            if sensitive.get(i).copied().unwrap_or(false) && !matches!(cell, Value::Null) {
                *cell = Value::Text(REDACTED.into());
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tablepro_core::ColumnInfo;

    #[test]
    fn matches_pan_column() {
        let patterns: Vec<String> = DEFAULT_SENSITIVE_PATTERNS.iter().map(|s| (*s).into()).collect();
        assert!(column_is_sensitive("card_pan", &patterns));
        assert!(column_is_sensitive("CVV", &patterns));
        assert!(!column_is_sensitive("amount", &patterns));
    }

    #[test]
    fn redacts_cells() {
        let patterns = vec!["*pan*".into()];
        let result = QueryResult {
            columns: vec![
                ColumnInfo {
                    name: "id".into(),
                    data_type: "int".into(),
                    nullable: false,
                    primary_key: true,
                    is_auto_increment: false,
                    default_value: None,
                    is_generated: false,
                },
                ColumnInfo {
                    name: "pan".into(),
                    data_type: "text".into(),
                    nullable: true,
                    primary_key: false,
                    is_auto_increment: false,
                    default_value: None,
                    is_generated: false,
                },
            ],
            rows: vec![vec![Value::Int(1), Value::Text("4111111111111111".into())]],
            truncated: false,
        };
        let masked = apply_masking(result, &patterns);
        assert_eq!(masked.rows[0][0], Value::Int(1));
        assert_eq!(masked.rows[0][1], Value::Text(REDACTED.into()));
    }
}
