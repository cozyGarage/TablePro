use thiserror::Error;

use crate::query::{ColumnInfo, ForeignKeyInfo, IndexInfo};
use crate::sql_dialect::quote_ident;

#[derive(Debug, Error)]
pub enum BuildDdlError {
    #[error("table name is empty")]
    EmptyTableName,

    #[error("at least one column is required")]
    NoColumns,

    #[error("column name is empty")]
    EmptyColumnName,

    #[error("column type is empty")]
    EmptyColumnType,

    #[error("index name is empty")]
    EmptyIndexName,

    #[error("foreign key name is empty")]
    EmptyForeignKeyName,

    #[error("operation not supported by SQLite: {0}")]
    SqliteNotSupported(&'static str),

    #[error("operation not supported by Postgres: {0}")]
    PostgresNotSupported(&'static str),

    #[error("unsupported driver: {0}")]
    UnsupportedDriver(String),

    #[error("nothing changed — alter is a no-op")]
    NoChange,

    #[error("unsafe column type: {0}")]
    UnsafeType(String),

    #[error("unsafe default expression: {0}")]
    UnsafeDefault(String),

    #[error("invalid foreign key action: {0}")]
    InvalidFkAction(String),

    #[error("unsafe identifier: {0}")]
    UnsafeIdentifier(String),
}

const MAX_TYPE_LEN: usize = 200;
const MAX_DEFAULT_LEN: usize = 500;

/// Characters that some SQL drivers (notably MySQL with certain
/// client encodings) treat as effective statement terminators or
/// line breaks. ASCII LF/CR are the obvious cases; Unicode
/// `LINE SEPARATOR` (U+2028) and `PARAGRAPH SEPARATOR` (U+2029) round
/// out the set so a crafted type / default string can't smuggle a
/// newline that bypasses the comment / `;` heuristics.
const FORBIDDEN_CONTROL_CHARS: &[char] = &['\0', '\n', '\r', '\u{2028}', '\u{2029}'];

fn contains_forbidden_control(s: &str) -> bool {
    s.chars().any(|c| FORBIDDEN_CONTROL_CHARS.contains(&c))
}

/// Reject sequences that escape the type-name syntactic context into
/// statement scope (`;`, comments) or break identifier quoting (double
/// quote, backtick, NUL, line-terminators). Type names may include
/// spaces (`DOUBLE PRECISION`), parens (`VARCHAR(255)`), commas
/// (`DECIMAL(10,2)`), brackets (`INT[]`), single quotes for
/// `ENUM('a','b')`, and dots for schema-qualified user types.
pub(crate) fn validate_safe_type(s: &str) -> Result<(), BuildDdlError> {
    if s.len() > MAX_TYPE_LEN {
        return Err(BuildDdlError::UnsafeType(s.into()));
    }
    if s.contains(';')
        || s.contains("--")
        || s.contains("/*")
        || s.contains("*/")
        || s.contains('"')
        || s.contains('`')
        || contains_forbidden_control(s)
    {
        return Err(BuildDdlError::UnsafeType(s.into()));
    }
    Ok(())
}

/// DEFAULT expressions sit between `DEFAULT` and the next column-def
/// boundary (comma, paren, end of statement). The user can legitimately
/// type literals (`'foo'`, `42`), function calls (`now()`), and
/// SQL-quoted strings with embedded escapes (`'O''Brien'`). The
/// dangerous shapes are statement-terminators and SQL comments —
/// outright reject those.
pub(crate) fn validate_safe_default(s: &str) -> Result<(), BuildDdlError> {
    if s.len() > MAX_DEFAULT_LEN {
        return Err(BuildDdlError::UnsafeDefault(s.into()));
    }
    if s.contains(';') || s.contains("--") || s.contains("/*") || s.contains("*/") || contains_forbidden_control(s) {
        return Err(BuildDdlError::UnsafeDefault(s.into()));
    }
    Ok(())
}

const FK_ACTIONS: &[&str] = &["NO ACTION", "RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT"];

/// T-SQL's `ON DELETE` / `ON UPDATE` grammar has no `RESTRICT`; the
/// engine spells that behaviour `NO ACTION`. Emitting `RESTRICT` is a
/// syntax error, so it is not an option the UI may offer either.
const FK_ACTIONS_MSSQL: &[&str] = &["NO ACTION", "CASCADE", "SET NULL", "SET DEFAULT"];

/// Referential actions the engine accepts, in the order the UI should
/// present them. The first entry is the SQL default.
pub fn supported_fk_actions(driver_id: &str) -> &'static [&'static str] {
    match driver_id {
        "mssql" => FK_ACTIONS_MSSQL,
        _ => FK_ACTIONS,
    }
}

/// FK actions are a closed enum per dialect. Allow-list rather than
/// escape; case-insensitive match against the canonical strings
/// returned in upper case for emission.
pub(crate) fn validate_fk_action(driver_id: &str, s: &str) -> Result<&'static str, BuildDdlError> {
    let upper = s.trim().to_ascii_uppercase();
    supported_fk_actions(driver_id)
        .iter()
        .copied()
        .find(|canon| *canon == upper.as_str())
        .ok_or_else(|| BuildDdlError::InvalidFkAction(s.into()))
}

