use crate::query::{ColumnInfo, Value};
use crate::sql_dialect::{BuildSqlError, quote_ident};

/// Render `value` as a SQL literal for `driver_id`.
///
/// Escaping is dialect-specific and getting it wrong is not cosmetic.
/// MySQL and ClickHouse treat a backslash as an escape character inside
/// a string literal, so a value ending in one would consume the closing
/// quote and let the rest of the value parse as SQL. PostgreSQL, SQLite
/// and SQL Server treat a backslash as an ordinary character, where
/// doubling it would corrupt the value instead.
///
/// Row values come from the database, which the project treats as
/// untrusted input.
pub fn render_sql_literal(driver_id: &str, value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::Bool(value) => value.to_string(),
        Value::Int(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::Decimal(value) => value.to_string(),
        Value::Text(text) => quote_literal(driver_id, text),
        Value::Bytes(_) => "/* bytes omitted */ NULL".into(),
        Value::Date(date) => quote_literal(driver_id, &date.format("%Y-%m-%d").to_string()),
        Value::Time(time) => quote_literal(driver_id, &time.format("%H:%M:%S").to_string()),
        Value::DateTime(stamp) => quote_literal(driver_id, &stamp.format("%Y-%m-%d %H:%M:%S").to_string()),
        Value::TimestampTz(stamp) => quote_literal(driver_id, &stamp.to_rfc3339()),
        Value::Uuid(id) => quote_literal(driver_id, &id.to_string()),
        Value::Json(json) => quote_literal(driver_id, &json.to_string()),
    }
}

fn quote_literal(driver_id: &str, text: &str) -> String {
    let escaped = match driver_id {
        "mysql" | "clickhouse" => text.replace('\\', "\\\\").replace('\'', "''"),
        _ => text.replace('\'', "''"),
    };
    format!("'{escaped}'")
}

