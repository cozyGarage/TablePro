use serde::{Deserialize, Serialize};
use sqlparser::ast::{
    Cte, Delete, Expr, FromTable, Insert, ObjectName, Query, SelectItem, SetExpr, Statement, TableFactor, TableObject,
};
use sqlparser::dialect::{Dialect, GenericDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

/// Coarse statement class used by policy rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatementClass {
    Select,
    Insert,
    Update,
    Delete,
    Ddl,
    Transaction,
    Other,
    /// Parser rejected the SQL. Fail closed: treated as a write.
    Unparseable,
}

impl StatementClass {
    pub fn is_write(self) -> bool {
        !matches!(self, Self::Select | Self::Transaction)
    }
}

/// Facts extracted from one SQL string. Multi-statement scripts set
/// `is_multi_statement` and merge write/table facts across statements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatementFacts {
    pub class: StatementClass,
    pub writes: bool,
    pub tables: Vec<String>,
    pub has_where: bool,
    pub is_multi_statement: bool,
    pub parse_error: Option<String>,
}

impl StatementFacts {
    pub fn unparseable(message: impl Into<String>) -> Self {
        Self {
            class: StatementClass::Unparseable,
            writes: true,
            tables: Vec::new(),
            has_where: false,
            is_multi_statement: false,
            parse_error: Some(message.into()),
        }
    }
}