/// User-edited column draft. Carries both the original (loaded from
/// `fetch_columns`) and the in-flight edit. `original` is `None` for
/// newly-added columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftColumn {
    pub original: Option<ColumnInfo>,
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub primary_key: bool,
    pub auto_increment: bool,
    pub default_value: Option<String>,
}

impl DraftColumn {
    /// Build a `DraftColumn` from a `ColumnInfo` returned by
    /// `fetch_columns` so the user starts with the live state and
    /// edits diff against `original`.
    pub fn from_info(info: ColumnInfo) -> Self {
        let data_type = info.data_type.clone();
        let nullable = info.nullable;
        let primary_key = info.primary_key;
        let auto_increment = info.is_auto_increment;
        let default_value = info.default_value.clone();
        let name = info.name.clone();
        Self {
            original: Some(info),
            name,
            data_type,
            nullable,
            primary_key,
            auto_increment,
            default_value,
        }
    }

    /// True when any of the user-editable attributes differ from the
    /// loaded original. New columns (`original = None`) always count
    /// as different. Used by the diff path to decide whether the
    /// column needs an `AlterColumn` op.
    pub fn differs_from_original(&self) -> bool {
        match &self.original {
            None => true,
            Some(orig) => {
                orig.name != self.name
                    || orig.data_type != self.data_type
                    || orig.nullable != self.nullable
                    || orig.primary_key != self.primary_key
                    || orig.is_auto_increment != self.auto_increment
                    || orig.default_value != self.default_value
            }
        }
    }
}

/// Pending DDL operation, produced by the diff between the loaded
/// snapshot of a table's structure and the user's in-flight edits.
/// `materialize_ops` consumes these into ordered SQL statements.
///
/// Identity-bearing fields (`schema`, `table`, name fields) are
/// captured at op-build time; `materialize_ops` doesn't reach back
/// into the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructureOp {
    /// Whole-table create — emitted by `New` mode where the user is
    /// drafting a fresh table. `Edit` mode never produces this op.
    CreateTable {
        schema: Option<String>,
        table: String,
        columns: Vec<DraftColumn>,
        indexes: Vec<IndexInfo>,
        fks: Vec<ForeignKeyInfo>,
    },
    RenameTable {
        schema: Option<String>,
        old_name: String,
        new_name: String,
    },
    RenameColumn {
        schema: Option<String>,
        table: String,
        old_name: String,
        new_name: String,
    },
    AddColumn {
        schema: Option<String>,
        table: String,
        column: DraftColumn,
    },
    DropColumn {
        schema: Option<String>,
        table: String,
        column_name: String,
    },
    /// Single op for any combination of name / type / nullable /
    /// default / pk / auto-increment changes on one column. Driver
    /// dialect decides how it's split (MySQL: one MODIFY COLUMN;
    /// Postgres / SQLite: per-attribute statements).
    AlterColumn {
        schema: Option<String>,
        table: String,
        column: DraftColumn,
    },
    AddIndex {
        schema: Option<String>,
        table: String,
        index: IndexInfo,
    },
    DropIndex {
        schema: Option<String>,
        table: String,
        index_name: String,
    },
    AddForeignKey {
        schema: Option<String>,
        table: String,
        fk: ForeignKeyInfo,
    },
    DropForeignKey {
        schema: Option<String>,
        table: String,
        fk_name: String,
    },
}

pub(crate) fn qualified_table(driver_id: &str, schema: Option<&str>, table: &str) -> String {
    // Trim leading / trailing whitespace before quoting so a user
    // who typed `" users "` doesn't end up with a literally
    // space-padded identifier in the generated DDL. The validator
    // rejects after-trim-empty separately; here we only protect
    // against accidental padding surviving into the SQL.
    let table = table.trim();
    match schema.map(str::trim) {
        Some(s) if !s.is_empty() => format!("{}.{}", quote_ident(driver_id, s), quote_ident(driver_id, table)),
        _ => quote_ident(driver_id, table),
    }
}

/// Escape a value for inclusion in a SQL string literal. Bracket
/// quoting escapes `]`, not `'`, so any identifier that travels as a
/// literal (`sp_rename`'s arguments, an `OBJECT_ID()` lookup) needs
/// this on top of, or instead of, `quote_ident`.
pub(crate) fn sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

/// Drop the default constraint bound to `column`, if any. SQL Server
/// generates the constraint name, and `DROP CONSTRAINT` does not accept
/// a variable, so the name is resolved from `sys.default_constraints`
/// and applied through `EXEC`. Emitted as one batch because the
/// variable does not outlive it.
pub(crate) fn mssql_drop_default_constraint(
    driver_id: &str,
    schema: Option<&str>,
    table: &str,
    column: &str,
) -> String {
    let qualified = qualified_table(driver_id, schema, table);
    let table_literal = sql_literal(&qualified);
    let column_literal = sql_literal(column.trim());
    format!(
        "DECLARE @default_constraint sysname = (\
SELECT dc.name FROM sys.default_constraints dc \
JOIN sys.columns c ON c.object_id = dc.parent_object_id AND c.column_id = dc.parent_column_id \
WHERE dc.parent_object_id = OBJECT_ID('{table_literal}') AND c.name = '{column_literal}'); \
IF @default_constraint IS NOT NULL \
EXEC('ALTER TABLE {table_literal} DROP CONSTRAINT [' + @default_constraint + ']')"
    )
}