/// Render a complete `INSERT` for one result row, for the user to paste
/// and run.
///
/// Generated columns are left out: the database computes them and
/// rejects an `INSERT` that supplies one, so including them produced a
/// statement that could never execute. An auto-increment column is kept,
/// because copying a row usually means keeping its key, and every
/// supported engine accepts an explicit value for one.
pub fn build_insert_literal(
    driver_id: &str,
    schema: Option<&str>,
    table: &str,
    columns: &[ColumnInfo],
    row: &[Value],
) -> Result<String, BuildSqlError> {
    if columns.len() != row.len() {
        return Err(BuildSqlError::LengthMismatch {
            expected: columns.len(),
            got: row.len(),
        });
    }
    let mut names: Vec<String> = Vec::with_capacity(columns.len());
    let mut values: Vec<String> = Vec::with_capacity(columns.len());
    for (column, value) in columns.iter().zip(row) {
        if column.is_generated {
            continue;
        }
        names.push(quote_ident(driver_id, &column.name));
        values.push(render_sql_literal(driver_id, value));
    }
    if names.is_empty() {
        return Err(BuildSqlError::NothingToUpdate);
    }
    let qualified = match schema {
        Some(schema) => format!("{}.{}", quote_ident(driver_id, schema), quote_ident(driver_id, table)),
        None => quote_ident(driver_id, table),
    };
    Ok(format!(
        "INSERT INTO {} ({}) VALUES ({});",
        qualified,
        names.join(", "),
        values.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: "text".into(),
            nullable: true,
            primary_key: false,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        }
    }

    fn generated(name: &str) -> ColumnInfo {
        ColumnInfo {
            is_generated: true,
            ..column(name)
        }
    }

    /// The exact shape that escaped a MySQL literal: the trailing
    /// backslash consumed the closing quote, so `OR 1=1` parsed as SQL
    /// rather than as data. Verified against MySQL 8.1, which evaluated
    /// the unescaped form as the expression `'x\'' OR 1=1` and returned 1.
    const BREAKOUT: &str = "x\\' OR 1=1 -- ";

    #[test]
    fn a_backslash_is_escaped_only_where_the_engine_treats_it_as_an_escape() {
        assert_eq!(
            render_sql_literal("mysql", &Value::Text(BREAKOUT.into())),
            "'x\\\\'' OR 1=1 -- '"
        );
        assert_eq!(
            render_sql_literal("clickhouse", &Value::Text(BREAKOUT.into())),
            "'x\\\\'' OR 1=1 -- '"
        );
        for driver_id in ["postgres", "sqlite", "mssql"] {
            assert_eq!(
                render_sql_literal(driver_id, &Value::Text(BREAKOUT.into())),
                "'x\\'' OR 1=1 -- '",
                "{driver_id} must keep a backslash literal"
            );
        }
    }

    #[test]
    fn a_rendered_literal_decodes_back_to_the_value_and_ends_where_it_should() {
        let awkward = [
            BREAKOUT,
            "ends with a backslash \\",
            "'",
            "''",
            "\\",
            "\\\\",
            "a'b\\c",
            "'; DROP TABLE users; --",
            "\\'; DROP TABLE users; --",
            "plain",
            "",
            "multi\nline",
            "unicode \u{2028} and emoji \u{1f600}",
        ];
        for driver_id in ["postgres", "mysql", "sqlite", "mssql", "clickhouse"] {
            for text in awkward {
                let rendered = render_sql_literal(driver_id, &Value::Text(text.into()));
                let (decoded, consumed) = decode_literal(driver_id, &rendered)
                    .unwrap_or_else(|| panic!("{driver_id} produced an unterminated literal for {text:?}: {rendered}"));
                assert_eq!(decoded, text, "{driver_id} changed the value: {rendered}");
                assert_eq!(
                    consumed,
                    rendered.chars().count(),
                    "{driver_id} let {text:?} close its literal early, leaving SQL behind: {rendered}"
                );
            }
        }
    }

    /// Decode a rendered literal the way the engine would, returning the
    /// text and how much of the input the literal consumed. A literal
    /// that ends before the end of the rendered string is a break-out:
    /// whatever follows would be parsed as SQL.
    fn decode_literal(driver_id: &str, rendered: &str) -> Option<(String, usize)> {
        let backslash_escapes = matches!(driver_id, "mysql" | "clickhouse");
        let characters: Vec<char> = rendered.chars().collect();
        if characters.first() != Some(&'\'') {
            return None;
        }
        let mut decoded = String::new();
        let mut index = 1usize;
        while index < characters.len() {
            match characters[index] {
                '\\' if backslash_escapes => {
                    decoded.push(*characters.get(index + 1)?);
                    index += 2;
                }
                '\'' if characters.get(index + 1) == Some(&'\'') => {
                    decoded.push('\'');
                    index += 2;
                }
                '\'' => return Some((decoded, index + 1)),
                other => {
                    decoded.push(other);
                    index += 1;
                }
            }
        }
        None
    }

    #[test]
    fn an_insert_leaves_out_a_generated_column_the_engine_would_reject() {
        let columns = vec![column("id"), generated("total"), column("note")];
        let row = vec![Value::Int(1), Value::Int(99), Value::Text("hi".into())];
        let sql = build_insert_literal("postgres", None, "t", &columns, &row).expect("build the insert");
        assert_eq!(sql, "INSERT INTO \"t\" (\"id\", \"note\") VALUES (1, 'hi');");
        assert!(!sql.contains("total"));
        assert!(!sql.contains("99"));
    }

    #[test]
    fn an_insert_keeps_an_auto_increment_column_so_a_copied_row_keeps_its_key() {
        let mut id = column("id");
        id.is_auto_increment = true;
        id.primary_key = true;
        let columns = vec![id, column("note")];
        let row = vec![Value::Int(7), Value::Text("hi".into())];
        let sql = build_insert_literal("mysql", None, "t", &columns, &row).expect("build the insert");
        assert_eq!(sql, "INSERT INTO `t` (`id`, `note`) VALUES (7, 'hi');");
    }

    #[test]
    fn an_insert_qualifies_with_the_schema_and_quotes_per_dialect() {
        let columns = vec![column("a")];
        let row = vec![Value::Int(1)];
        assert_eq!(
            build_insert_literal("postgres", Some("public"), "t", &columns, &row).expect("postgres"),
            "INSERT INTO \"public\".\"t\" (\"a\") VALUES (1);"
        );
        assert_eq!(
            build_insert_literal("mssql", Some("dbo"), "t", &columns, &row).expect("mssql"),
            "INSERT INTO [dbo].[t] ([a]) VALUES (1);"
        );
    }

    #[test]
    fn a_row_that_does_not_match_the_columns_is_refused_rather_than_rendered() {
        let columns = vec![column("a"), column("b")];
        let row = vec![Value::Int(1)];
        let error = build_insert_literal("postgres", None, "t", &columns, &row)
            .expect_err("a mismatched row must not produce SQL");
        assert!(matches!(error, BuildSqlError::LengthMismatch { expected: 2, got: 1 }));
    }

    #[test]
    fn a_row_of_only_generated_columns_has_nothing_to_insert() {
        let columns = vec![generated("a")];
        let row = vec![Value::Int(1)];
        let error = build_insert_literal("postgres", None, "t", &columns, &row).expect_err("nothing to insert");
        assert!(matches!(error, BuildSqlError::NothingToUpdate));
    }

    #[test]
    fn scalar_values_render_without_quotes_and_null_stays_a_keyword() {
        assert_eq!(render_sql_literal("postgres", &Value::Null), "NULL");
        assert_eq!(render_sql_literal("postgres", &Value::Bool(true)), "true");
        assert_eq!(render_sql_literal("postgres", &Value::Int(-3)), "-3");
        assert_eq!(render_sql_literal("postgres", &Value::Float(1.5)), "1.5");
    }

    #[test]
    fn a_quote_inside_a_value_is_doubled_for_every_dialect() {
        for driver_id in ["postgres", "mysql", "sqlite", "mssql", "clickhouse"] {
            assert_eq!(
                render_sql_literal(driver_id, &Value::Text("O'Brien".into())),
                "'O''Brien'",
                "{driver_id}"
            );
        }
    }

    #[test]
    fn bytes_are_marked_rather_than_silently_rendered_as_data() {
        let rendered = render_sql_literal("postgres", &Value::Bytes(vec![1, 2, 3]));
        assert!(rendered.contains("bytes omitted"), "{rendered}");
    }
}
