use super::*;
use crate::model::flow::{BaseDirection, BidiState};
fn style() -> TextStyle {
    TextStyle {
        family: FontFamily::Named("Borrowed family".into()),
        size: 12.0,
        weight: FontWeight::Normal,
        italic: false,
        strikethrough: false,
        link: false,
        initial_letter: None,
        baseline_shift: 0.0,
        letter_spacing: 0.0,
        bidi: BidiState::default(),
        bidi_boundary: None,
        shape_direction: BaseDirection::LeftToRight,
    }
}
#[test]
fn borrowed_lookup_hashes_like_owned_keys_and_respects_dependencies() {
    let mut cache = TextCache::default();
    let mut budget = CacheBudget {
        limit: 8192,
        ..CacheBudget::default()
    };
    let style = style();
    cache.insert_new("reused", &style, 1.0, 42, 0, &mut budget);
    assert_eq!(cache.get("reused", &style, 1.0), Some(&42));
    assert!(cache.get("reused", &style, 2.0).is_none());
    let mut changed = style.clone();
    changed.shape_direction = BaseDirection::RightToLeft;
    assert!(cache.get("reused", &changed, 1.0).is_none());
    changed = style.clone();
    changed.size = 14.0;
    assert!(cache.get("reused", &changed, 1.0).is_none());
    assert_eq!(cache.get("reused", &style.clone(), 1.0), Some(&42));
}
#[test]
fn oversized_values_do_not_evict_useful_entries_or_bypass_the_budget() {
    let mut cache = TextCache::default();
    let mut budget = CacheBudget {
        limit: 4096,
        ..CacheBudget::default()
    };
    let style = style();
    cache.insert_new("small", &style, 1.0, 1, 0, &mut budget);
    let used = budget.used;
    cache.insert_new("oversized", &style, 1.0, 2, 8192, &mut budget);
    assert_eq!(budget.used, used);
    assert_eq!(cache.get("small", &style, 1.0), Some(&1));
    assert!(cache.get("oversized", &style, 1.0).is_none());
    cache.clear(&mut budget);
    assert_eq!(budget.used, 0);
}
#[test]
fn eviction_is_lru_and_accounting_is_released_on_clear() {
    let mut cache = TextCache::default();
    let mut budget = CacheBudget {
        limit: 4096,
        ..CacheBudget::default()
    };
    let style = style();
    cache.insert_new("one", &style, 1.0, 1, 0, &mut budget);
    let entry = budget.used;
    budget.limit = entry * 2;
    cache.insert_new("two", &style, 1.0, 2, 0, &mut budget);
    cache.get("one", &style, 1.0);
    cache.insert_new("new", &style, 1.0, 3, 0, &mut budget);
    assert_eq!(cache.get("one", &style, 1.0), Some(&1));
    assert!(cache.get("two", &style, 1.0).is_none());
    assert_eq!(budget.used, entry * 2);
    assert_eq!(cache.get("new", &style, 1.0), Some(&3));
    cache.clear(&mut budget);
    assert_eq!(budget.used, 0);
}
