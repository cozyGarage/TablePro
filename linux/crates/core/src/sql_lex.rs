pub fn skip_span(rest: &str, driver_id: &str) -> Option<usize> {
    if rest.starts_with("--") {
        return Some(rest.find('\n').map_or(rest.len(), |offset| offset + 1));
    }
    if let Some(body) = rest.strip_prefix("/*") {
        return Some(body.find("*/").map_or(rest.len(), |offset| offset + 4));
    }
    if driver_id == "mysql" && rest.starts_with('#') {
        return Some(rest.find('\n').map_or(rest.len(), |offset| offset + 1));
    }
    if rest.starts_with('\'') {
        return Some(quoted_length(rest, '\'', true));
    }
    if let Some(tag_length) = dollar_quote_length(rest, driver_id) {
        return Some(tag_length);
    }
    match (driver_id, rest.as_bytes().first()) {
        ("mysql" | "clickhouse", Some(b'`')) => Some(quoted_length(rest, '`', false)),
        ("mssql", Some(b'[')) => Some(bracket_length(rest)),
        (_, Some(b'"')) => Some(quoted_length(rest, '"', true)),
        _ => None,
    }
}

fn quoted_length(rest: &str, quote: char, doubling_escapes: bool) -> usize {
    let mut characters = rest.char_indices().skip(1);
    while let Some((offset, character)) = characters.next() {
        if character != quote {
            continue;
        }
        if doubling_escapes && rest[offset + character.len_utf8()..].starts_with(quote) {
            characters.next();
            continue;
        }
        return offset + character.len_utf8();
    }
    rest.len()
}

fn bracket_length(rest: &str) -> usize {
    match rest.find(']') {
        Some(offset) => offset + 1,
        None => rest.len(),
    }
}

fn dollar_quote_length(rest: &str, driver_id: &str) -> Option<usize> {
    if driver_id != "postgres" || !rest.starts_with('$') {
        return None;
    }
    let close = rest[1..].find('$')? + 1;
    let tag = &rest[..close + 1];
    let mut tag_characters = tag[1..close].chars();
    if let Some(first) = tag_characters.next()
        && !(first.is_ascii_alphabetic() || first == '_')
    {
        return None;
    }
    if tag_characters.any(|c| !c.is_ascii_alphanumeric() && c != '_') {
        return None;
    }
    let body = &rest[close + 1..];
    match body.find(tag) {
        Some(offset) => Some(close + 1 + offset + tag.len()),
        None => Some(rest.len()),
    }
}

pub fn statement_spans(sql: &str, driver_id: &str) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let bytes = sql.as_bytes();
    let mut start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        let rest = &sql[index..];
        if let Some(length) = skip_span(rest, driver_id).filter(|length| *length > 0) {
            index += length;
            continue;
        }
        if bytes[index] == b';' {
            spans.push((start, index));
            index += 1;
            start = index;
            continue;
        }
        index += rest.chars().next().map_or(1, char::len_utf8);
    }
    spans.push((start, sql.len()));
    spans
}

