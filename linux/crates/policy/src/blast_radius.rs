use sqlparser::ast::{Delete, Expr, FromTable, Statement, TableFactor, TableWithJoins};
use sqlparser::dialect::{Dialect, GenericDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlastRadiusResult {
    pub count_sql: String,
    pub tables: Vec<String>,
}

/// Rewrite an UPDATE or DELETE into `SELECT count(*) FROM ... WHERE ...`
/// so the guard can measure blast radius before executing. Returns
/// `None` when the statement is not a single UPDATE/DELETE or cannot
/// be rewritten safely.
pub fn count_sql_for_mutation(sql: &str, driver_id: &str) -> Option<BlastRadiusResult> {
    let dialect = dialect_for(driver_id);
    let statements = Parser::parse_sql(dialect.as_ref(), sql.trim()).ok()?;
    if statements.len() != 1 {
        return None;
    }
    match &statements[0] {
        Statement::Update { table, selection, .. } => {
            let tables = table_names_from_factor(&table.relation);
            let from = vec![TableWithJoins {
                relation: table.relation.clone(),
                joins: table.joins.clone(),
            }];
            Some(build_count_sql(&from, selection.as_ref(), tables, driver_id))
        }
        Statement::Delete(Delete { from, selection, .. }) => {
            let list = match from {
                FromTable::WithFromKeyword(t) | FromTable::WithoutKeyword(t) => t,
            };
            let tables: Vec<String> = list.iter().flat_map(|t| table_names_from_factor(&t.relation)).collect();
            Some(build_count_sql(list, selection.as_ref(), tables, driver_id))
        }
        _ => None,
    }
}

fn build_count_sql(
    from: &[TableWithJoins],
    selection: Option<&Expr>,
    tables: Vec<String>,
    driver_id: &str,
) -> BlastRadiusResult {
    let table_sql = if from.len() == 1 {
        match &from[0].relation {
            TableFactor::Table { name, alias, .. } => {
                let base = name.to_string();
                match alias {
                    Some(a) => format!("{base} AS {}", a.name),
                    None => base,
                }
            }
            _ => from[0].relation.to_string(),
        }
    } else {
        from.iter()
            .map(|t| t.relation.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };

    let where_sql = selection.map(|s| format!(" WHERE {s}")).unwrap_or_default();

    let count_fn = if driver_id == "mssql" {
        "COUNT_BIG(*)"
    } else {
        "count(*)"
    };

    BlastRadiusResult {
        count_sql: format!("SELECT {count_fn} FROM {table_sql}{where_sql}"),
        tables,
    }
}

fn table_names_from_factor(factor: &TableFactor) -> Vec<String> {
    match factor {
        TableFactor::Table { name, .. } => vec![name.to_string()],
        _ => Vec::new(),
    }
}

fn dialect_for(driver_id: &str) -> Box<dyn Dialect> {
    match driver_id {
        "postgres" => Box::new(PostgreSqlDialect {}),
        "mysql" => Box::new(MySqlDialect {}),
        "sqlite" => Box::new(SQLiteDialect {}),
        "mssql" => Box::new(MsSqlDialect {}),
        _ => Box::new(GenericDialect {}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_delete_with_where() {
        let r = count_sql_for_mutation("DELETE FROM payments WHERE status = 'x'", "postgres").unwrap();
        let sql = r.count_sql.to_lowercase();
        assert!(sql.contains("count"), "{sql}");
        assert!(sql.contains("payments"), "{sql}");
        assert!(sql.contains("where"), "{sql}");
    }

    #[test]
    fn rewrite_update() {
        let r = count_sql_for_mutation("UPDATE t SET a = 1 WHERE id = 2", "postgres").unwrap();
        assert!(r.count_sql.to_lowercase().contains("count"));
        assert!(r.tables.iter().any(|t| t.contains('t')));
    }

    #[test]
    fn select_returns_none() {
        assert!(count_sql_for_mutation("SELECT * FROM t", "postgres").is_none());
    }
}
