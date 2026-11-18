//! Byte-bounded LRU with allocation-free, borrowed text/style lookup.
#[cfg(test)]
mod tests;
use crate::layout::{FontFamily, FontWeight, TextStyle};
use crate::model::flow::{BaseDirection, BidiState};
use lru::LruCache;
use std::{
    borrow::Borrow,
    hash::{Hash, Hasher},
};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CacheBudget {
    pub limit: usize,
    pub used: usize,
    pub evictions: u64,
}
impl CacheBudget {
    pub fn reserve(&mut self, bytes: usize) -> bool {
        if bytes > self.limit.saturating_sub(self.used) {
            return false;
        }
        self.used += bytes;
        true
    }
    pub fn release(&mut self, bytes: usize) {
        self.used -= bytes;
    }
}

#[derive(Eq, PartialEq, Hash)]
struct Parts<'a> {
    text: &'a str,
    family: &'a FontFamily,
    size: u32,
    weight: FontWeight,
    italic: bool,
    letter_spacing: u32,
    bidi: BidiState,
    direction: BaseDirection,
    scale: u32,
}
trait KeyView {
    fn parts(&self) -> Parts<'_>;
}
impl Hash for dyn KeyView + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.parts().hash(state);
    }
}
impl PartialEq for dyn KeyView + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.parts() == other.parts()
    }
}
impl Eq for dyn KeyView + '_ {}
struct Lookup<'a> {
    text: &'a str,
    style: &'a TextStyle,
    scale: f32,
}
impl KeyView for Lookup<'_> {
    fn parts(&self) -> Parts<'_> {
        Parts {
            text: self.text,
            family: &self.style.family,
            size: self.style.size.to_bits(),
            weight: self.style.weight,
            italic: self.style.italic,
            letter_spacing: self.style.letter_spacing.to_bits(),
            bidi: self.style.bidi,
            direction: self.style.shape_direction,
            scale: self.scale.to_bits(),
        }
    }
}
struct Key {
    text: Box<str>,
    family: FontFamily,
    size: u32,
    weight: FontWeight,
    italic: bool,
    letter_spacing: u32,
    bidi: BidiState,
    direction: BaseDirection,
    scale: u32,
}
impl KeyView for Key {
    fn parts(&self) -> Parts<'_> {
        Parts {
            text: &self.text,
            family: &self.family,
            size: self.size,
            weight: self.weight,
            italic: self.italic,
            letter_spacing: self.letter_spacing,
            bidi: self.bidi,
            direction: self.direction,
            scale: self.scale,
        }
    }
}
impl Hash for Key {
    fn hash<H: Hasher>(&self, s: &mut H) {
        self.parts().hash(s);
    }
}
impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.parts() == other.parts()
    }
}
impl Eq for Key {}
impl<'a> Borrow<dyn KeyView + 'a> for Key {
    fn borrow(&self) -> &(dyn KeyView + 'a) {
        self
    }
}
struct Entry<V> {
    value: V,
    bytes: usize,
}
pub(super) struct TextCache<V> {
    entries: LruCache<Key, Entry<V>>,
}
impl<V> Default for TextCache<V> {
    fn default() -> Self {
        Self {
            entries: LruCache::unbounded(),
        }
    }
}
impl<V> TextCache<V> {
    pub fn get(&mut self, text: &str, style: &TextStyle, scale: f32) -> Option<&V> {
        self.entries
            .get(&Lookup { text, style, scale } as &dyn KeyView)
            .map(|entry| &entry.value)
    }
    // The caller has already observed a miss; do not hash the key again just
    // to check for a replacement. No callbacks can mutate this cache meanwhile.
    pub fn insert_new(
        &mut self,
        text: &str,
        style: &TextStyle,
        scale: f32,
        value: V,
        payload: usize,
        budget: &mut CacheBudget,
    ) {
        let family_bytes = match &style.family {
            FontFamily::Named(s) => s.len(),
            _ => 0,
        };
        // Include key/value and conservative map/LRU-node overhead, as well as
        // the owned text/family and payload capacities supplied by the caller.
        let bytes = std::mem::size_of::<Key>()
            + std::mem::size_of::<Entry<V>>()
            + 6 * std::mem::size_of::<usize>()
            + text.len()
            + family_bytes
            + payload;
        if bytes > budget.limit {
            return;
        }
        debug_assert!(
            self.entries
                .peek(&Lookup { text, style, scale } as &dyn KeyView)
                .is_none()
        );
        while !budget.reserve(bytes) {
            if !self.evict(budget) {
                return;
            }
        }
        self.entries.put(
            Key {
                text: text.into(),
                family: style.family.clone(),
                size: style.size.to_bits(),
                weight: style.weight,
                italic: style.italic,
                letter_spacing: style.letter_spacing.to_bits(),
                bidi: style.bidi,
                direction: style.shape_direction,
                scale: scale.to_bits(),
            },
            Entry { value, bytes },
        );
    }
    pub fn evict(&mut self, budget: &mut CacheBudget) -> bool {
        if let Some((_, old)) = self.entries.pop_lru() {
            budget.release(old.bytes);
            budget.evictions += 1;
            true
        } else {
            false
        }
    }
    pub fn shrink_storage(&mut self) {
        // Resize to the current length without evicting anything, shrink the
        // map, then restore byte-budget-controlled (rather than count) growth.
        self.entries
            .resize(std::num::NonZeroUsize::new(self.entries.len().max(1)).unwrap());
        self.entries.resize(std::num::NonZeroUsize::MAX);
    }
    pub fn clear(&mut self, budget: &mut CacheBudget) {
        while self.evict(budget) {}
    }
}
