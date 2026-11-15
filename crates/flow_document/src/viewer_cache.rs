use std::{collections::HashMap, mem::size_of, sync::Arc};

use crate::{
    layout::{AtomicLayoutKind, BlockLayout, ComposedDocument, ContinuousLayout},
    pagination::PaginatedLayout,
};

pub(crate) const DEFAULT_LAYOUT_CACHE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[doc(hidden)]
pub struct SemanticKey(pub(crate) u64);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CompositionKey {
    pub(crate) semantic: SemanticKey,
    pub(crate) renderer_revision: u64,
    pub(crate) environment: [u8; 32],
    pub(crate) content_width: u32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ContinuousPlacementKey {
    pub(crate) composition: CompositionKey,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PaginationKey {
    pub(crate) composition: CompositionKey,
    pub(crate) body_height: u32,
}

struct Entry<T> {
    artifact: Arc<T>,
    bytes: usize,
    last_used: u64,
}

#[derive(Clone, Copy)]
enum Oldest {
    Composition(CompositionKey),
    Continuous(ContinuousPlacementKey),
    Pagination(PaginationKey),
}

pub(crate) struct LayoutArtifactCache {
    compositions: HashMap<CompositionKey, Entry<ComposedDocument>>,
    continuous: HashMap<ContinuousPlacementKey, Entry<ContinuousLayout>>,
    pagination: HashMap<PaginationKey, Entry<PaginatedLayout>>,
    bytes: usize,
    limit: usize,
    clock: u64,
}

impl Default for LayoutArtifactCache {
    fn default() -> Self {
        Self::new(DEFAULT_LAYOUT_CACHE_BYTES)
    }
}

impl LayoutArtifactCache {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            compositions: HashMap::new(),
            continuous: HashMap::new(),
            pagination: HashMap::new(),
            bytes: 0,
            limit,
            clock: 0,
        }
    }