pub fn classify(sql: &str, driver_id: &str) -> StatementFacts {
    let dialect = dialect_for(driver_id);
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return StatementFacts::unparseable("empty SQL");
    }

    let statements = match Parser::parse_sql(dialect.as_ref(), trimmed) {
        Ok(s) => s,
        Err(e) => return StatementFacts::unparseable(e.to_string()),
    };

    if statements.is_empty() {
        return StatementFacts::unparseable("no statements parsed");
    }

    let is_multi = statements.len() > 1;
    let mut class = StatementClass::Select;
    let mut writes = false;
    let mut tables = Vec::new();
    let mut has_where = true;

    for stmt in &statements {
        let facts = classify_statement(stmt);
        writes |= facts.writes;
        if facts.class.is_write() || class == StatementClass::Select {
            class = facts.class;
        }
        for t in facts.tables {
            if !tables.iter().any(|x| x == &t) {
                tables.push(t);
            }
        }
        if facts.class == StatementClass::Update || facts.class == StatementClass::Delete {
            has_where &= facts.has_where;
        }
    }

    if writes && class == StatementClass::Select {
        class = StatementClass::Other;
    }

    StatementFacts {
        class,
        writes,
        tables,
        has_where,
        is_multi_statement: is_multi,
        parse_error: None,
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

fn classify_statement(stmt: &Statement) -> StatementFacts {
    match stmt {
        Statement::Query(q) => classify_query(q),
        Statement::Insert(insert) => classify_insert(insert),
        Statement::Update { table, selection, .. } => StatementFacts {
            class: StatementClass::Update,
            writes: true,
            tables: table_factor_names(&table.relation),
            has_where: selection.is_some(),
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::Delete(delete) => classify_delete(delete),
        Statement::CreateTable(c) => StatementFacts {
            class: StatementClass::Ddl,
            writes: true,
            tables: object_name_strings(&c.name),
            has_where: true,
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::CreateView { name, .. } => StatementFacts {
            class: StatementClass::Ddl,
            writes: true,
            tables: object_name_strings(name),
            has_where: true,
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::CreateIndex(c) => StatementFacts {
            class: StatementClass::Ddl,
            writes: true,
            tables: match &c.name {
                Some(n) => object_name_strings(n),
                None => object_name_strings(&c.table_name),
            },
            has_where: true,
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::AlterTable { name, .. } => StatementFacts {
            class: StatementClass::Ddl,
            writes: true,
            tables: object_name_strings(name),
            has_where: true,
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::Drop { names, .. } => StatementFacts {
            class: StatementClass::Ddl,
            writes: true,
            tables: names.iter().flat_map(object_name_strings).collect(),
            has_where: true,
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::Truncate { table_names, .. } => StatementFacts {
            class: StatementClass::Ddl,
            writes: true,
            tables: table_names.iter().map(|t| t.name.to_string()).collect(),
            has_where: false,
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::StartTransaction { .. } | Statement::Commit { .. } | Statement::Rollback { .. } => StatementFacts {
            class: StatementClass::Transaction,
            writes: false,
            tables: Vec::new(),
            has_where: true,
            is_multi_statement: false,
            parse_error: None,
        },
        _ => StatementFacts {
            class: StatementClass::Other,
            writes: true,
            tables: Vec::new(),
            has_where: true,
            is_multi_statement: false,
            parse_error: None,
        },
    }
}

fn classify_insert(insert: &Insert) -> StatementFacts {
    let tables = match &insert.table {
        TableObject::TableName(name) => object_name_strings(name),
        TableObject::TableFunction(f) => vec![f.name.to_string()],
    };
    StatementFacts {
        class: StatementClass::Insert,
        writes: true,
        tables,
        has_where: true,
        is_multi_statement: false,
        parse_error: None,
    }
}

fn classify_delete(delete: &Delete) -> StatementFacts {
    let mut tables = Vec::new();
    for t in &delete.tables {
        tables.extend(object_name_strings(t));
    }
    tables.extend(from_table_names(&delete.from));
    StatementFacts {
        class: StatementClass::Delete,
        writes: true,
        tables,
        has_where: delete.selection.is_some(),
        is_multi_statement: false,
        parse_error: None,
    }
}

fn from_table_names(from: &FromTable) -> Vec<String> {
    let list = match from {
        FromTable::WithFromKeyword(t) | FromTable::WithoutKeyword(t) => t,
    };
    list.iter().flat_map(|t| table_factor_names(&t.relation)).collect()
}

fn classify_query(query: &Query) -> StatementFacts {
    let mut writes = false;
    let mut tables = Vec::new();

    if let Some(with) = &query.with {
        for Cte { query: cte_q, .. } in &with.cte_tables {
            let inner = classify_query(cte_q);
            writes |= inner.writes;
            tables.extend(inner.tables);
        }
    }

    writes |= set_expr_writes(query.body.as_ref());
    tables.extend(set_expr_tables(query.body.as_ref()));

    StatementFacts {
        class: if writes {
            StatementClass::Other
        } else {
            StatementClass::Select
        },
        writes,
        tables,
        has_where: true,
        is_multi_statement: false,
        parse_error: None,
    }
}

fn set_expr_writes(body: &SetExpr) -> bool {
    match body {
        SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Values(_) => true,
        SetExpr::Query(q) => classify_query(q).writes,
        SetExpr::SetOperation { left, right, .. } => set_expr_writes(left) || set_expr_writes(right),
        SetExpr::Select(select) => select.projection.iter().any(|item| match item {
            SelectItem::ExprWithAlias { expr, .. } | SelectItem::UnnamedExpr(expr) => expr_writes(expr),
            _ => false,
        }),
        SetExpr::Table(_) => false,
    }
}

fn set_expr_tables(body: &SetExpr) -> Vec<String> {
    match body {
        SetExpr::Select(select) => {
            let mut tables = Vec::new();
            for t in &select.from {
                tables.extend(table_factor_names(&t.relation));
                for join in &t.joins {
                    tables.extend(table_factor_names(&join.relation));
                }
            }
            tables
        }
        SetExpr::Query(q) => classify_query(q).tables,
        SetExpr::SetOperation { left, right, .. } => {
            let mut t = set_expr_tables(left);
            t.extend(set_expr_tables(right));
            t
        }
        SetExpr::Insert(stmt) | SetExpr::Update(stmt) | SetExpr::Delete(stmt) => classify_statement(stmt).tables,
        _ => Vec::new(),
    }
}

fn expr_writes(expr: &Expr) -> bool {
    match expr {
        Expr::Subquery(q) => classify_query(q).writes,
        Expr::BinaryOp { left, right, .. } => expr_writes(left) || expr_writes(right),
        Expr::UnaryOp { expr, .. } => expr_writes(expr),
        Expr::Nested(e) => expr_writes(e),
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            operand.as_ref().is_some_and(|e| expr_writes(e))
                || conditions
                    .iter()
                    .any(|c| expr_writes(&c.condition) || expr_writes(&c.result))
                || else_result.as_ref().is_some_and(|e| expr_writes(e))
        }
        _ => false,
    }
}

fn object_name_strings(name: &ObjectName) -> Vec<String> {
    vec![name.to_string()]
}

fn table_factor_names(factor: &TableFactor) -> Vec<String> {
    match factor {
        TableFactor::Table { name, .. } => object_name_strings(name),
        TableFactor::Derived { subquery, .. } => classify_query(subquery).tables,
        TableFactor::NestedJoin { table_with_joins, .. } => {
            let mut out = table_factor_names(&table_with_joins.relation);
            for j in &table_with_joins.joins {
                out.extend(table_factor_names(&j.relation));
            }
            out
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_is_read() {
        let f = classify("SELECT * FROM users WHERE id = 1", "postgres");
        assert!(!f.writes);
        assert_eq!(f.class, StatementClass::Select);
        assert!(f.tables.iter().any(|t| t.contains("users")));
    }

    #[test]
    fn delete_without_where() {
        let f = classify("DELETE FROM payments", "postgres");
        assert!(f.writes);
        assert_eq!(f.class, StatementClass::Delete);
        assert!(!f.has_where);
    }

    #[test]
    fn update_with_where() {
        let f = classify("UPDATE payments SET status = 'ok' WHERE id = 1", "postgres");
        assert!(f.writes);
        assert_eq!(f.class, StatementClass::Update);
        assert!(f.has_where);
    }

    #[test]
    fn data_modifying_cte_is_write() {
        let sql = "WITH d AS (DELETE FROM payments WHERE id = 1 RETURNING *) SELECT * FROM d";
        let f = classify(sql, "postgres");
        assert!(f.writes, "data-modifying CTE must report writes=true: {f:?}");
    }

    #[test]
    fn insert_select_cte_write() {
        let sql = "WITH x AS (INSERT INTO t(a) VALUES (1) RETURNING a) SELECT * FROM x";
        let f = classify(sql, "postgres");
        assert!(f.writes);
    }

    #[test]
    fn multi_statement_flagged() {
        let f = classify("SELECT 1; DELETE FROM t", "postgres");
        assert!(f.is_multi_statement);
        assert!(f.writes);
    }

    #[test]
    fn parse_failure_fail_closed() {
        let f = classify("SELECCT * FROM", "postgres");
        assert_eq!(f.class, StatementClass::Unparseable);
        assert!(f.writes);
        assert!(f.parse_error.is_some());
    }

    #[test]
    fn truncate_is_ddl_write() {
        let f = classify("TRUNCATE TABLE payments", "postgres");
        assert!(f.writes);
        assert_eq!(f.class, StatementClass::Ddl);
    }

    #[test]
    fn mysql_delete() {
        let f = classify("DELETE FROM t WHERE id = 1", "mysql");
        assert_eq!(f.class, StatementClass::Delete);
        assert!(f.has_where);
    }

    #[test]
    fn mssql_update() {
        let f = classify("UPDATE dbo.t SET a = 1 WHERE a = 2", "mssql");
        assert_eq!(f.class, StatementClass::Update);
        assert!(f.has_where);
    }
}
