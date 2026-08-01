use crate::sql_dialect::quote_ident;

use super::types::{
    BuildDdlError, DraftColumn, mssql_drop_default_constraint, qualified_table, render_column_definition, sql_literal,
    validate_column_name, validate_safe_type, validate_table, validated_default,
};

pub fn build_add_column(
    driver_id: &str,
    schema: Option<&str>,
    table: &str,
    column: &DraftColumn,
) -> Result<String, BuildDdlError> {
    validate_table(table)?;
    let column_def = render_column_definition(driver_id, column, false)?;
    if driver_id == "sqlite" && !column.nullable && column.default_value.as_deref().unwrap_or("").is_empty() {
        // SQLite refuses ADD COLUMN NOT NULL unless the column has a
        // DEFAULT (or is a generated column we don't yet handle).
        // Surface the limit at build time so the UI can show a clear
        // error before sending the statement to the driver.
        return Err(BuildDdlError::SqliteNotSupported("ADD COLUMN NOT NULL without DEFAULT"));
    }
    let keyword = if driver_id == "mssql" { "ADD" } else { "ADD COLUMN" };
    Ok(format!(
        "ALTER TABLE {} {} {}",
        qualified_table(driver_id, schema, table),
        keyword,
        column_def
    ))
}

pub fn build_drop_column(
    driver_id: &str,
    schema: Option<&str>,
    table: &str,
    column_name: &str,
) -> Result<String, BuildDdlError> {
    validate_table(table)?;
    validate_column_name(column_name)?;
    if driver_id == "sqlite" {
        // SQLite added DROP COLUMN in 3.35; we trust the runtime
        // SQLite to enforce. The UI disables the affordance for
        // older runtimes via the same path, but this builder doesn't
        // version-detect — the error surfaces from the driver if the
        // version is too old.
    }
    Ok(format!(
        "ALTER TABLE {} DROP COLUMN {}",
        qualified_table(driver_id, schema, table),
        quote_ident(driver_id, column_name)
    ))
}

pub fn build_rename_column(
    driver_id: &str,
    schema: Option<&str>,
    table: &str,
    old_name: &str,
    new_name: &str,
) -> Result<String, BuildDdlError> {
    validate_table(table)?;
    validate_column_name(old_name)?;
    validate_column_name(new_name)?;
    if driver_id == "mssql" {
        // Same string-literal escaping requirement as build_rename_table.
        let object_name = sql_literal(&format!(
            "{}.{}",
            qualified_table(driver_id, schema, table),
            quote_ident(driver_id, old_name)
        ));
        let new_bare = sql_literal(new_name.trim());
        return Ok(format!("EXEC sp_rename '{}', '{}', 'COLUMN'", object_name, new_bare));
    }
    Ok(format!(
        "ALTER TABLE {} RENAME COLUMN {} TO {}",
        qualified_table(driver_id, schema, table),
        quote_ident(driver_id, old_name),
        quote_ident(driver_id, new_name)
    ))
}

