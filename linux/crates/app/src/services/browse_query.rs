//! Pure SQL planning shared by browse page and count requests.
//! Native unfiltered fetches stay available for non-SQL drivers.
use tablepro_core::{ColumnInfo, FilterSet, KEYSET_OFFSET_THRESHOLD, Value, build_filter_where, keyset_where_clause};

pub(crate) struct BrowseTarget<'a> {
    pub driver_id: &'a str,
    pub schema: Option<&'a str>,
    pub table: &'a str,
    pub columns: &'a [ColumnInfo],
    pub filter: &'a FilterSet,
}

#[derive(Debug)]
pub(crate) struct BoundQuery {
    pub sql: String,
    pub params: Vec<Value>,
}

#[derive(Debug)]
pub(crate) enum PageQuery {
    Native,
    Sql(BoundQuery),
}

impl BrowseTarget<'_> {
    fn filtered_query(&self, projection: &str) -> Result<BoundQuery, String> {
        let filter = build_filter_where(self.driver_id, self.columns, self.filter).map_err(|e| e.to_string())?;
        let quote = |s| tablepro_core::sql_dialect::quote_ident(self.driver_id, s);
        let target = match self.schema {
            Some(schema) => format!("{}.{}", quote(schema), quote(self.table)),
            None => quote(self.table),
        };
        let mut query = BoundQuery {
            sql: format!("SELECT {projection} FROM {target}"),
            params: Vec::new(),
        };
        if let Some((clause, params)) = filter {
            query.sql.push_str(" WHERE ");
            query.sql.push_str(&clause);
            query.params = params;
        }
        Ok(query)
    }

    pub fn count(&self) -> Result<BoundQuery, String> {
        self.filtered_query("COUNT(*)")
    }

    pub fn page(
        &self,
        offset: u64,
        limit: u64,
        sort: Option<(usize, bool)>,
        cursor: Option<&[Value]>,
    ) -> Result<PageQuery, String> {
        let mut query = self.filtered_query("*")?;
        let order = resolved_order_by(self.driver_id, self.columns, sort);
        let keys: Vec<&str> = self
            .columns
            .iter()
            .filter(|c| c.primary_key)
            .map(|c| c.name.as_str())
            .collect();
        let cursor = cursor.filter(|c| {
            offset >= KEYSET_OFFSET_THRESHOLD && sort.is_none() && !keys.is_empty() && c.len() == keys.len()
        });
        let actual_offset = if let Some(cursor) = cursor {
            let (clause, params) =
                keyset_where_clause(self.driver_id, &keys, cursor, query.params.len()).map_err(|e| e.to_string())?;
            query
                .sql
                .push_str(if self.filter.is_empty() { " WHERE " } else { " AND " });
            query.sql.push_str(&clause);
            query.params.extend(params);
            0
        } else {
            if self.filter.is_empty() && order.is_none() {
                return Ok(PageQuery::Native);
            }
            offset
        };
        query
            .sql
            .push_str(&tablepro_core::sql_dialect::build_order_and_pagination(
                self.driver_id,
                order.as_deref(),
                limit,
                actual_offset,
            ));
        Ok(PageQuery::Sql(query))
    }
}

fn resolved_order_by(driver_id: &str, columns: &[ColumnInfo], sort: Option<(usize, bool)>) -> Option<String> {
    let mut terms = Vec::new();
    let selected = sort.and_then(|(index, ascending)| {
        columns.get(index).map(|column| {
            let direction = if ascending { "ASC" } else { "DESC" };
            terms.push(format!(
                "{} {direction}",
                tablepro_core::sql_dialect::quote_ident(driver_id, &column.name)
            ));
            column.name.as_str()
        })
    });
    for column in columns.iter().filter(|column| column.primary_key) {
        if selected == Some(column.name.as_str()) {
            continue;
        }
        terms.push(format!(
            "{} ASC",
            tablepro_core::sql_dialect::quote_ident(driver_id, &column.name)
        ));
    }
    (!terms.is_empty()).then(|| terms.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tablepro_core::{FilterOp, FilterRule, FilterValue};
    fn column(name: &str, primary_key: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: "text".into(),
            nullable: false,
            primary_key,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        }
    }

    #[test]
    fn default_order_uses_every_primary_key_column() {
        let columns = vec![column("tenant", true), column("name", false), column("id", true)];
        assert_eq!(
            resolved_order_by("postgres", &columns, None).as_deref(),
            Some("\"tenant\" ASC, \"id\" ASC")
        );
    }

    #[test]
    fn explicit_sort_appends_primary_key_tie_breakers() {
        let columns = vec![column("tenant", true), column("name", false), column("id", true)];
        assert_eq!(
            resolved_order_by("postgres", &columns, Some((1, false))).as_deref(),
            Some("\"name\" DESC, \"tenant\" ASC, \"id\" ASC")
        );
    }

    #[test]
    fn sorted_primary_key_is_not_duplicated() {
        let columns = vec![column("tenant", true), column("id", true)];
        assert_eq!(
            resolved_order_by("postgres", &columns, Some((0, false))).as_deref(),
            Some("\"tenant\" DESC, \"id\" ASC")
        );
    }

    #[test]
    fn table_without_pk_or_sort_has_no_promised_order() {
        assert_eq!(resolved_order_by("postgres", &[column("name", false)], None), None);
    }
    #[test]
    fn invalid_filter_refuses_both_page_and_count() {
        let filter = FilterSet {
            rules: vec![FilterRule {
                column: "missing".into(),
                op: FilterOp::Eq,
                value: Some(FilterValue::Single("x".into())),
            }],
            ..Default::default()
        };
        let target = BrowseTarget {
            driver_id: "postgres",
            schema: None,
            table: "items",
            columns: &[],
            filter: &filter,
        };
        assert!(target.count().unwrap_err().contains("missing"));
        assert!(target.page(0, 100, None, None).unwrap_err().contains("missing"));
    }

    #[test]
    fn page_and_count_share_filter_and_keyset_parameters_follow_it() {
        let columns = vec![column("id", true), column("name", false)];
        let filter = FilterSet {
            rules: vec![FilterRule {
                column: "name".into(),
                op: FilterOp::Eq,
                value: Some(FilterValue::Single("Ada".into())),
            }],
            ..Default::default()
        };
        let target = BrowseTarget {
            driver_id: "postgres",
            schema: Some("odd\"schema"),
            table: "items",
            columns: &columns,
            filter: &filter,
        };
        let count = target.count().unwrap();
        let PageQuery::Sql(page) = target.page(10_000, 100, None, Some(&[Value::Int(50)])).unwrap() else {
            panic!("expected SQL")
        };
        assert_eq!(
            count.sql,
            "SELECT COUNT(*) FROM \"odd\"\"schema\".\"items\" WHERE \"name\" = $1"
        );
        assert!(page.sql.contains("\"name\" = $1 AND \"id\" > $2"));
        assert!(!page.sql.contains("10000"));
        assert_eq!(page.params, vec![Value::Text("Ada".into()), Value::Int(50)]);
        assert_eq!(count.params, vec![Value::Text("Ada".into())]);
    }

    #[test]
    fn unfiltered_unsorted_non_sql_fetch_is_preserved() {
        let target = BrowseTarget {
            driver_id: "mongodb",
            schema: None,
            table: "stats",
            columns: &[],
            filter: &FilterSet::default(),
        };
        assert!(matches!(target.page(0, 100, None, None).unwrap(), PageQuery::Native));
    }
}
