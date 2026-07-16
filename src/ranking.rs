//! Proactive ranking: score and rank items by relevance.

use crate::extract::ProactiveItem;

pub fn rank(items: &mut [ProactiveItem]) {
    items.sort_by(|a, b| b.priority.cmp(&a.priority));
}

pub fn top_n(items: &mut Vec<ProactiveItem>, n: usize) -> Vec<ProactiveItem> {
    rank(items);
    items.drain(..n.min(items.len())).collect()
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
}