/// Apply column type / nullable / default changes. Returns one or
/// more SQL statements:
///
/// - **MySQL**: a single `ALTER TABLE ... MODIFY COLUMN col_def` that
///   replaces the whole definition.
/// - **Postgres**: one `ALTER TABLE ... ALTER COLUMN ...` per
///   attribute that diffed against `column.original`. Returning a Vec
///   means a single `AlterColumn` op carrying simultaneous type +
///   nullable + default changes maps to up to three statements; the
///   previous single-`String` return cascaded through early-return
///   guards and silently dropped all but the first changed attribute.
/// - **SQLite**: not supported; returns `SqliteNotSupported`.
pub fn build_alter_column(
    driver_id: &str,
    schema: Option<&str>,
    table: &str,
    column: &DraftColumn,
) -> Result<Vec<String>, BuildDdlError> {
    validate_table(table)?;
    validate_column_name(&column.name)?;
    let qualified = qualified_table(driver_id, schema, table);
    match driver_id {
        "mysql" => {
            // MySQL's MODIFY COLUMN replaces the whole column
            // definition. Render the column inline (without inline-PK
            // since MODIFY can't change PK) and emit.
            let column_def = render_column_definition(driver_id, column, false)?;
            Ok(vec![format!("ALTER TABLE {} MODIFY COLUMN {}", qualified, column_def)])
        }
        "postgres" => {
            // Postgres needs separate sub-statements per attribute.
            // Build all that changed and join with `;` so the single
            // returned string carries every change. The caller passes
            // the result to `Connection::execute` which splits on `;`
            // and runs each as a separate statement, matching how
            // MySQL's MODIFY COLUMN coalesces several changes into
            // one wire-level command. Previously this builder
            // returned only the first changed attribute (type wins
            // over nullable wins over default), silently losing the
            // user's other edits when more than one attribute moved.
            let original = column.original.as_ref();
            let type_changed = original.map(|o| o.data_type != column.data_type).unwrap_or(true);
            let nullable_changed = original.map(|o| o.nullable != column.nullable).unwrap_or(false);
            let default_changed = original
                .map(|o| o.default_value.as_deref() != column.default_value.as_deref())
                .unwrap_or(column.default_value.is_some());
            let mut stmts: Vec<String> = Vec::new();
            if type_changed {
                validate_safe_type(&column.data_type)?;
                stmts.push(format!(
                    "ALTER TABLE {} ALTER COLUMN {} TYPE {} USING {}::{}",
                    qualified,
                    quote_ident(driver_id, &column.name),
                    column.data_type,
                    quote_ident(driver_id, &column.name),
                    column.data_type,
                ));
            }
            if nullable_changed {
                stmts.push(if column.nullable {
                    format!(
                        "ALTER TABLE {} ALTER COLUMN {} DROP NOT NULL",
                        qualified,
                        quote_ident(driver_id, &column.name)
                    )
                } else {
                    format!(
                        "ALTER TABLE {} ALTER COLUMN {} SET NOT NULL",
                        qualified,
                        quote_ident(driver_id, &column.name)
                    )
                });
            }
            if default_changed {
                stmts.push(match validated_default(column.default_value.as_deref())? {
                    Some(default) => format!(
                        "ALTER TABLE {} ALTER COLUMN {} SET DEFAULT {}",
                        qualified,
                        quote_ident(driver_id, &column.name),
                        default
                    ),
                    None => format!(
                        "ALTER TABLE {} ALTER COLUMN {} DROP DEFAULT",
                        qualified,
                        quote_ident(driver_id, &column.name)
                    ),
                });
            }
            if stmts.is_empty() {
                // Nothing actually changed — surface as NoChange so
                // the caller can skip emission.
                return Err(BuildDdlError::NoChange);
            }
            Ok(stmts)
        }
        "mssql" => {
            let original = column.original.as_ref();
            let type_changed = original.map(|o| o.data_type != column.data_type).unwrap_or(true);
            let nullable_changed = original.map(|o| o.nullable != column.nullable).unwrap_or(false);
            let default_changed = original
                .map(|o| o.default_value.as_deref() != column.default_value.as_deref())
                .unwrap_or(column.default_value.is_some());
            let mut stmts: Vec<String> = Vec::new();
            // ALTER COLUMN carries type and nullability together: T-SQL
            // reads an omitted NULL / NOT NULL as NULL, so a type-only
            // change has to restate the nullability or it would silently
            // drop NOT NULL.
            if type_changed || nullable_changed {
                validate_safe_type(&column.data_type)?;
                let nullability = if column.nullable { "NULL" } else { "NOT NULL" };
                stmts.push(format!(
                    "ALTER TABLE {} ALTER COLUMN {} {} {}",
                    qualified,
                    quote_ident(driver_id, &column.name),
                    column.data_type,
                    nullability
                ));
            }
            if default_changed {
                // A default is a separate named constraint here, not a
                // column attribute, so changing one is drop-then-add.
                // The existing constraint's name is generated by the
                // server, so the drop resolves it from the catalog.
                stmts.push(mssql_drop_default_constraint(driver_id, schema, table, &column.name));
                if let Some(default) = validated_default(column.default_value.as_deref())? {
                    stmts.push(format!(
                        "ALTER TABLE {} ADD DEFAULT ({}) FOR {}",
                        qualified,
                        default,
                        quote_ident(driver_id, &column.name)
                    ));
                }
            }
            if stmts.is_empty() {
                return Err(BuildDdlError::NoChange);
            }
            Ok(stmts)
        }
        "sqlite" => Err(BuildDdlError::SqliteNotSupported(
            "ALTER COLUMN (type / nullable / default change)",
        )),
        other => Err(BuildDdlError::UnsupportedDriver(other.to_string())),
    }
}

/// MySQL-only column reorder. Emits `MODIFY COLUMN ... AFTER other`
/// or `MODIFY COLUMN ... FIRST` when `after` is `None`.
pub fn build_reorder_column(
    driver_id: &str,
    schema: Option<&str>,
    table: &str,
    column: &DraftColumn,
    after: Option<&str>,
) -> Result<String, BuildDdlError> {
    validate_table(table)?;
    validate_column_name(&column.name)?;
    if driver_id != "mysql" {
        return Err(BuildDdlError::UnsupportedDriver(driver_id.to_string()));
    }
    let column_def = render_column_definition(driver_id, column, false)?;
    let position = match after {
        Some(name) if !name.is_empty() => format!("AFTER {}", quote_ident(driver_id, name)),
        _ => "FIRST".to_string(),
    };
    Ok(format!(
        "ALTER TABLE {} MODIFY COLUMN {} {}",
        qualified_table(driver_id, schema, table),
        column_def,
        position,
    ))
}
