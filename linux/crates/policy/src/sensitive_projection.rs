use sqlparser::ast::{Expr, Query, SelectItem, SetExpr, Statement, TableFactor};
use sqlparser::parser::Parser;

use crate::classify::dialect_for;
use crate::mask::column_is_sensitive;

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
    text_identifiers(&expr.to_string()).any(|ident| column_is_sensitive(ident, patterns))
}

fn text_identifiers(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|s| !s.is_empty())
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
}