pub fn split_statements(sql: &str, driver_id: &str) -> Vec<String> {
    statement_spans(sql, driver_id)
        .into_iter()
        .filter_map(|(start, end)| sql.get(start..end))
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn statement_at_cursor(sql: &str, driver_id: &str, cursor_byte: usize) -> Option<String> {
    let spans = statement_spans(sql, driver_id);
    let cursor = cursor_byte.min(sql.len());
    let pick = spans
        .iter()
        .find(|(start, end)| cursor >= *start && cursor <= *end)
        .copied()
        .or_else(|| spans.last().copied())?;
    let trimmed = sql.get(pick.0..pick.1)?.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::{split_statements, statement_at_cursor};

    #[test]
    fn a_postgres_dollar_quoted_body_stays_one_statement() {
        let sql = "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql";
        assert_eq!(split_statements(sql, "postgres"), vec![sql]);
    }

    #[test]
    fn a_tagged_dollar_quoted_body_stays_one_statement_next_to_another() {
        let sql = "CREATE FUNCTION f() AS $body$ SELECT 1; SELECT 2; $body$ LANGUAGE sql; SELECT 3";
        let split = split_statements(sql, "postgres");
        assert_eq!(split.len(), 2);
        assert!(split[0].contains("SELECT 1; SELECT 2;"));
        assert_eq!(split[1], "SELECT 3");
    }

    #[test]
    fn a_mysql_hash_comment_does_not_end_a_statement() {
        let sql = "SELECT 1 # note ; here\n; SELECT 2";
        assert_eq!(
            split_statements(sql, "mysql"),
            vec!["SELECT 1 # note ; here", "SELECT 2"]
        );
    }

    #[test]
    fn a_hash_is_not_a_comment_for_a_dialect_without_one() {
        assert_eq!(split_statements("SELECT 1 # a ; b", "postgres").len(), 2);
    }

    #[test]
    fn a_backtick_identifier_holding_a_semicolon_does_not_split() {
        let sql = "SELECT `col;name` FROM t; SELECT 2";
        assert_eq!(split_statements(sql, "mysql").len(), 2);
        assert_eq!(split_statements(sql, "clickhouse").len(), 2);
    }

    #[test]
    fn a_bracket_identifier_holding_a_semicolon_does_not_split() {
        let sql = "SELECT [col;name] FROM t; SELECT 2";
        assert_eq!(split_statements(sql, "mssql").len(), 2);
    }

    #[test]
    fn the_cursor_inside_a_dollar_quoted_body_selects_the_whole_statement() {
        let sql = "CREATE FUNCTION f() AS $$ SELECT 1; SELECT 2; $$ LANGUAGE sql; SELECT 3";
        let picked = statement_at_cursor(sql, "postgres", 30).unwrap();
        assert!(picked.starts_with("CREATE FUNCTION"));
        assert!(picked.ends_with("LANGUAGE sql"));
    }

    #[test]
    fn an_unterminated_span_keeps_the_tail_as_one_statement() {
        assert_eq!(
            split_statements("SELECT 1; SELECT 'open; SELECT 2", "postgres"),
            vec!["SELECT 1", "SELECT 'open; SELECT 2"]
        );
        assert_eq!(
            split_statements("SELECT 1; /* open ; SELECT 2", "postgres"),
            vec!["SELECT 1", "/* open ; SELECT 2"]
        );
    }
}

#[cfg(test)]
mod migrated_editor_tests {
    use super::{split_statements, statement_at_cursor};

    #[test]
    fn an_unterminated_single_quote_keeps_the_tail_as_one_statement() {
        let sql = "SELECT 1; SELECT 'open; SELECT 2";
        assert_eq!(
            split_statements(sql, "postgres"),
            vec!["SELECT 1", "SELECT 'open; SELECT 2"]
        );
    }

    #[test]
    fn an_unterminated_double_quote_keeps_the_tail_as_one_statement() {
        let sql = "SELECT 1; SELECT \"open; SELECT 2";
        assert_eq!(
            split_statements(sql, "postgres"),
            vec!["SELECT 1", "SELECT \"open; SELECT 2"]
        );
    }

    #[test]
    fn an_unterminated_block_comment_swallows_the_rest() {
        assert_eq!(
            split_statements("SELECT 1; /* open ; SELECT 2", "postgres"),
            vec!["SELECT 1", "/* open ; SELECT 2"]
        );
    }

    #[test]
    fn statement_at_cursor_survives_an_unterminated_literal() {
        let sql = "SELECT 'open; SELECT 2";
        assert_eq!(statement_at_cursor(sql, "postgres", 0).as_deref(), Some(sql));
        assert_eq!(statement_at_cursor(sql, "postgres", sql.len()).as_deref(), Some(sql));
        assert_eq!(statement_at_cursor("'", "postgres", 1).as_deref(), Some("'"));
        assert_eq!(statement_at_cursor("", "postgres", 0), None);
    }
}

#[cfg(test)]
mod migrated_editor_cursor_tests {
    use super::{split_statements, statement_at_cursor};

    #[test]
    fn splits_on_top_level_semicolons() {
        let s = split_statements("SELECT 1; SELECT 2", "postgres");
        assert_eq!(s, vec!["SELECT 1".to_string(), "SELECT 2".to_string()]);
    }

    #[test]
    fn ignores_semicolons_in_string_literals() {
        let s = split_statements("INSERT INTO t VALUES ('a;b'); SELECT 1", "postgres");
        assert_eq!(s.len(), 2);
        assert!(s[0].contains("'a;b'"));
    }

    #[test]
    fn ignores_semicolons_in_double_quotes() {
        let s = split_statements("SELECT \"col;name\" FROM t; SELECT 2", "postgres");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn ignores_semicolons_in_line_comment() {
        let s = split_statements("SELECT 1 -- comment ; here\n; SELECT 2", "postgres");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn ignores_semicolons_in_block_comment() {
        let s = split_statements("SELECT 1 /* hi ; bye */; SELECT 2", "postgres");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn trailing_semicolon_does_not_create_empty_statement() {
        let s = split_statements("SELECT 1;", "postgres");
        assert_eq!(s, vec!["SELECT 1".to_string()]);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(split_statements("", "postgres").is_empty());
        assert!(split_statements("   \n\t  ", "postgres").is_empty());
    }

    #[test]
    fn cursor_in_first_statement() {
        let sql = "SELECT 1; SELECT 2";
        let r = statement_at_cursor(sql, "postgres", 4).unwrap();
        assert_eq!(r, "SELECT 1");
    }

    #[test]
    fn cursor_in_second_statement() {
        let sql = "SELECT 1; SELECT 2";
        let r = statement_at_cursor(sql, "postgres", 17).unwrap();
        assert_eq!(r, "SELECT 2");
    }

    #[test]
    fn cursor_past_end_picks_last_statement() {
        let sql = "SELECT 1; SELECT 2";
        let r = statement_at_cursor(sql, "postgres", 9999).unwrap();
        assert_eq!(r, "SELECT 2");
    }

    #[test]
    fn cursor_on_semicolon_takes_preceding_statement() {
        let sql = "SELECT 1; SELECT 2";
        let r = statement_at_cursor(sql, "postgres", 8).unwrap();
        assert_eq!(r, "SELECT 1");
    }

    #[test]
    fn cursor_in_string_literal_with_semicolon_inside() {
        let sql = "INSERT INTO t VALUES ('a;b'); SELECT 2";
        let r = statement_at_cursor(sql, "postgres", 24).unwrap();
        assert!(r.starts_with("INSERT INTO t VALUES"));
        assert!(r.contains("'a;b'"));
    }

    #[test]
    fn cursor_in_block_comment_with_semicolon_inside() {
        let sql = "SELECT 1 /* hi ; bye */; SELECT 2";
        let r = statement_at_cursor(sql, "postgres", 16).unwrap();
        assert!(r.starts_with("SELECT 1"));
        assert!(r.contains("/* hi ; bye */"));
    }

    #[test]
    fn empty_buffer_returns_none() {
        assert!(statement_at_cursor("", "postgres", 0).is_none());
        assert!(statement_at_cursor("   \n\t  ", "postgres", 3).is_none());
    }

    #[test]
    fn multibyte_identifier_does_not_split_mid_char() {
        let sql = "SELECT \"chú_ý\" FROM t; SELECT 2";
        let r = statement_at_cursor(sql, "postgres", 0).unwrap();
        assert!(r.starts_with("SELECT"));
        assert!(r.contains("chú_ý"));
    }
}
