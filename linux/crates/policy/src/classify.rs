use serde::{Deserialize, Serialize};
use sqlparser::ast::{
    CopyTarget, Cte, Delete, Expr, FromTable, FunctionArg, FunctionArgExpr, FunctionArguments, Insert, ObjectName,
    Query, SelectItem, SetExpr, Statement, TableFactor, TableObject, UtilityOption, Value,
};
use sqlparser::dialect::{Dialect, GenericDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer};

/// Coarse statement class used by policy rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatementClass {
    Select,
    Insert,
    Update,
    Delete,
    Ddl,
    Administrative,
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
    pub contains_ddl: bool,
    pub contains_mutating_dml: bool,
    pub contains_unscoped_dml: bool,
    pub contains_unknown_write: bool,
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
            contains_ddl: false,
            contains_mutating_dml: false,
            contains_unscoped_dml: false,
            contains_unknown_write: true,
            is_multi_statement: false,
            parse_error: Some(message.into()),
        }
    }
}

pub fn statement_requires_write_capability(sql: &str, driver_id: &str) -> bool {
    classify(sql, driver_id).writes
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
    let mut contains_ddl = false;
    let mut contains_mutating_dml = false;
    let mut contains_unscoped_dml = false;
    let mut contains_unknown_write = false;
    let contains_administrative_call = sql_contains_administrative_call(trimmed, dialect.as_ref(), driver_id);

    for stmt in &statements {
        let facts = classify_statement(stmt);
        writes |= facts.writes;
        contains_ddl |= facts.contains_ddl;
        contains_mutating_dml |= facts.contains_mutating_dml;
        contains_unscoped_dml |= facts.contains_unscoped_dml;
        contains_unknown_write |= facts.contains_unknown_write;
        if facts.class == StatementClass::Administrative {
            class = StatementClass::Administrative;
        } else if class != StatementClass::Administrative && (facts.class.is_write() || class == StatementClass::Select)
        {
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

    if contains_administrative_call {
        class = StatementClass::Administrative;
        writes = true;
    }

    if writes && class == StatementClass::Select {
        class = StatementClass::Other;
    }

    StatementFacts {
        class,
        writes,
        tables,
        has_where,
        contains_ddl,
        contains_mutating_dml,
        contains_unscoped_dml,
        contains_unknown_write,
        is_multi_statement: is_multi,
        parse_error: None,
    }
}

/// Detect administrative calls anywhere in a successfully parsed statement.
/// Token inspection complements the AST walk so calls hidden in predicates,
/// ordering expressions, or less-common expression wrappers still fail closed,
/// while names inside literals and comments remain harmless. Function names
/// must be followed by an opening parenthesis; stored procedures are invoked
/// without one and are matched on the name alone.
fn sql_contains_administrative_call(sql: &str, dialect: &dyn Dialect, driver_id: &str) -> bool {
    let Ok(tokens) = Tokenizer::new(dialect, sql).tokenize() else {
        return false;
    };

    tokens.iter().enumerate().any(|(index, token)| {
        let Token::Word(word) = token else {
            return false;
        };
        if is_administrative_procedure_name(driver_id, &word.value) {
            return true;
        }
        if !is_administrative_function_name(&word.value)
            && !is_engine_administrative_function_name(driver_id, &word.value)
        {
            return false;
        }

        tokens[index + 1..]
            .iter()
            .find(|next| !matches!(next, Token::Whitespace(_)))
            .is_some_and(|next| matches!(next, Token::LParen))
    })
}

fn is_engine_administrative_function_name(driver_id: &str, name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    if driver_id != "mysql" {
        return false;
    }
    matches!(lowered.as_str(), "benchmark" | "load_file" | "sleep")
}

fn is_administrative_procedure_name(driver_id: &str, name: &str) -> bool {
    if driver_id != "mssql" {
        return false;
    }
    let lowered = name.to_ascii_lowercase();
    lowered.starts_with("xp_")
        || matches!(
            lowered.as_str(),
            "sp_addsrvrolemember" | "sp_configure" | "sp_lock" | "sp_password" | "sp_who" | "sp_who2"
        )
}

/// Per-output-column sensitivity for a single simple SELECT, derived from the
/// parsed projection rather than the result set's reported column names.
/// Defeats aliasing (`pan AS p`) and expression wrapping (`substr(pan,1,8)`),
/// and one level of derived-table wildcard (`SELECT * FROM (SELECT pan AS v
/// FROM cards) t`). Returns `None` when the statement's shape is not one of
/// these recognized cases (multi-statement, CTE, set operation, joins, mixed
/// wildcard/expr projection, and so on); callers must treat `None` as
/// "unknown" and fall back to matching on the result set's column names, not
/// as "nothing is sensitive".
pub fn sensitive_projection(sql: &str, driver_id: &str, patterns: &[String]) -> Option<Vec<bool>> {
    let dialect = dialect_for(driver_id);
    let trimmed = sql.trim();
    let statements = Parser::parse_sql(dialect.as_ref(), trimmed).ok()?;
    let [Statement::Query(query)] = statements.as_slice() else {
        return None;
    };
    select_projection_sensitivity(query, patterns)
}

fn select_projection_sensitivity(query: &Query, patterns: &[String]) -> Option<Vec<bool>> {
    if query.with.is_some() {
        return None;
    }
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    if let [item] = select.projection.as_slice()
        && matches!(item, SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _))
    {
        let [table] = select.from.as_slice() else {
            return None;
        };
        if !table.joins.is_empty() {
            return None;
        }
        let TableFactor::Derived { subquery, .. } = &table.relation else {
            return None;
        };
        return select_projection_sensitivity(subquery, patterns);
    }
    let mut sensitive = Vec::with_capacity(select.projection.len());
    for item in &select.projection {
        match item {
            SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                sensitive.push(expr_references_sensitive(expr, patterns));
            }
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => return None,
        }
    }
    Some(sensitive)
}

