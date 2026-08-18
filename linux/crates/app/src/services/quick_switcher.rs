use tablepro_storage::SavedQuery;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickTarget {
    Favorite(Uuid),
    Tab(Uuid),
    Connection(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickItem {
    pub target: QuickTarget,
    pub title: String,
    pub subtitle: String,
}

pub fn favorite_items(favorites: &[SavedQuery]) -> Vec<QuickItem> {
    tablepro_storage::rank_favorites(favorites)
        .into_iter()
        .map(|favorite| QuickItem {
            target: QuickTarget::Favorite(favorite.id),
            title: favorite.name.clone(),
            subtitle: one_line(&favorite.sql),
        })
        .collect()
}

pub fn filter(items: &[QuickItem], needle: &str) -> Vec<QuickItem> {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return items.to_vec();
    }
    let mut scored: Vec<(u32, usize, QuickItem)> = items
        .iter()
        .enumerate()
        .filter_map(|(position, item)| score(item, &needle).map(|score| (score, position, item.clone())))
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    scored.into_iter().map(|(_, _, item)| item).collect()
}

fn score(item: &QuickItem, needle: &str) -> Option<u32> {
    let title = item.title.to_lowercase();
    if title == needle {
        return Some(100);
    }
    if title.starts_with(needle) {
        return Some(80);
    }
    if title.contains(needle) {
        return Some(60);
    }
    if item.subtitle.to_lowercase().contains(needle) {
        return Some(40);
    }
    is_subsequence(&title, needle).then_some(20)
}

fn is_subsequence(haystack: &str, needle: &str) -> bool {
    let mut characters = haystack.chars();
    needle
        .chars()
        .all(|wanted| characters.any(|candidate| candidate == wanted))
}

fn one_line(sql: &str) -> String {
    let collapsed = sql.split_whitespace().collect::<Vec<&str>>().join(" ");
    let mut out: String = collapsed.chars().take(80).collect();
    if collapsed.chars().count() > 80 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, subtitle: &str) -> QuickItem {
        QuickItem {
            target: QuickTarget::Favorite(Uuid::new_v4()),
            title: title.to_string(),
            subtitle: subtitle.to_string(),
        }
    }

    #[test]
    fn an_empty_needle_keeps_the_incoming_order() {
        let items = vec![item("zeta", ""), item("alpha", "")];
        assert_eq!(filter(&items, "  "), items);
    }

    #[test]
    fn exact_and_prefix_matches_rank_above_substring_matches() {
        let items = vec![
            item("revenue by month", ""),
            item("daily", ""),
            item("daily revenue", ""),
        ];
        let filtered = filter(&items, "daily");
        assert_eq!(filtered[0].title, "daily");
        assert_eq!(filtered[1].title, "daily revenue");
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn a_statement_match_ranks_below_a_title_match() {
        let items = vec![
            item("nightly job", "SELECT * FROM orders"),
            item("orders overview", "SELECT 1"),
        ];
        let filtered = filter(&items, "orders");
        assert_eq!(filtered[0].title, "orders overview");
        assert_eq!(filtered[1].title, "nightly job");
    }

    #[test]
    fn initials_match_as_a_subsequence() {
        let items = vec![item("daily revenue export", "")];
        assert_eq!(filter(&items, "dre").len(), 1);
        assert!(filter(&items, "zzz").is_empty());
    }

    #[test]
    fn matching_ignores_case() {
        let items = vec![item("Daily Revenue", "")];
        assert_eq!(filter(&items, "DAILY").len(), 1);
    }

    #[test]
    fn favorite_items_use_recency_order_and_a_single_line_statement() {
        let mut recent = SavedQuery::new("zeta".into(), "SELECT\n  1".into(), None, None);
        recent.last_used_at = Some(chrono::Utc::now());
        let older = SavedQuery::new("alpha".into(), "SELECT 2".into(), None, None);

        let items = favorite_items(&[older, recent]);

        assert_eq!(items[0].title, "zeta");
        assert_eq!(items[0].subtitle, "SELECT 1");
        assert_eq!(items[1].title, "alpha");
    }

    #[test]
    fn long_statements_are_truncated_for_display() {
        let sql = "SELECT ".to_string() + &"column_name, ".repeat(20);
        let favorite = SavedQuery::new("wide".into(), sql, None, None);
        let items = favorite_items(&[favorite]);
        assert!(items[0].subtitle.ends_with('…'));
        assert!(items[0].subtitle.chars().count() <= 81);
    }
}