    pub(crate) fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
        self.trim();
    }

    pub(crate) fn composition(&mut self, key: CompositionKey) -> Option<Arc<ComposedDocument>> {
        self.clock = self.clock.wrapping_add(1);
        let Some(entry) = self.compositions.get_mut(&key) else {
            crate::telemetry::phase_event!("flow.cache.miss", cache = "composition");
            return None;
        };
        entry.last_used = self.clock;
        crate::telemetry::phase_event!("flow.cache.hit", cache = "composition");
        Some(Arc::clone(&entry.artifact))
    }

    pub(crate) fn continuous(
        &mut self,
        key: ContinuousPlacementKey,
    ) -> Option<Arc<ContinuousLayout>> {
        self.clock = self.clock.wrapping_add(1);
        let Some(entry) = self.continuous.get_mut(&key) else {
            crate::telemetry::phase_event!("flow.cache.miss", cache = "continuous");
            return None;
        };
        entry.last_used = self.clock;
        crate::telemetry::phase_event!("flow.cache.hit", cache = "continuous");
        Some(Arc::clone(&entry.artifact))
    }

    pub(crate) fn pagination(&mut self, key: PaginationKey) -> Option<Arc<PaginatedLayout>> {
        self.clock = self.clock.wrapping_add(1);
        let Some(entry) = self.pagination.get_mut(&key) else {
            crate::telemetry::phase_event!("flow.cache.miss", cache = "pagination");
            return None;
        };
        entry.last_used = self.clock;
        crate::telemetry::phase_event!("flow.cache.hit", cache = "pagination");
        Some(Arc::clone(&entry.artifact))
    }

    pub(crate) fn insert_composition(
        &mut self,
        key: CompositionKey,
        artifact: Arc<ComposedDocument>,
    ) {
        self.clock = self.clock.wrapping_add(1);
        let bytes = composed_bytes(&artifact);
        self.replace_composition(
            key,
            Entry {
                artifact,
                bytes,
                last_used: self.clock,
            },
        );
        self.trim();
    }

    pub(crate) fn insert_continuous(
        &mut self,
        key: ContinuousPlacementKey,
        artifact: Arc<ContinuousLayout>,
    ) {
        self.clock = self.clock.wrapping_add(1);
        let bytes = continuous_bytes(&artifact);
        if let Some(previous) = self.continuous.insert(
            key,
            Entry {
                artifact,
                bytes,
                last_used: self.clock,
            },
        ) {
            self.bytes = self.bytes.saturating_sub(previous.bytes);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.trim();
    }

    pub(crate) fn insert_pagination(&mut self, key: PaginationKey, artifact: Arc<PaginatedLayout>) {
        self.clock = self.clock.wrapping_add(1);
        let bytes = paginated_bytes(&artifact);
        if let Some(previous) = self.pagination.insert(
            key,
            Entry {
                artifact,
                bytes,
                last_used: self.clock,
            },
        ) {
            self.bytes = self.bytes.saturating_sub(previous.bytes);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.trim();
    }

    fn replace_composition(&mut self, key: CompositionKey, entry: Entry<ComposedDocument>) {
        let bytes = entry.bytes;
        if let Some(previous) = self.compositions.insert(key, entry) {
            self.bytes = self.bytes.saturating_sub(previous.bytes);
        }
        self.bytes = self.bytes.saturating_add(bytes);
    }

    fn trim(&mut self) {
        while self.bytes > self.limit {
            let Some(oldest) = self.oldest() else { break };
            let removed = match oldest {
                Oldest::Composition(key) => self.compositions.remove(&key).map(|entry| entry.bytes),
                Oldest::Continuous(key) => self.continuous.remove(&key).map(|entry| entry.bytes),
                Oldest::Pagination(key) => self.pagination.remove(&key).map(|entry| entry.bytes),
            };
            let Some(bytes) = removed else { continue };
            self.bytes = self.bytes.saturating_sub(bytes);
            crate::telemetry::phase_event!(
                "flow.cache.evict",
                cache = "layout-artifact",
                estimated_bytes = self.bytes as u64,
                byte_limit = self.limit as u64,
            );
        }
    }

    fn oldest(&self) -> Option<Oldest> {
        self.compositions
            .iter()
            .map(|(key, entry)| (entry.last_used, Oldest::Composition(*key)))
            .chain(
                self.continuous
                    .iter()
                    .map(|(key, entry)| (entry.last_used, Oldest::Continuous(*key))),
            )
            .chain(
                self.pagination
                    .iter()
                    .map(|(key, entry)| (entry.last_used, Oldest::Pagination(*key))),
            )
            .min_by_key(|(last_used, _)| *last_used)
            .map(|(_, oldest)| oldest)
    }
}

fn composed_bytes(document: &ComposedDocument) -> usize {
    size_of::<ComposedDocument>()
        + document.blocks.capacity() * size_of::<crate::layout::ComposedBlock>()
        + document
            .blocks
            .iter()
            .map(|block| block_dynamic_bytes(&block.layout))
            .sum::<usize>()
}

fn continuous_bytes(layout: &ContinuousLayout) -> usize {
    size_of::<ContinuousLayout>()
        + layout.blocks.capacity() * size_of::<BlockLayout>()
        + layout.blocks.iter().map(block_dynamic_bytes).sum::<usize>()
        + layout.quote_bars.capacity() * size_of::<crate::layout::ContinuousQuoteBarLayout>()
}

fn block_dynamic_bytes(block: &BlockLayout) -> usize {
    let lines = block.lines.capacity() * size_of::<crate::layout::LineLayout>()
        + block
            .lines
            .iter()
            .map(|line| {
                line.runs.capacity() * size_of::<crate::layout::RunLayout>()
                    + line
                        .runs
                        .iter()
                        .map(|run| run.text.capacity())
                        .sum::<usize>()
            })
            .sum::<usize>();
    let atomics = block.atomic_objects.capacity() * size_of::<crate::layout::AtomicLayout>()
        + block
            .atomic_objects
            .iter()
            .map(|object| match &object.kind {
                AtomicLayoutKind::Image { key, alt } => key.capacity() + alt.capacity(),
                AtomicLayoutKind::Placeholder { label } => label.capacity(),
                AtomicLayoutKind::ThematicBreak => 0,
            })
            .sum::<usize>();
    let table = block.table.as_ref().map_or(0, |table| {
        table.column_widths.capacity() * size_of::<f32>()
            + table.row_heights.capacity() * size_of::<f32>()
            + table.row_spans.capacity() * size_of::<(usize, usize)>()
            + table.rules.capacity() * size_of::<crate::layout::TableRuleLayout>()
            + table.quote_bars.capacity() * size_of::<crate::layout::TableQuoteBarLayout>()
    });
    lines + atomics + table + block.quote_bars.capacity() * size_of::<f32>()
}

fn paginated_bytes(layout: &PaginatedLayout) -> usize {
    size_of::<PaginatedLayout>()
        + layout.pages.capacity() * size_of::<crate::pagination::ComposedPage>()
        + layout.fragments.capacity() * size_of::<crate::pagination::PageFragment>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_limit_evicts_across_artifact_classes() {
        let mut cache = LayoutArtifactCache::new(1);
        let semantic = SemanticKey(1);
        let composition = CompositionKey {
            semantic,
            renderer_revision: 1,
            environment: [0; 32],
            content_width: 1,
        };
        cache.insert_composition(composition, Arc::new(ComposedDocument::default()));
        assert!(cache.compositions.is_empty());
        assert_eq!(cache.bytes, 0);
        cache.insert_continuous(
            ContinuousPlacementKey { composition },
            Arc::new(ContinuousLayout::default()),
        );
        assert!(cache.continuous.is_empty());
        assert_eq!(cache.bytes, 0);
        cache.insert_pagination(
            PaginationKey {
                composition,
                body_height: 1,
            },
            Arc::new(PaginatedLayout::default()),
        );
        assert_eq!(cache.bytes, 0);
        assert!(cache.compositions.is_empty());
        assert!(cache.continuous.is_empty());
        assert!(cache.pagination.is_empty());
    }
}
