//! Explicitly borrowed, bounded font-environment ownership. No synchronization
//! or process-global state: one context serves one operation at a time.
#[cfg(test)]
mod tests;
use crate::{
    settings::RenderSettings,
    text::{FontResource, FontSystem, FontSystemStats, RasterizationOptions},
};
use lru::LruCache;
use std::cell::{RefCell, RefMut};

#[derive(Clone, Copy, Debug)]
pub struct FontContextOptions {
    pub max_environments: usize,
    /// Accounted cache admission bytes, not font backing or opaque backend
    /// metadata, allocator overhead, external references, or process RSS.
    pub cache_bytes: usize,
    /// Zero disables explicit plan reuse.
    pub plans_per_face: usize,
    /// Parsed/shaper face working set per environment (minimum one).
    pub max_faces_per_environment: usize,
}
impl Default for FontContextOptions {
    fn default() -> Self {
        Self {
            max_environments: 4,
            cache_bytes: 4 * 1024 * 1024,
            plans_per_face: 8,
            max_faces_per_environment: 16,
        }
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct FontContextStats {
    pub environment_builds: u64,
    pub environment_hits: u64,
    pub environment_evictions: u64,
    pub environments: usize,
    pub cache_bytes: usize,
    pub fonts: FontSystemStats,
    pub cached_faces: usize,
    pub cached_plans: usize,
}

/// Reuse this owner across independently created viewers, using their
/// `with_font_context` operations. Dropping viewers does not discard this cache.
pub struct FontContext {
    options: FontContextOptions,
    environments: LruCache<[u8; 32], FontSystem>,
    stats: FontContextStats,
}
impl Default for FontContext {
    fn default() -> Self {
        Self::new(FontContextOptions::default())
    }
}
impl FontContext {
    pub fn new(mut options: FontContextOptions) -> Self {
        options.max_faces_per_environment = options.max_faces_per_environment.max(1);
        Self {
            environments: LruCache::unbounded(),
            options,
            stats: FontContextStats::default(),
        }
    }
    pub fn options(&self) -> FontContextOptions {
        self.options
    }
    /// Resize/trim reusable state without changing any font or layout identity.
    pub fn set_options(&mut self, mut options: FontContextOptions) {
        let shrink = options.cache_bytes < self.options.cache_bytes;
        options.max_faces_per_environment = options.max_faces_per_environment.max(1);
        self.options = options;
        while self.environments.len() > options.max_environments {
            self.environments.pop_lru();
            self.stats.environment_evictions += 1;
        }
        for (_, fonts) in self.environments.iter_mut() {
            fonts.set_face_capacity(options.max_faces_per_environment);
            fonts.set_plan_capacity(options.plans_per_face);
        }
        loop {
            let used: usize = self
                .environments
                .iter()
                .map(|(_, fonts)| fonts.cache_bytes())
                .sum();
            if used <= options.cache_bytes {
                break;
            }
            let key = *self
                .environments
                .iter()
                .rev()
                .find(|(_, fonts)| fonts.cache_bytes() > 0)
                .unwrap()
                .0;
            let fonts = self.environments.peek_mut(&key).unwrap();
            fonts.set_cache_budget(
                fonts
                    .cache_bytes()
                    .saturating_sub(used - options.cache_bytes),
            );
            accumulate(&mut self.stats.fonts, fonts.take_stats());
        }
        if shrink {
            for (_, fonts) in self.environments.iter_mut() {
                fonts.shrink_cache_storage();
            }
        }
        self.environments
            .resize(std::num::NonZeroUsize::new(self.environments.len().max(1)).unwrap());
        self.environments.resize(std::num::NonZeroUsize::MAX);
    }

    pub fn stats(&self) -> FontContextStats {
        let mut stats = self.stats;
        stats.environments = self.environments.len();
        stats.cache_bytes = self
            .environments
            .iter()
            .map(|(_, fonts)| fonts.cache_bytes())
            .sum();
        stats.cached_faces = self
            .environments
            .iter()
            .map(|(_, fonts)| fonts.cached_faces())
            .sum();
        stats.cached_plans = self
            .environments
            .iter()
            .map(|(_, fonts)| fonts.cached_plans())
            .sum();
        stats
    }
    pub fn trim(&mut self) {
        self.stats.environment_evictions += self.environments.len() as u64;
        self.environments.clear();
    }
    fn checkout(&mut self, description: &FontDescription) -> Option<FontSystem> {
        let key = description.key();
        let mut fonts = if let Some(fonts) = self.environments.pop(&key) {
            self.stats.environment_hits += 1;
            fonts
        } else {
            let mut fonts =
                description.build(self.options.cache_bytes, self.options.plans_per_face)?;
            fonts.set_face_capacity(self.options.max_faces_per_environment);
            self.stats.environment_builds += 1;
            if self.options.max_environments > 0
                && self.environments.len() >= self.options.max_environments
            {
                self.environments.pop_lru();
                self.stats.environment_evictions += 1;
            }
            fonts
        };
        // Offer each selected environment a fair minimum working set. Reclaim
        // from the least-recently used *idle* environment, never an active user.
        let required = fonts
            .cache_bytes()
            .max(self.options.cache_bytes / self.options.max_environments.max(1));
        loop {
            let used: usize = self
                .environments
                .iter()
                .map(|(_, font)| font.cache_bytes())
                .sum();
            if used <= self.options.cache_bytes.saturating_sub(required) {
                fonts.set_cache_budget(self.options.cache_bytes - used);
                break;
            }
            let key = *self
                .environments
                .iter()
                .rev()
                .find(|(_, font)| font.cache_bytes() > 0)
                .expect("cache bytes have an owner")
                .0;
            let other = self.environments.peek_mut(&key).unwrap();
            let excess = used - (self.options.cache_bytes - required);
            other.set_cache_budget(other.cache_bytes().saturating_sub(excess));
            accumulate(&mut self.stats.fonts, other.take_stats());
        }
        Some(fonts)
    }
    fn checkin(&mut self, key: [u8; 32], mut fonts: FontSystem) {
        accumulate(&mut self.stats.fonts, fonts.take_stats());
        if self.options.max_environments == 0 {
            return;
        }
        if self.environments.push(key, fonts).is_some() {
            self.stats.environment_evictions += 1;
        }
        while self.environments.len() > self.options.max_environments {
            self.environments.pop_lru();
            self.stats.environment_evictions += 1;
        }
        debug_assert!(self.stats().cache_bytes <= self.options.cache_bytes);
    }
    pub(crate) fn validate(&mut self, description: &FontDescription) -> bool {
        if let Some(fonts) = self.checkout(description) {
            self.checkin(description.key(), fonts);
            true
        } else {
            false
        }
    }
    pub(crate) fn scope<T: FontHost, R>(
        &mut self,
        host: &mut T,
        operation: impl FnOnce(&mut T) -> R,
    ) -> R {
        let description = host.font_slot().description.clone();
        let rasterization = host.font_slot().rasterization;
        let fonts = self
            .checkout(&description)
            .expect("configured font resources were validated");
        let limit = fonts.cache_limit();
        let slot = FontSlot {
            description,
            rasterization,
            backend: RefCell::new(Some(fonts)),
            limit,
            carried: FontSystemStats::default(),
            plans: self.options.plans_per_face,
            faces: self.options.max_faces_per_environment,
        };
        let previous = std::mem::replace(host.font_slot(), slot);
        let scope = FontScope {
            host,
            context: self,
            previous: Some(previous),
        };
        operation(scope.host)
    }
}

fn accumulate(to: &mut FontSystemStats, from: FontSystemStats) {
    to.registrations += from.registrations;
    to.metric_hits += from.metric_hits;
    to.metric_misses += from.metric_misses;
    to.shaped_hits += from.shaped_hits;
    to.shaped_misses += from.shaped_misses;
    to.shaper_data_builds += from.shaper_data_builds;
    to.plan_hits += from.plan_hits;
    to.plan_builds += from.plan_builds;
    to.evictions += from.evictions;
}

#[derive(Clone)]
pub(crate) struct FontDescription {
    body: Vec<String>,
    code: Vec<String>,
    author: bool,
    resources: Vec<FontResource>,
}
impl FontDescription {
    pub(crate) fn new(settings: &RenderSettings, resources: Vec<FontResource>) -> Self {
        Self {
            body: settings.body_fonts.clone(),
            code: settings.code_fonts.clone(),
            author: settings.author_fonts,
            resources,
        }
    }
    fn key(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(b"flow-font-environment-v1\0");
        for names in [&self.body, &self.code] {
            h.update(&(names.len() as u64).to_le_bytes());
            for name in names {
                h.update(&(name.len() as u64).to_le_bytes());
                h.update(name.as_bytes());
            }
        }
        h.update(&[u8::from(self.author)]);
        for resource in &self.resources {
            h.update(&resource.digest());
        }
        *h.finalize().as_bytes()
    }
    fn build(&self, limit: usize, plans: usize) -> Option<FontSystem> {
        let defaults = RenderSettings::default().rasterization();
        let mut fonts = FontSystem::new(
            &self.body,
            &self.code,
            defaults.regular_gamma,
            defaults.emphasized_gamma,
            defaults.hinting,
            defaults.positioning,
        );
        fonts.set_cache_budget(limit);
        fonts.set_plan_capacity(plans);
        fonts.set_author_fonts(self.author);
        for resource in &self.resources {
            if !fonts.register_font(resource.clone()) {
                return None;
            }
        }
        Some(fonts)
    }
}

pub(crate) struct FontSlot {
    pub(crate) description: FontDescription,
    rasterization: RasterizationOptions,
    backend: RefCell<Option<FontSystem>>,
    pub(crate) limit: usize,
    carried: FontSystemStats,
    plans: usize,
    faces: usize,
}
impl FontSlot {
    pub(crate) fn new(
        settings: &RenderSettings,
        resources: Vec<FontResource>,
        limit: usize,
    ) -> Self {
        Self {
            description: FontDescription::new(settings, resources),
            rasterization: settings.rasterization(),
            backend: RefCell::new(None),
            limit,
            carried: FontSystemStats::default(),
            plans: 8,
            faces: 16,
        }
    }
    pub(crate) fn validate(&mut self) -> bool {
        if self.backend.get_mut().is_none() {
            *self.backend.get_mut() = self.description.build(self.limit, self.plans);
        }
        self.backend.get_mut().is_some()
    }
    pub(crate) fn borrow_mut(&self) -> RefMut<'_, FontSystem> {
        let mut backend = self.backend.borrow_mut();
        if backend.is_none() {
            *backend = Some(
                self.description
                    .build(self.limit, 8)
                    .expect("validated font resources"),
            );
        }
        let mut backend = RefMut::map(backend, |fonts| fonts.as_mut().unwrap());
        backend.set_face_capacity(self.faces);
        backend.set_plan_capacity(self.plans);
        if backend.rasterization() != self.rasterization {
            backend.set_rasterization(self.rasterization);
        }
        backend
    }
    pub(crate) fn set_rasterization(&mut self, options: RasterizationOptions) {
        self.rasterization = options;
    }
    pub(crate) fn replace(&mut self, mut next: Self) {
        next.plans = self.plans;
        next.faces = self.faces;
        if let Some(fonts) = next.backend.get_mut().as_mut() {
            fonts.set_face_capacity(next.faces);
            fonts.set_plan_capacity(next.plans);
        }
        accumulate(&mut next.carried, self.carried);
        if let Some(mut fonts) = self.backend.get_mut().take() {
            accumulate(&mut next.carried, fonts.take_stats());
        }
        *self = next;
    }
    pub(crate) fn rebind(&mut self, settings: &RenderSettings, resources: Vec<FontResource>) {
        let mut next = Self::new(settings, resources, self.limit);
        next.plans = self.plans;
        accumulate(&mut next.carried, self.carried);
        if let Some(mut fonts) = self.backend.get_mut().take() {
            accumulate(&mut next.carried, fonts.take_stats());
        }
        *self = next;
    }
}
impl crate::layout::TextMeasurer for FontSlot {
    fn measure(&self, text: &str, style: &crate::layout::TextStyle) -> crate::layout::TextMetrics {
        self.borrow_mut().measure(text, style)
    }
}
pub(crate) trait FontHost {
    fn font_slot(&mut self) -> &mut FontSlot;
}
struct FontScope<'a, T: FontHost> {
    host: &'a mut T,
    context: &'a mut FontContext,
    previous: Option<FontSlot>,
}
impl<T: FontHost> Drop for FontScope<'_, T> {
    fn drop(&mut self) {
        let previous = self.previous.take().unwrap();
        let mut current = std::mem::replace(self.host.font_slot(), previous);
        let restored = self.host.font_slot();
        if restored.description.key() != current.description.key() {
            // Configuration changes inside a scope invalidate private fallback
            // state, but cannot mutate any other cached environment.
            *restored = FontSlot {
                description: current.description.clone(),
                rasterization: current.rasterization,
                backend: RefCell::new(None),
                limit: FontContextOptions::default().cache_bytes,
                carried: FontSystemStats::default(),
                plans: 8,
                faces: 16,
            };
        } else {
            restored.rasterization = current.rasterization;
        }
        accumulate(&mut self.context.stats.fonts, current.carried);
        if let Some(fonts) = current.backend.get_mut().take() {
            self.context.checkin(current.description.key(), fonts);
        }
    }
}
