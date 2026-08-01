use relm4::gtk;
use relm4::gtk::prelude::*;

pub(crate) fn toggle_line_comment(buffer: &gtk::TextBuffer) {
    let (sel_start, sel_end) = buffer.selection_bounds().unwrap_or_else(|| {
        let i = buffer.iter_at_mark(&buffer.get_insert());
        (i, i)
    });
    let start_line = sel_start.line();
    let mut end_line = sel_end.line();
    if sel_end.line_offset() == 0 && end_line > start_line {
        end_line -= 1;
    }

    let lines: Vec<String> = (start_line..=end_line)
        .map(|l| {
            let Some(s) = buffer.iter_at_line(l) else {
                return String::new();
            };
            let mut e = s;
            e.forward_to_line_end();
            buffer.text(&s, &e, false).to_string()
        })
        .collect();

    let all_commented = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .all(|l| l.trim_start().starts_with("--"));

    buffer.begin_user_action();
    for (offset, original) in lines.iter().enumerate() {
        if original.trim().is_empty() {
            continue;
        }
        let line_n = start_line + offset as i32;
        let leading_chars: i32 = original.chars().take_while(|c| c.is_whitespace()).count() as i32;
        let Some(mut iter) = buffer.iter_at_line(line_n) else {
            continue;
        };
        iter.forward_chars(leading_chars);

        if all_commented {
            let trimmed = original.trim_start();
            let strip_chars: i32 = if trimmed.starts_with("-- ") {
                3
            } else if trimmed.starts_with("--") {
                2
            } else {
                0
            };
            if strip_chars > 0 {
                let mut end = iter;
                end.forward_chars(strip_chars);
                buffer.delete(&mut iter, &mut end);
            }
        } else {
            buffer.insert(&mut iter, "-- ");
        }
    }
    buffer.end_user_action();
}

pub(crate) fn statement_at_cursor(sql: &str, cursor_byte: usize) -> Option<String> {
    let mut segments: Vec<(usize, usize)> = Vec::new();
    let mut seg_start = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut chars = sql.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if in_line_comment {
            if c == '\n' {
                in_line_comment = false;
            }
            continue;
        }
        if in_block_comment {
            if c == '*'
                && let Some(&(_, '/')) = chars.peek()
            {
                chars.next();
                in_block_comment = false;
            }
            continue;
        }
        if !in_single && !in_double {
            if c == '-'
                && let Some(&(_, '-')) = chars.peek()
            {
                chars.next();
                in_line_comment = true;
                continue;
            }
            if c == '/'
                && let Some(&(_, '*')) = chars.peek()
            {
                chars.next();
                in_block_comment = true;
                continue;
            }
        }
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ';' if !in_single && !in_double => {
                segments.push((seg_start, i));
                seg_start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    segments.push((seg_start, sql.len()));
    let cursor = cursor_byte.min(sql.len());
    let pick = segments
        .iter()
        .find(|(start, end)| cursor >= *start && cursor <= *end)
        .copied()
        .or_else(|| segments.last().copied())?;
    let trimmed = sql.get(pick.0..pick.1)?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    while let Some(c) = chars.next() {
        if in_line_comment {
            current.push(c);
            if c == '\n' {
                in_line_comment = false;
            }
            continue;
        }
        if in_block_comment {
            current.push(c);
            if c == '*' && chars.peek() == Some(&'/') {
                current.push(chars.next().unwrap());
                in_block_comment = false;
            }
            continue;
        }
        if !in_single && !in_double {
            if c == '-' && chars.peek() == Some(&'-') {
                current.push(c);
                current.push(chars.next().unwrap());
                in_line_comment = true;
                continue;
            }
            if c == '/' && chars.peek() == Some(&'*') {
                current.push(c);
                current.push(chars.next().unwrap());
                in_block_comment = true;
                continue;
            }
        }
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ';' if !in_single && !in_double => {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    out.push(trimmed);
                }
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        out.push(trimmed);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{split_sql_statements, statement_at_cursor};

    #[test]
    fn splits_on_top_level_semicolons() {
        let s = split_sql_statements("SELECT 1; SELECT 2");
        assert_eq!(s, vec!["SELECT 1".to_string(), "SELECT 2".to_string()]);
    }

    #[test]
    fn ignores_semicolons_in_string_literals() {
        let s = split_sql_statements("INSERT INTO t VALUES ('a;b'); SELECT 1");
        assert_eq!(s.len(), 2);
        assert!(s[0].contains("'a;b'"));
    }

    #[test]
    fn ignores_semicolons_in_double_quotes() {
        let s = split_sql_statements("SELECT \"col;name\" FROM t; SELECT 2");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn ignores_semicolons_in_line_comment() {
        let s = split_sql_statements("SELECT 1 -- comment ; here\n; SELECT 2");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn ignores_semicolons_in_block_comment() {
        let s = split_sql_statements("SELECT 1 /* hi ; bye */; SELECT 2");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn trailing_semicolon_does_not_create_empty_statement() {
        let s = split_sql_statements("SELECT 1;");
        assert_eq!(s, vec!["SELECT 1".to_string()]);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(split_sql_statements("").is_empty());
        assert!(split_sql_statements("   \n\t  ").is_empty());
    }

    #[test]
    fn cursor_in_first_statement() {
        let sql = "SELECT 1; SELECT 2";
        let r = statement_at_cursor(sql, 4).unwrap();
        assert_eq!(r, "SELECT 1");
    }

    #[test]
    fn cursor_in_second_statement() {
        let sql = "SELECT 1; SELECT 2";
        let r = statement_at_cursor(sql, 17).unwrap();
        assert_eq!(r, "SELECT 2");
    }

    #[test]
    fn cursor_past_end_picks_last_statement() {
        let sql = "SELECT 1; SELECT 2";
        let r = statement_at_cursor(sql, 9999).unwrap();
        assert_eq!(r, "SELECT 2");
    }

    #[test]
    fn cursor_on_semicolon_takes_preceding_statement() {
        let sql = "SELECT 1; SELECT 2";
        let r = statement_at_cursor(sql, 8).unwrap();
        assert_eq!(r, "SELECT 1");
    }

    #[test]
    fn cursor_in_string_literal_with_semicolon_inside() {
        let sql = "INSERT INTO t VALUES ('a;b'); SELECT 2";
        let r = statement_at_cursor(sql, 24).unwrap();
        assert!(r.starts_with("INSERT INTO t VALUES"));
        assert!(r.contains("'a;b'"));
    }

    #[test]
    fn cursor_in_block_comment_with_semicolon_inside() {
        let sql = "SELECT 1 /* hi ; bye */; SELECT 2";
        let r = statement_at_cursor(sql, 16).unwrap();
        assert!(r.starts_with("SELECT 1"));
        assert!(r.contains("/* hi ; bye */"));
    }

    #[test]
    fn empty_buffer_returns_none() {
        assert!(statement_at_cursor("", 0).is_none());
        assert!(statement_at_cursor("   \n\t  ", 3).is_none());
    }

    #[test]
    fn multibyte_identifier_does_not_split_mid_char() {
        let sql = "SELECT \"chú_ý\" FROM t; SELECT 2";
        let r = statement_at_cursor(sql, 0).unwrap();
        assert!(r.starts_with("SELECT"));
        assert!(r.contains("chú_ý"));
    }
}