pub(crate) fn validate_table(table: &str) -> Result<(), BuildDdlError> {
    match crate::sql_dialect::validate_ident(table) {
        Ok(()) => Ok(()),
        Err(crate::sql_dialect::IdentError::Empty) => Err(BuildDdlError::EmptyTableName),
        Err(_) => Err(BuildDdlError::UnsafeIdentifier(table.into())),
    }
}

pub(crate) fn validate_column_name(name: &str) -> Result<(), BuildDdlError> {
    match crate::sql_dialect::validate_ident(name) {
        Ok(()) => Ok(()),
        Err(crate::sql_dialect::IdentError::Empty) => Err(BuildDdlError::EmptyColumnName),
        Err(_) => Err(BuildDdlError::UnsafeIdentifier(name.into())),
    }
}

pub(crate) fn validate_column_type(data_type: &str) -> Result<(), BuildDdlError> {
    if data_type.trim().is_empty() {
        return Err(BuildDdlError::EmptyColumnType);
    }
    validate_safe_type(data_type)?;
    Ok(())
}

pub(crate) fn validated_default(default: Option<&str>) -> Result<Option<&str>, BuildDdlError> {
    let Some(d) = default.filter(|d| !d.is_empty()) else {
        return Ok(None);
    };
    validate_safe_default(d)?;
    Ok(Some(d))
}

/// Render one inline column definition for a CREATE TABLE statement.
/// PK is rendered inline only for single-column PK; composite PKs are
/// emitted as a table-level constraint by the caller.
pub(crate) fn render_column_definition(
    driver_id: &str,
    column: &DraftColumn,
    inline_pk: bool,
) -> Result<String, BuildDdlError> {
    validate_column_name(&column.name)?;
    validate_column_type(&column.data_type)?;
    let mut parts = vec![quote_ident(driver_id, &column.name), column.data_type.clone()];

    // SQLite: INTEGER PRIMARY KEY (with optional AUTOINCREMENT) is
    // the canonical rowid alias and is its own paragraph in the
    // grammar. Render that pattern when the user asked for inline PK
    // on a single integer column. AUTOINCREMENT is opt-in (it adds
    // monotonic-id guarantees + sqlite_sequence overhead).
    if driver_id == "sqlite" && inline_pk && column.primary_key {
        parts.push("PRIMARY KEY".into());
        if column.auto_increment {
            parts.push("AUTOINCREMENT".into());
        }
        if !column.nullable {
            parts.push("NOT NULL".into());
        }
        if let Some(default) = validated_default(column.default_value.as_deref())? {
            parts.push(format!("DEFAULT {default}"));
        }
        return Ok(parts.join(" "));
    }

    // Postgres SERIAL / BIGSERIAL when auto_increment is requested on
    // an integer column. SERIAL implies NOT NULL + a sequence default,
    // so don't emit those redundantly. The user-typed type is
    // overridden because `serial` IS the type for that pseudo-pattern.
    if driver_id == "postgres" && column.auto_increment {
        let lower = column.data_type.to_ascii_lowercase();
        let serial_type = if lower.contains("bigint") || lower.contains("int8") {
            "BIGSERIAL"
        } else if lower.contains("smallint") || lower.contains("int2") {
            "SMALLSERIAL"
        } else {
            "SERIAL"
        };
        parts = vec![quote_ident(driver_id, &column.name), serial_type.into()];
        if inline_pk && column.primary_key {
            parts.push("PRIMARY KEY".into());
        }
        return Ok(parts.join(" "));
    }

    // MSSQL IDENTITY(1,1) auto-increment. An identity column cannot
    // carry a DEFAULT, so this bypasses the generic tail entirely —
    // unlike Postgres SERIAL, MSSQL identity still honors the user's
    // NOT NULL choice instead of forcing one implicitly.
    if driver_id == "mssql" && column.auto_increment {
        parts.push("IDENTITY(1,1)".into());
        if !column.nullable {
            parts.push("NOT NULL".into());
        }
        if inline_pk && column.primary_key {
            parts.push("PRIMARY KEY".into());
        }
        return Ok(parts.join(" "));
    }

    if !column.nullable {
        parts.push("NOT NULL".into());
    }
    if let Some(default) = validated_default(column.default_value.as_deref())? {
        parts.push(format!("DEFAULT {default}"));
    }

    if driver_id == "mysql" && column.auto_increment {
        parts.push("AUTO_INCREMENT".into());
    }

    if inline_pk && column.primary_key {
        parts.push("PRIMARY KEY".into());
    }

    Ok(parts.join(" "))
}
