//! Streaming export helpers. CSV writes row-by-row without holding the
//! full result set; Parquet is stubbed until arrow/parquet deps are
//! justified by compile-time cost.

use std::io::{self, Write};

use crate::connection::Connection;
use crate::error::DriverError;
use crate::query::{ColumnInfo, Value};
use crate::sql_dialect::{build_order_and_pagination, quote_ident};

/// Escape and write a CSV header line for `columns`.
pub fn write_csv_header(w: &mut impl Write, columns: &[ColumnInfo]) -> io::Result<()> {
    for (i, col) in columns.iter().enumerate() {
        if i > 0 {
            write!(w, ",")?;
        }
        write!(w, "{}", csv_escape(&col.name))?;
    }
    writeln!(w)?;
    Ok(())
}

/// Escape and write one CSV data row.
pub fn write_csv_row(w: &mut impl Write, row: &[Value]) -> io::Result<()> {
    for (i, cell) in row.iter().enumerate() {
        if i > 0 {
            write!(w, ",")?;
        }
        write!(w, "{}", csv_escape(&value_to_csv_text(cell)))?;
    }
    writeln!(w)?;
    Ok(())
}

/// Stream an entire table to CSV by paging through `fetch_rows`.
/// Returns the number of data rows written.
pub async fn stream_table_to_csv(
    conn: &dyn Connection,
    schema: Option<&str>,
    table: &str,
    writer: &mut impl Write,
    page_size: u64,
) -> Result<u64, DriverError> {
    let page_size = page_size.max(1);
    let mut offset: u64 = 0;
    let mut total: u64 = 0;
    let mut header_written = false;

    loop {
        let page = conn.fetch_rows(schema, table, offset, page_size).await?;
        if !header_written {
            write_csv_header(writer, &page.columns).map_err(|e| DriverError::Internal(e.to_string()))?;
            header_written = true;
        }
        if page.rows.is_empty() {
            break;
        }
        for row in &page.rows {
            write_csv_row(writer, row).map_err(|e| DriverError::Internal(e.to_string()))?;
            total += 1;
        }
        if (page.rows.len() as u64) < page_size {
            break;
        }
        offset = offset.saturating_add(page_size);
    }
    Ok(total)
}

/// Stream rows from an arbitrary SELECT by re-running with LIMIT/OFFSET
/// pages. Prefer [`stream_table_to_csv`] for table browse exports.
pub async fn stream_query_to_csv(
    conn: &dyn Connection,
    driver_id: &str,
    base_sql: &str,
    writer: &mut impl Write,
    page_size: u64,
) -> Result<u64, DriverError> {
    let page_size = page_size.max(1);
    let mut offset: u64 = 0;
    let mut total: u64 = 0;
    let mut header_written = false;

    loop {
        let sql = format!(
            "{base_sql}{}",
            build_order_and_pagination(driver_id, None, page_size, offset)
        );
        let page = conn.query(&sql).await?;
        if !header_written {
            write_csv_header(writer, &page.columns).map_err(|e| DriverError::Internal(e.to_string()))?;
            header_written = true;
        }
        if page.rows.is_empty() {
            break;
        }
        for row in &page.rows {
            write_csv_row(writer, row).map_err(|e| DriverError::Internal(e.to_string()))?;
            total += 1;
        }
        if (page.rows.len() as u64) < page_size || page.truncated {
            break;
        }
        offset = offset.saturating_add(page_size);
    }
    Ok(total)
}

/// Parquet export is not wired yet (arrow/parquet inflate compile time).
pub fn export_parquet_unsupported(_path: &str) -> Result<(), DriverError> {
    Err(DriverError::Unsupported(
        "Parquet export is not implemented yet; use CSV streaming export".into(),
    ))
}

/// Qualified table name for logging / filenames.
pub fn qualified_table_name(driver_id: &str, schema: Option<&str>, table: &str) -> String {
    match schema {
        Some(s) => format!(
            "{}.{}",
            quote_ident(driver_id, s),
            quote_ident(driver_id, table)
        ),
        None => quote_ident(driver_id, table),
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn value_to_csv_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Text(s) => s.clone(),
        Value::Bytes(b) => {
            let mut s = String::with_capacity(2 + b.len() * 2);
            s.push_str("\\x");
            for byte in b {
                use std::fmt::Write as _;
                let _ = write!(s, "{byte:02x}");
            }
            s
        }
        Value::Date(d) => d.to_string(),
        Value::Time(t) => t.to_string(),
        Value::DateTime(dt) => dt.to_string(),
        Value::TimestampTz(ts) => ts.to_rfc3339(),
        Value::Decimal(d) => d.to_string(),
        Value::Uuid(u) => u.to_string(),
        Value::Json(j) => j.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_header_and_row() {
        let cols = vec![ColumnInfo {
            name: "a,b".into(),
            data_type: "text".into(),
            nullable: true,
            primary_key: false,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        }];
        let mut buf = Vec::new();
        write_csv_header(&mut buf, &cols).unwrap();
        write_csv_row(&mut buf, &[Value::Text("hello".into())]).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s, "\"a,b\"\nhello\n");
    }

    #[test]
    fn parquet_stub_errors() {
        let err = export_parquet_unsupported("/tmp/x.parquet").unwrap_err();
        assert!(matches!(err, DriverError::Unsupported(_)));
    }
}
