//! Proactive ranking: score and rank items by relevance.

use crate::extract::ProactiveItem;

/// Rank by priority desc, then title for stability.
pub fn rank(items: &mut [ProactiveItem]) {
    items.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.title.cmp(&b.title))
    });
}

pub fn top_n(items: &mut Vec<ProactiveItem>, n: usize) -> Vec<ProactiveItem> {
    rank(items);
    items.drain(..n.min(items.len())).collect()
}

/// Keyword boost: +2 priority (capped 255) when query tokens hit title/description/action.
pub fn rank_with_query(items: &mut [ProactiveItem], query: &str) {
    let tokens: Vec<String> = query
        .split_whitespace()
        .filter(|t| t.len() > 1)
        .map(|t| t.to_ascii_lowercase())
        .collect();
    if !tokens.is_empty() {
        for item in items.iter_mut() {
            let hay = format!(
                "{} {} {}",
                item.title.to_ascii_lowercase(),
                item.description.to_ascii_lowercase(),
                item.action.to_ascii_lowercase()
            );
            let hits = tokens.iter().filter(|t| hay.contains(t.as_str())).count();
            if hits > 0 {
                let boost = (hits as u8).saturating_mul(2);
                item.priority = item.priority.saturating_add(boost);
            }
        }
    }
    rank(items);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_by_priority() {
        let mut items = vec![
            ProactiveItem {
                title: "a".into(),
                description: "".into(),
                priority: 1,
                action: "".into(),
            },
            ProactiveItem {
                title: "b".into(),
                description: "".into(),
                priority: 5,
                action: "".into(),
            },
        ];
        rank(&mut items);
        assert_eq!(items[0].title, "b");
    }

    #[test]
    fn query_boosts_match() {
        let mut items = vec![
            ProactiveItem {
                title: "other".into(),
                description: "x".into(),
                priority: 3,
                action: "".into(),
            },
            ProactiveItem {
                title: "fix login".into(),
                description: "auth bug".into(),
                priority: 1,
                action: "patch".into(),
            },
        ];
        rank_with_query(&mut items, "login auth");
        assert_eq!(items[0].title, "fix login");
    }
}
