//! Streaming export helpers. CSV writes row-by-row without holding the
//! full result set; Parquet is stubbed until arrow/parquet deps are
//! justified by compile-time cost.

#[cfg(test)]
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;

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

/// Legacy best-effort full-table CSV paging helper.
///
/// This does not establish a database snapshot or stable row order. It is not
/// used by the RC GUI; callers must provide those guarantees themselves before
/// presenting its output as a consistent full-table export.
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
/// pages. This legacy helper does not establish a snapshot or stable order;
/// the GUI exports its already loaded result instead.
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

/// Write `path` so a reader never observes a half-written export. The
/// content goes to a sibling temporary file, is flushed and synced, then
/// renamed over the destination, which is atomic within a filesystem.
/// Writing straight to the destination meant another process could open
/// the file after the header and a few rows had reached the disk and read
/// a truncated export.
pub fn write_atomically<F>(path: &Path, fill: F) -> io::Result<()>
where
    F: FnOnce(&mut dyn Write) -> io::Result<()>,
{
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(".tablepro-export-")
        .tempfile_in(directory)?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        fill(&mut writer)?;
        writer.flush()?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
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
        Some(s) => format!("{}.{}", quote_ident(driver_id, s), quote_ident(driver_id, table)),
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
mod atomic_write_tests {
    use super::*;

    #[test]
    fn overlapping_exports_publish_complete_independent_files() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("page.csv");
        fs::write(&path, "original").expect("existing export");

        write_atomically(&path, |outer| {
            outer.write_all(b"outer export")?;
            write_atomically(&path, |inner| inner.write_all(b"inner"))?;
            assert_eq!(fs::read_to_string(&path)?, "inner");
            Ok(())
        })
        .expect("both exports complete");

        assert_eq!(fs::read_to_string(&path).expect("read export"), "outer export");
        assert_eq!(fs::read_dir(directory.path()).expect("list").count(), 1);
    }

    #[test]
    fn a_failed_export_preserves_an_existing_destination() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("page.csv");
        fs::write(&path, "original").expect("existing export");
        assert!(
            write_atomically(&path, |writer| {
                writer.write_all(b"replacement")?;
                Err(io::Error::other("export failed"))
            })
            .is_err()
        );
        assert_eq!(fs::read_to_string(&path).expect("read export"), "original");
        assert_eq!(fs::read_dir(directory.path()).expect("list").count(), 1);
    }

    #[test]
    fn export_staging_stays_on_the_destination_filesystem() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("page.csv");
        write_atomically(&path, |writer| {
            // Atomic rename requires staging beside the destination, even
            // when the destination is on a different mount from the cwd.
            assert_eq!(fs::read_dir(directory.path())?.count(), 1);
            assert!(!path.exists());
            writer.write_all(b"complete export")
        })
        .expect("export");
        assert_eq!(fs::read_to_string(path).expect("read export"), "complete export");
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_temporary_symlink_cannot_redirect_an_export() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("page.csv");
        let unrelated = directory.path().join("unrelated.txt");
        fs::write(&unrelated, "keep me").expect("unrelated file");
        std::os::unix::fs::symlink(&unrelated, directory.path().join("page.csv.tablepro-part"))
            .expect("preexisting symlink");

        write_atomically(&path, |writer| writer.write_all(b"export")).expect("export");

        assert_eq!(fs::read_to_string(&unrelated).expect("unrelated file"), "keep me");
        assert_eq!(fs::read_to_string(&path).expect("export file"), "export");
        assert!(!path.is_symlink());
    }

    #[test]
    fn a_completed_write_lands_at_the_destination_and_leaves_no_leftovers() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("page.csv");

        write_atomically(&path, |writer| writer.write_all(b"id\n1\n2\n")).expect("write the export");

        assert_eq!(fs::read_to_string(&path).expect("read back"), "id\n1\n2\n");
        assert_eq!(
            fs::read_dir(directory.path()).expect("list").count(),
            1,
            "the temporary file must not survive"
        );
    }

    #[test]
    fn the_destination_never_exists_while_the_export_is_still_being_written() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("page.csv");
        let observed = std::cell::Cell::new(false);

        write_atomically(&path, |writer| {
            writer.write_all(b"id\n")?;
            for row in 0..500 {
                writeln!(writer, "{row}")?;
            }
            observed.set(path.exists());
            Ok(())
        })
        .expect("write the export");

        assert!(
            !observed.get(),
            "a reader must not be able to open a partially written export"
        );
        assert!(path.exists(), "the finished export must exist");
    }

    #[test]
    fn a_failed_write_leaves_neither_the_destination_nor_a_temporary_file() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("page.csv");

        let error = write_atomically(&path, |writer| {
            writer.write_all(b"id\n")?;
            Err(io::Error::other("driver stopped mid-export"))
        })
        .expect_err("the failure must be reported");

        assert_eq!(error.to_string(), "driver stopped mid-export");
        assert!(!path.exists(), "a failed export must not leave a destination file");
        assert_eq!(
            fs::read_dir(directory.path()).expect("list").count(),
            0,
            "a failed export must not leave a temporary file"
        );
    }

    #[test]
    fn a_second_export_replaces_the_first_without_a_partial_state() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("page.csv");

        write_atomically(&path, |writer| writer.write_all(b"first")).expect("first export");
        write_atomically(&path, |writer| writer.write_all(b"second")).expect("second export");

        assert_eq!(fs::read_to_string(&path).expect("read back"), "second");
        assert_eq!(fs::read_dir(directory.path()).expect("list").count(), 1);
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