fn expr_references_sensitive(expr: &Expr, patterns: &[String]) -> bool {
    text_identifiers(&expr.to_string()).any(|ident| crate::mask::column_is_sensitive(ident, patterns))
}

fn text_identifiers(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|s| !s.is_empty())
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
            contains_ddl: false,
            contains_mutating_dml: true,
            contains_unscoped_dml: selection.is_none(),
            contains_unknown_write: false,
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::Delete(delete) => classify_delete(delete),
        Statement::CreateTable(c) => StatementFacts {
            class: StatementClass::Ddl,
            writes: true,
            tables: object_name_strings(&c.name),
            has_where: true,
            contains_ddl: true,
            contains_mutating_dml: false,
            contains_unscoped_dml: false,
            contains_unknown_write: false,
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::CreateView { name, .. } => StatementFacts {
            class: StatementClass::Ddl,
            writes: true,
            tables: object_name_strings(name),
            has_where: true,
            contains_ddl: true,
            contains_mutating_dml: false,
            contains_unscoped_dml: false,
            contains_unknown_write: false,
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
            contains_ddl: true,
            contains_mutating_dml: false,
            contains_unscoped_dml: false,
            contains_unknown_write: false,
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::AlterTable { name, .. } => StatementFacts {
            class: StatementClass::Ddl,
            writes: true,
            tables: object_name_strings(name),
            has_where: true,
            contains_ddl: true,
            contains_mutating_dml: false,
            contains_unscoped_dml: false,
            contains_unknown_write: false,
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::Drop { names, .. } => StatementFacts {
            class: StatementClass::Ddl,
            writes: true,
            tables: names.iter().flat_map(object_name_strings).collect(),
            has_where: true,
            contains_ddl: true,
            contains_mutating_dml: false,
            contains_unscoped_dml: false,
            contains_unknown_write: false,
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::Truncate { table_names, .. } => ddl_facts(table_names.iter().map(|t| t.name.to_string()).collect()),
        Statement::CreateVirtualTable { .. }
        | Statement::CreateRole { .. }
        | Statement::CreateSecret { .. }
        | Statement::CreateServer(_)
        | Statement::CreatePolicy { .. }
        | Statement::CreateConnector(_)
        | Statement::AlterIndex { .. }
        | Statement::AlterView { .. }
        | Statement::AlterType(_)
        | Statement::AlterRole { .. }
        | Statement::AlterPolicy { .. }
        | Statement::AlterConnector { .. }
        | Statement::DropFunction { .. }
        | Statement::DropDomain(_)
        | Statement::DropProcedure { .. }
        | Statement::DropSecret { .. }
        | Statement::DropPolicy { .. }
        | Statement::DropConnector { .. }
        | Statement::CreateExtension { .. }
        | Statement::DropExtension { .. }
        | Statement::Comment { .. }
        | Statement::CreateSchema { .. }
        | Statement::CreateDatabase { .. }
        | Statement::CreateFunction(_)
        | Statement::CreateTrigger { .. }
        | Statement::DropTrigger { .. }
        | Statement::CreateProcedure { .. }
        | Statement::CreateMacro { .. }
        | Statement::CreateStage { .. }
        | Statement::Grant { .. }
        | Statement::Deny(_)
        | Statement::Revoke { .. }
        | Statement::CreateSequence { .. }
        | Statement::CreateDomain(_)
        | Statement::CreateType { .. } => ddl_facts(Vec::new()),
        Statement::Kill { .. } => StatementFacts {
            class: StatementClass::Administrative,
            writes: true,
            tables: Vec::new(),
            has_where: true,
            contains_ddl: false,
            contains_mutating_dml: false,
            contains_unscoped_dml: false,
            contains_unknown_write: false,
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::StartTransaction { .. } | Statement::Commit { .. } | Statement::Rollback { .. } => StatementFacts {
            class: StatementClass::Transaction,
            writes: false,
            tables: Vec::new(),
            has_where: true,
            contains_ddl: false,
            contains_mutating_dml: false,
            contains_unscoped_dml: false,
            contains_unknown_write: false,
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::Explain {
            analyze,
            statement,
            options,
            ..
        } => classify_explain(*analyze, statement, options.as_deref()),
        Statement::ExplainTable { table_name, .. } => StatementFacts {
            class: StatementClass::Select,
            writes: false,
            tables: object_name_strings(table_name),
            has_where: true,
            contains_ddl: false,
            contains_mutating_dml: false,
            contains_unscoped_dml: false,
            contains_unknown_write: false,
            is_multi_statement: false,
            parse_error: None,
        },
        Statement::Copy { target, .. } => {
            let reaches_the_host = matches!(target, CopyTarget::File { .. } | CopyTarget::Program { .. });
            StatementFacts {
                class: if reaches_the_host {
                    StatementClass::Administrative
                } else {
                    StatementClass::Other
                },
                writes: true,
                tables: Vec::new(),
                has_where: true,
                contains_ddl: false,
                contains_mutating_dml: false,
                contains_unscoped_dml: false,
                contains_unknown_write: !reaches_the_host,
                is_multi_statement: false,
                parse_error: None,
            }
        }
        _ => StatementFacts {
            class: StatementClass::Other,
            writes: true,
            tables: Vec::new(),
            has_where: true,
            contains_ddl: false,
            contains_mutating_dml: false,
            contains_unscoped_dml: false,
            contains_unknown_write: true,
            is_multi_statement: false,
            parse_error: None,
        },
    }
}

fn classify_explain(analyze: bool, statement: &Statement, options: Option<&[UtilityOption]>) -> StatementFacts {
    if analyze || options.is_some_and(analyze_option_enabled) {
        return classify_statement(statement);
    }
    let inner = classify_statement(statement);
    StatementFacts {
        class: StatementClass::Select,
        writes: false,
        tables: inner.tables,
        has_where: true,
        contains_ddl: false,
        contains_mutating_dml: false,
        contains_unscoped_dml: false,
        contains_unknown_write: false,
        is_multi_statement: false,
        parse_error: None,
    }
}

fn analyze_option_enabled(options: &[UtilityOption]) -> bool {
    options.iter().any(|option| {
        option.name.value.eq_ignore_ascii_case("analyze") && !option.arg.as_ref().is_some_and(is_disabled_option_arg)
    })
}

fn is_disabled_option_arg(arg: &Expr) -> bool {
    match arg {
        Expr::Value(value) => match &value.value {
            Value::Boolean(flag) => !flag,
            Value::Number(number, _) => number.parse::<f64>().is_ok_and(|n| n == 0.0),
            Value::SingleQuotedString(text) | Value::DoubleQuotedString(text) => is_disabled_word(text),
            _ => false,
        },
        Expr::Identifier(ident) => is_disabled_word(&ident.value),
        _ => false,
    }
}

fn is_disabled_word(text: &str) -> bool {
    matches!(text.to_ascii_lowercase().as_str(), "false" | "off" | "0" | "no")
}

fn ddl_facts(tables: Vec<String>) -> StatementFacts {
    StatementFacts {
        class: StatementClass::Ddl,
        writes: true,
        tables,
        has_where: true,
        contains_ddl: true,
        contains_mutating_dml: false,
        contains_unscoped_dml: false,
        contains_unknown_write: false,
        is_multi_statement: false,
        parse_error: None,
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
        contains_ddl: false,
        contains_mutating_dml: false,
        contains_unscoped_dml: false,
        contains_unknown_write: false,
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
        contains_ddl: false,
        contains_mutating_dml: true,
        contains_unscoped_dml: delete.selection.is_none(),
        contains_unknown_write: false,
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
    let mut administrative = false;
    let mut contains_ddl = false;
    let mut contains_mutating_dml = false;
    let mut contains_unscoped_dml = false;
    let mut contains_unknown_write = false;
    let mut tables = Vec::new();

    if let Some(with) = &query.with {
        for Cte { query: cte_q, .. } in &with.cte_tables {
            let inner = classify_query(cte_q);
            writes |= inner.writes;
            administrative |= inner.class == StatementClass::Administrative;
            contains_ddl |= inner.contains_ddl;
            contains_mutating_dml |= inner.contains_mutating_dml;
            contains_unscoped_dml |= inner.contains_unscoped_dml;
            contains_unknown_write |= inner.contains_unknown_write;
            tables.extend(inner.tables);
        }
    }

    let body_facts = classify_set_expr(query.body.as_ref());
    writes |= body_facts.writes;
    administrative |= body_facts.class == StatementClass::Administrative;
    contains_ddl |= body_facts.contains_ddl;
    contains_mutating_dml |= body_facts.contains_mutating_dml;
    contains_unscoped_dml |= body_facts.contains_unscoped_dml;
    contains_unknown_write |= body_facts.contains_unknown_write;
    writes |= administrative;
    tables.extend(body_facts.tables);

    StatementFacts {
        class: if administrative {
            StatementClass::Administrative
        } else if writes {
            StatementClass::Other
        } else {
            StatementClass::Select
        },
        writes,
        tables,
        has_where: !contains_unscoped_dml,
        contains_ddl,
        contains_mutating_dml,
        contains_unscoped_dml,
        contains_unknown_write,
        is_multi_statement: false,
        parse_error: None,
    }
}

fn classify_set_expr(body: &SetExpr) -> StatementFacts {
    match body {
        SetExpr::Insert(statement) | SetExpr::Update(statement) | SetExpr::Delete(statement) => {
            classify_statement(statement)
        }
        SetExpr::Query(query) => classify_query(query),
        SetExpr::SetOperation { left, right, .. } => {
            let left = classify_set_expr(left);
            let right = classify_set_expr(right);
            StatementFacts {
                class: if left.class == StatementClass::Administrative || right.class == StatementClass::Administrative
                {
                    StatementClass::Administrative
                } else if left.writes || right.writes {
                    StatementClass::Other
                } else {
                    StatementClass::Select
                },
                writes: left.writes || right.writes,
                tables: left.tables.into_iter().chain(right.tables).collect(),
                has_where: left.has_where && right.has_where,
                contains_ddl: left.contains_ddl || right.contains_ddl,
                contains_mutating_dml: left.contains_mutating_dml || right.contains_mutating_dml,
                contains_unscoped_dml: left.contains_unscoped_dml || right.contains_unscoped_dml,
                contains_unknown_write: left.contains_unknown_write || right.contains_unknown_write,
                is_multi_statement: false,
                parse_error: None,
            }
        }
        _ => {
            let administrative = set_expr_administrative(body);
            let writes = set_expr_writes(body) || administrative;
            StatementFacts {
                class: if administrative {
                    StatementClass::Administrative
                } else if writes {
                    StatementClass::Other
                } else {
                    StatementClass::Select
                },
                writes,
                tables: set_expr_tables(body),
                has_where: true,
                contains_ddl: false,
                contains_mutating_dml: false,
                contains_unscoped_dml: false,
                contains_unknown_write: writes && !administrative,
                is_multi_statement: false,
                parse_error: None,
            }
        }
    }
}

fn set_expr_administrative(body: &SetExpr) -> bool {
    match body {
        SetExpr::Query(query) => classify_query(query).class == StatementClass::Administrative,
        SetExpr::SetOperation { left, right, .. } => set_expr_administrative(left) || set_expr_administrative(right),
        SetExpr::Select(select) => select.projection.iter().any(|item| match item {
            SelectItem::ExprWithAlias { expr, .. } | SelectItem::UnnamedExpr(expr) => expr_is_administrative(expr),
            _ => false,
        }),
        _ => false,
    }
}

fn expr_is_administrative(expr: &Expr) -> bool {
    match expr {
        Expr::Function(function) => {
            is_administrative_function(&function.name) || function_arguments_are_administrative(&function.args)
        }
        Expr::Subquery(query) => classify_query(query).class == StatementClass::Administrative,
        Expr::BinaryOp { left, right, .. } => expr_is_administrative(left) || expr_is_administrative(right),
        Expr::UnaryOp { expr, .. } | Expr::Nested(expr) => expr_is_administrative(expr),
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            operand.as_ref().is_some_and(|expr| expr_is_administrative(expr))
                || conditions
                    .iter()
                    .any(|case| expr_is_administrative(&case.condition) || expr_is_administrative(&case.result))
                || else_result.as_ref().is_some_and(|expr| expr_is_administrative(expr))
        }
        _ => false,
    }
}

fn is_administrative_function(name: &ObjectName) -> bool {
    is_administrative_function_name(&name.to_string())
}

fn is_administrative_function_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "lo_create"
            | "lo_export"
            | "lo_import"
            | "lo_put"
            | "lo_unlink"
            | "lowrite"
            | "nextval"
            | "pg_advisory_lock"
            | "pg_advisory_lock_shared"
            | "pg_advisory_unlock"
            | "pg_advisory_unlock_all"
            | "pg_advisory_unlock_shared"
            | "pg_advisory_xact_lock"
            | "pg_advisory_xact_lock_shared"
            | "pg_backup_start"
            | "pg_backup_stop"
            | "pg_cancel_backend"
            | "pg_create_restore_point"
            | "pg_create_logical_replication_slot"
            | "pg_create_physical_replication_slot"
            | "pg_drop_replication_slot"
            | "pg_log_backend_memory_contexts"
            | "pg_promote"
            | "pg_reload_conf"
            | "pg_rotate_logfile"
            | "pg_start_backup"
            | "pg_stop_backup"
            | "pg_switch_wal"
            | "pg_terminate_backend"
            | "pg_try_advisory_lock"
            | "pg_try_advisory_lock_shared"
            | "pg_try_advisory_xact_lock"
            | "pg_try_advisory_xact_lock_shared"
            | "pg_wal_replay_pause"
            | "pg_wal_replay_resume"
            | "lo_from_bytea"
            | "lo_truncate"
            | "lo_truncate64"
            | "set_config"
            | "setval"
            | "pg_read_file"
            | "pg_read_binary_file"
            | "pg_stat_file"
            | "pg_ls_dir"
            | "pg_ls_logdir"
            | "pg_ls_waldir"
            | "pg_ls_archive_statusdir"
            | "pg_ls_tmpdir"
            | "dblink"
            | "dblink_exec"
            | "dblink_connect"
            | "dblink_send_query"
            | "query_to_xml"
    )
}

fn function_arguments_are_administrative(arguments: &FunctionArguments) -> bool {
    match arguments {
        FunctionArguments::None => false,
        FunctionArguments::Subquery(query) => classify_query(query).class == StatementClass::Administrative,
        FunctionArguments::List(arguments) => arguments.args.iter().any(|argument| {
            let expression = match argument {
                FunctionArg::Named { arg, .. } | FunctionArg::ExprNamed { arg, .. } | FunctionArg::Unnamed(arg) => arg,
            };
            matches!(expression, FunctionArgExpr::Expr(expr) if expr_is_administrative(expr))
        }),
    }
}

fn set_expr_writes(body: &SetExpr) -> bool {
    match body {
        SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) => true,
        // VALUES is a read-only row constructor. INSERT ... VALUES is already
        // represented by Statement::Insert and classified as a write above.
        SetExpr::Values(_) => false,
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

    fn sensitive_patterns() -> Vec<String> {
        crate::mask::DEFAULT_SENSITIVE_PATTERNS
            .iter()
            .map(|p| (*p).to_string())
            .collect()
    }

    #[test]
    fn an_aliased_sensitive_column_is_flagged() {
        let positions = sensitive_projection("SELECT pan AS p FROM cards", "postgres", &sensitive_patterns()).unwrap();
        assert_eq!(positions, vec![true]);
    }

    #[test]
    fn a_sensitive_column_wrapped_in_an_expression_is_flagged() {
        let positions =
            sensitive_projection("SELECT substr(pan,1,8) FROM cards", "postgres", &sensitive_patterns()).unwrap();
        assert_eq!(positions, vec![true]);
    }

    #[test]
    fn a_wildcard_over_a_derived_table_sees_through_to_the_inner_alias() {
        let positions = sensitive_projection(
            "SELECT * FROM (SELECT pan AS v FROM cards) t",
            "postgres",
            &sensitive_patterns(),
        )
        .unwrap();
        assert_eq!(positions, vec![true]);
    }

    #[test]
    fn an_unrelated_column_is_not_flagged() {
        let positions =
            sensitive_projection("SELECT amount AS a FROM orders", "postgres", &sensitive_patterns()).unwrap();
        assert_eq!(positions, vec![false]);
    }

    #[test]
    fn a_plain_wildcard_over_a_real_table_is_unknown() {
        assert!(sensitive_projection("SELECT * FROM cards", "postgres", &sensitive_patterns()).is_none());
    }

    #[test]
    fn a_cte_is_unknown() {
        let sql = "WITH d AS (SELECT pan AS p FROM cards) SELECT * FROM d";
        assert!(sensitive_projection(sql, "postgres", &sensitive_patterns()).is_none());
    }

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
    fn values_and_recursive_values_cte_are_reads() {
        for sql in [
            "VALUES (1), (2)",
            "WITH RECURSIVE counter(value) AS (VALUES(0) UNION ALL SELECT value + 1 FROM counter WHERE value < 10) SELECT sum(value) FROM counter",
        ] {
            let facts = classify(sql, "sqlite");
            assert_eq!(facts.class, StatementClass::Select, "SQL: {sql}, facts: {facts:?}");
            assert!(!facts.writes, "SQL: {sql}, facts: {facts:?}");
        }
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
    fn mixed_script_preserves_ddl_in_both_orders() {
        for sql in [
            "DROP TABLE protected_data; INSERT INTO audit_log VALUES (1)",
            "INSERT INTO audit_log VALUES (1); DROP TABLE protected_data",
        ] {
            let facts = classify(sql, "postgres");
            assert!(facts.contains_ddl, "SQL: {sql}");
        }
    }

    #[test]
    fn mixed_script_preserves_unknown_writes_in_both_orders() {
        for sql in [
            "SET session_replication_role = replica; INSERT INTO jobs(id) VALUES (1)",
            "INSERT INTO jobs(id) VALUES (1); SET session_replication_role = replica",
        ] {
            let facts = classify(sql, "postgres");
            assert!(facts.contains_unknown_write, "SQL: {sql}");
        }
    }

    #[test]
    fn mixed_script_preserves_unscoped_dml_in_both_orders() {
        for sql in [
            "DELETE FROM protected_data; CREATE TABLE replacement(id integer)",
            "CREATE TABLE replacement(id integer); DELETE FROM protected_data",
        ] {
            let facts = classify(sql, "postgres");
            assert!(facts.contains_mutating_dml, "SQL: {sql}");
            assert!(facts.contains_unscoped_dml, "SQL: {sql}");
        }
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
    fn broader_schema_and_privilege_changes_are_ddl() {
        for sql in [
            "CREATE SCHEMA reporting",
            "CREATE SEQUENCE job_ids",
            "CREATE FUNCTION answer() RETURNS integer LANGUAGE SQL AS 'SELECT 42'",
            "GRANT SELECT ON TABLE jobs TO analyst",
            "REVOKE SELECT ON TABLE jobs FROM analyst",
        ] {
            let facts = classify(sql, "postgres");
            assert!(facts.contains_ddl, "SQL: {sql}, facts: {facts:?}");
        }
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

    #[test]
    fn postgres_terminate_backend_is_administrative() {
        let facts = classify("SELECT pg_terminate_backend(42)", "postgres");
        assert_eq!(facts.class, StatementClass::Administrative);
        assert!(facts.writes);
    }

    #[test]
    fn postgres_side_effecting_functions_are_administrative() {
        for sql in [
            "SELECT pg_promote()",
            "SELECT nextval('jobs_id_seq')",
            "SELECT setval('jobs_id_seq', 10)",
            "SELECT pg_advisory_lock(42)",
            "SELECT pg_try_advisory_lock(42)",
            "SELECT pg_wal_replay_pause()",
            "SELECT pg_wal_replay_resume()",
            "SELECT set_config('work_mem', '64MB', false)",
        ] {
            let facts = classify(sql, "postgres");
            assert_eq!(facts.class, StatementClass::Administrative, "SQL: {sql}");
            assert!(facts.writes, "SQL: {sql}");
        }
    }

    #[test]
    fn administrative_function_nested_in_expression_is_detected() {
        let facts = classify("SELECT coalesce(pg_cancel_backend(42), false)", "postgres");
        assert_eq!(facts.class, StatementClass::Administrative);
        assert!(facts.writes);
    }

    #[test]
    fn administrative_function_in_predicate_is_detected() {
        let facts = classify("SELECT 1 WHERE pg_terminate_backend(42)", "postgres");
        assert_eq!(facts.class, StatementClass::Administrative);
        assert!(facts.writes);
    }

    #[test]
    fn administrative_function_name_in_literal_or_comment_is_not_a_call() {
        for sql in [
            "SELECT 'pg_terminate_backend(42)'",
            "SELECT 1 -- pg_terminate_backend(42)",
            "SELECT 1 /* pg_terminate_backend(42) */",
        ] {
            let facts = classify(sql, "postgres");
            assert_eq!(facts.class, StatementClass::Select, "SQL: {sql}");
            assert!(!facts.writes, "SQL: {sql}");
        }
    }

    #[test]
    fn mysql_kill_is_administrative() {
        let facts = classify("KILL 42", "mysql");
        assert_eq!(facts.class, StatementClass::Administrative);
        assert!(facts.writes);
    }

    #[test]
    fn plain_explain_of_a_select_is_a_read() {
        let facts = classify("EXPLAIN SELECT id FROM users", "postgres");
        assert_eq!(facts.class, StatementClass::Select);
        assert!(!facts.writes);
        assert!(!facts.contains_unknown_write);
        assert_eq!(facts.tables, vec!["users".to_string()]);
    }

    #[test]
    fn plain_explain_of_a_write_does_not_execute_it() {
        let facts = classify("EXPLAIN DELETE FROM users", "postgres");
        assert_eq!(facts.class, StatementClass::Select);
        assert!(!facts.writes);
        assert!(!facts.contains_mutating_dml);
        assert!(!facts.contains_unscoped_dml);
    }

    #[test]
    fn explain_verbose_of_a_select_is_a_read() {
        let facts = classify("EXPLAIN (VERBOSE) SELECT id FROM users", "postgres");
        assert_eq!(facts.class, StatementClass::Select);
        assert!(!facts.writes);
    }

    #[test]
    fn explain_analyze_keeps_the_inner_statement_facts() {
        let facts = classify("EXPLAIN ANALYZE DELETE FROM users", "postgres");
        assert_eq!(facts.class, StatementClass::Delete);
        assert!(facts.writes);
        assert!(facts.contains_mutating_dml);
        assert!(facts.contains_unscoped_dml);
        assert_eq!(facts.tables, vec!["users".to_string()]);
    }

    #[test]
    fn explain_with_a_parenthesized_analyze_option_is_a_write() {
        let facts = classify("EXPLAIN (ANALYZE) UPDATE users SET name = 'x'", "postgres");
        assert_eq!(facts.class, StatementClass::Update);
        assert!(facts.writes);
        assert!(facts.contains_unscoped_dml);
    }

    #[test]
    fn explain_with_analyze_disabled_is_a_read() {
        for sql in [
            "EXPLAIN (ANALYZE false) DELETE FROM users",
            "EXPLAIN (ANALYZE off) DELETE FROM users",
            "EXPLAIN (ANALYZE 0) DELETE FROM users",
        ] {
            let facts = classify(sql, "postgres");
            assert_eq!(facts.class, StatementClass::Select, "SQL: {sql}");
            assert!(!facts.writes, "SQL: {sql}");
        }
    }

    #[test]
    fn describe_table_is_a_read() {
        let facts = classify("DESCRIBE users", "mysql");
        assert_eq!(facts.class, StatementClass::Select);
        assert!(!facts.writes);
    }

    #[test]
    fn only_a_provable_read_skips_the_write_capability_check() {
        for sql in ["SELECT 1", "SELECT id FROM t WHERE id = 1"] {
            assert!(!statement_requires_write_capability(sql, "postgres"), "{sql}");
        }
        for sql in [
            "DELETE FROM t WHERE id = 1",
            "CREATE TABLE t (id int)",
            "SELECT pg_read_file('/etc/passwd')",
            "COPY t TO PROGRAM 'sh'",
            "this is not sql at all",
            "",
        ] {
            assert!(statement_requires_write_capability(sql, "postgres"), "{sql}");
        }
    }
}
