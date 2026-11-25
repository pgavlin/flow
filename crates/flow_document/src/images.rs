use crate::{layout::ImageMetrics, pixels::PixelFormat};
use image::{RgbaImage, imageops::FilterType};
use lru::LruCache;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
const MAX_IMAGE_PIXELS: u64 = 50_000_000;
const SOURCE_IMAGE_CACHE_BUDGET: usize = 16 * 1024 * 1024;
const SCALED_IMAGE_CACHE_BUDGET: usize = 16 * 1024 * 1024;
const BYTES_PER_PIXEL: usize = 4;
static NEXT_IMAGE_RESOURCE_ID: AtomicU64 = AtomicU64::new(1);

fn next_image_resource_id() -> u64 {
    NEXT_IMAGE_RESOURCE_ID.fetch_add(1, Ordering::Relaxed)
}

/// Immutable decoded image pixels with host-selected backing, stride, and channel order.
///
/// The backing may be owned bytes, a memory map, or a platform resource handle whose
/// [`AsRef`] implementation exposes stable bytes. Cloning this value only clones the
/// shared backing handle.
#[derive(Clone)]
pub struct ImagePixels {
    width: u32,
    height: u32,
    stride: usize,
    format: PixelFormat,
    resource_id: u64,
    data: Arc<dyn AsRef<[u8]> + Send + Sync>,
}

impl ImagePixels {
    pub fn from_rgba(width: u32, height: u32, data: Vec<u8>) -> Option<Self> {
        let stride = (width as usize).checked_mul(BYTES_PER_PIXEL)?;
        Self::from_backing(width, height, stride, PixelFormat::Rgba8888, data)
    }

    /// Wraps arbitrary shared, immutable decoded pixel backing without copying it.
    pub fn from_backing(
        width: u32,
        height: u32,
        stride: usize,
        format: PixelFormat,
        data: impl AsRef<[u8]> + Send + Sync + 'static,
    ) -> Option<Self> {
        let row_bytes = (width as usize).checked_mul(BYTES_PER_PIXEL)?;
        if width == 0 || height == 0 || stride < row_bytes {
            return None;
        }
        let required = (height as usize - 1)
            .checked_mul(stride)?
            .checked_add(row_bytes)?;
        if data.as_ref().len() < required {
            return None;
        }
        Some(Self {
            width,
            height,
            stride,
            format,
            resource_id: next_image_resource_id(),
            data: Arc::new(data),
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }

    pub fn resource_id(&self) -> u64 {
        self.resource_id
    }

    pub fn pixels(&self) -> &[u8] {
        self.data.as_ref().as_ref()
    }

    fn retained_bytes(&self) -> usize {
        self.pixels().len()
    }

    fn resized_rgba(&self, width: u32, height: u32) -> RgbaImage {
        let row_bytes = self.width as usize * BYTES_PER_PIXEL;
        if self.format == PixelFormat::Rgba8888 && self.stride == row_bytes {
            let source = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(
                self.width,
                self.height,
                self.pixels(),
            )
            .unwrap();
            return image::imageops::resize(&source, width, height, FilterType::Lanczos3);
        }
        image::imageops::resize(&self.to_rgba_image(), width, height, FilterType::Lanczos3)
    }

    fn to_rgba_image(&self) -> RgbaImage {
        let row_bytes = self.width as usize * BYTES_PER_PIXEL;
        let mut rgba = vec![0; row_bytes * self.height as usize];
        for y in 0..self.height as usize {
            let source = &self.pixels()[y * self.stride..y * self.stride + row_bytes];
            let target = &mut rgba[y * row_bytes..(y + 1) * row_bytes];
            match self.format {
                PixelFormat::Rgba8888 => target.copy_from_slice(source),
                PixelFormat::Bgra8888 => {
                    for (target, source) in target.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
                        target.copy_from_slice(&[source[2], source[1], source[0], source[3]]);
                    }
                }
            }
        }
        RgbaImage::from_raw(self.width, self.height, rgba).unwrap()
    }
}

impl From<RgbaImage> for ImagePixels {
    fn from(image: RgbaImage) -> Self {
        let (width, height) = image.dimensions();
        Self {
            width,
            height,
            stride: width as usize * BYTES_PER_PIXEL,
            format: PixelFormat::Rgba8888,
            resource_id: next_image_resource_id(),
            data: Arc::new(image.into_raw()),
        }
    }
}

/// The frontend-neutral identity of an image referenced by a document.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ImageRequest {
    pub url: String,
    /// Opaque cache scope. Never an acquisition address.
    pub namespace: Option<String>,
    pub resource_base: Option<String>,
    /// Preferred decoded pixel dimensions. Metadata requests leave these unset.
    pub desired_width: Option<u32>,
    pub desired_height: Option<u32>,
}

impl ImageRequest {
    pub fn new(url: impl Into<String>, resource_base: Option<String>) -> Self {
        Self {
            url: url.into(),
            namespace: None,
            resource_base,
            desired_width: None,
            desired_height: None,
        }
    }

    fn for_pixels(&self, width: u32, height: u32) -> Self {
        let mut request = self.clone();
        request.desired_width = Some(width);
        request.desired_height = Some(height);
        request
    }

    pub(crate) fn key(&self) -> String {
        let key = resource_key(&self.url, self.resource_base.as_deref());
        match &self.namespace {
            Some(namespace) => format!("\0scope:{}:{namespace}{key}", namespace.len()),
            None => key,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum RequestState {
    #[default]
    Unknown,
    Pending,
    Failed,
}

struct CachedImage {
    id: u64,
    request: ImageRequest,
    dimensions: Option<(u32, u32)>,
    pixels: Option<Arc<ImagePixels>>,
    last_source_use: u64,
    metadata_request: RequestState,
    pixels_request: RequestState,
}

impl CachedImage {
    fn unknown(id: u64, request: ImageRequest) -> Self {
        Self {
            id,
            request,
            dimensions: None,
            pixels: None,
            last_source_use: 0,
            metadata_request: RequestState::Unknown,
            pixels_request: RequestState::Unknown,
        }
    }
}

pub struct ImageCache {
    pub(crate) namespace: Option<String>,
    images: HashMap<String, CachedImage>,
    metadata_requests: Vec<ImageRequest>,
    pixel_requests: Vec<ImageRequest>,
    scaled_images: LruCache<(u64, u32, u32), Arc<ImagePixels>>,
    source_image_bytes: usize,
    scaled_image_bytes: usize,
    use_clock: u64,
    next_image_id: u64,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self {
            namespace: None,
            images: HashMap::new(),
            metadata_requests: Vec::new(),
            pixel_requests: Vec::new(),
            scaled_images: LruCache::unbounded(),
            source_image_bytes: 0,
            scaled_image_bytes: 0,
            use_clock: 0,
            next_image_id: 0,
        }
    }
}

impl ImageCache {
    pub(crate) fn request(&self, url: &str, base: Option<&str>) -> ImageRequest {
        let mut request = ImageRequest::new(url, base.map(str::to_owned));
        request.namespace.clone_from(&self.namespace);
        request
    }

    fn allocate_image_id(&mut self) -> u64 {
        self.next_image_id = self.next_image_id.wrapping_add(1).max(1);
        self.next_image_id
    }

    fn clear_scaled_images(&mut self) {
        self.scaled_images.clear();
        self.scaled_image_bytes = 0;
    }

    fn remove_scaled_image(&mut self, id: u64) {
        let keys: Vec<_> = self
            .scaled_images
            .iter()
            .filter(|(key, _)| key.0 == id)
            .map(|(key, _)| *key)
            .collect();
        for key in keys {
            if let Some(pixels) = self.scaled_images.pop(&key) {
                self.scaled_image_bytes = self
                    .scaled_image_bytes
                    .saturating_sub(pixels.retained_bytes());
            }
        }
    }

    fn cache_scaled(&mut self, key: (u64, u32, u32), pixels: Arc<ImagePixels>) {
        let bytes = pixels.retained_bytes();
        if bytes > SCALED_IMAGE_CACHE_BUDGET {
            return;
        }
        while self.scaled_image_bytes + bytes > SCALED_IMAGE_CACHE_BUDGET {
            let Some((_, evicted)) = self.scaled_images.pop_lru() else {
                break;
            };
            self.scaled_image_bytes = self
                .scaled_image_bytes
                .saturating_sub(evicted.retained_bytes());
        }
        if let Some(replaced) = self.scaled_images.put(key, pixels) {
            self.scaled_image_bytes = self
                .scaled_image_bytes
                .saturating_sub(replaced.retained_bytes());
        }
        self.scaled_image_bytes += bytes;
    }

    fn evict_source_images(&mut self) {
        while self.source_image_bytes > SOURCE_IMAGE_CACHE_BUDGET {
            let Some(key) = self
                .images
                .iter()
                .filter(|(_, image)| image.pixels.is_some())
                .min_by_key(|(_, image)| image.last_source_use)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            let image = self.images.get_mut(&key).unwrap();
            if let Some(pixels) = image.pixels.take() {
                self.source_image_bytes = self
                    .source_image_bytes
                    .saturating_sub(pixels.retained_bytes());
                image.pixels_request = RequestState::Unknown;
            }
        }
    }

    pub fn trim(&mut self) {
        self.clear_scaled_images();
        for image in self.images.values_mut() {
            image.pixels = None;
            image.pixels_request = RequestState::Unknown;
        }
        self.source_image_bytes = 0;
    }

    pub fn register(&mut self, request: ImageRequest, pixels: impl Into<ImagePixels>) -> bool {
        let pixels = pixels.into();
        if !valid_dimensions(pixels.width(), pixels.height()) {
            return false;
        }
        let key = request.key();
        let id = self.allocate_image_id();
        self.use_clock = self.use_clock.wrapping_add(1);
        if let Some(previous) = self.images.remove(&key) {
            self.remove_scaled_image(previous.id);
            if let Some(pixels) = previous.pixels {
                self.source_image_bytes = self
                    .source_image_bytes
                    .saturating_sub(pixels.retained_bytes());
            }
        }
        self.source_image_bytes += pixels.retained_bytes();
        self.images.insert(
            key,
            CachedImage {
                id,
                request,
                dimensions: Some((pixels.width(), pixels.height())),
                pixels: Some(Arc::new(pixels)),
                last_source_use: self.use_clock,
                metadata_request: RequestState::Unknown,
                pixels_request: RequestState::Unknown,
            },
        );
        self.evict_source_images();
        true
    }

    /// Imported intrinsic dimensions let sized images request visible pixels
    /// without fetching metadata for every image in the document first.
    /// A hint never replaces dimensions already supplied by the host.
    pub fn hint_dimensions(&mut self, request: ImageRequest, width: u32, height: u32) -> bool {
        if !valid_dimensions(width, height) {
            return false;
        }
        let id = self.allocate_image_id();
        let image = self
            .images
            .entry(request.key())
            .or_insert_with(|| CachedImage::unknown(id, request));
        image.dimensions.get_or_insert((width, height));
        true
    }

    pub fn provide_metadata(&mut self, request: ImageRequest, width: u32, height: u32) -> bool {
        if !valid_dimensions(width, height) {
            return false;
        }
        let key = request.key();
        let id = self.allocate_image_id();
        let image = self
            .images
            .entry(key)
            .or_insert_with(|| CachedImage::unknown(id, request));
        image.dimensions = Some((width, height));
        image.metadata_request = RequestState::Unknown;
        true
    }

    /// Returns whether accepting the pixels supplied previously unknown intrinsic dimensions.
    pub fn provide_pixels(
        &mut self,
        request: ImageRequest,
        pixels: impl Into<ImagePixels>,
    ) -> Option<bool> {
        let pixels = pixels.into();
        if !valid_dimensions(pixels.width(), pixels.height()) {
            return None;
        }
        let decoded_dimensions = (pixels.width(), pixels.height());
        let key = request.key();
        let id = self.allocate_image_id();
        self.use_clock = self.use_clock.wrapping_add(1);
        let mut image = self
            .images
            .remove(&key)
            .unwrap_or_else(|| CachedImage::unknown(id, request.clone()));
        let dimensions_changed = image.dimensions.is_none();
        image.dimensions.get_or_insert(decoded_dimensions);
        image.request = request.clone();
        image.request.desired_width = None;
        image.request.desired_height = None;
        image.metadata_request = RequestState::Unknown;
        image.pixels_request = RequestState::Unknown;
        let pixels = Arc::new(pixels);
        let sized_response = request.desired_width == Some(decoded_dimensions.0)
            && request.desired_height == Some(decoded_dimensions.1);
        if sized_response {
            self.cache_scaled(
                (image.id, decoded_dimensions.0, decoded_dimensions.1),
                Arc::clone(&pixels),
            );
        } else {
            // A new source replaces this resource's variants only. Display-sized
            // replies above retain the resource identity, its other sizes, and
            // every other image on the page.
            self.remove_scaled_image(image.id);
            if let Some(previous) = image.pixels.take() {
                self.source_image_bytes = self
                    .source_image_bytes
                    .saturating_sub(previous.retained_bytes());
            }
            image.id = id;
            self.source_image_bytes += pixels.retained_bytes();
            image.last_source_use = self.use_clock;
            image.pixels = Some(pixels);
        }
        self.images.insert(key, image);
        self.evict_source_images();
        Some(dimensions_changed)
    }

    pub fn fail_metadata(&mut self, request: &ImageRequest) {
        let key = request.key();
        let id = self.allocate_image_id();
        let image = self
            .images
            .entry(key)
            .or_insert_with(|| CachedImage::unknown(id, request.clone()));
        if image.dimensions.is_none() {
            image.metadata_request = RequestState::Failed;
        }
    }

    pub fn fail_pixels(&mut self, request: &ImageRequest) {
        let key = request.key();
        let id = self.allocate_image_id();
        let image = self
            .images
            .entry(key)
            .or_insert_with(|| CachedImage::unknown(id, request.clone()));
        if image.pixels.is_none() {
            image.pixels_request = RequestState::Failed;
        }
    }

    pub fn retry(&mut self, request: &ImageRequest) {
        if let Some(image) = self.images.get_mut(&request.key()) {
            if image.dimensions.is_none() {
                image.metadata_request = RequestState::Unknown;
            }
            if image.pixels.is_none() {
                image.pixels_request = RequestState::Unknown;
            }
        }
    }

    pub fn measure(&mut self, url: &str, base: Option<&str>) -> Option<ImageMetrics> {
        let request = self.request(url, base);
        let key = request.key();

        let id = self.allocate_image_id();
        let image = self
            .images
            .entry(key.clone())
            .or_insert_with(|| CachedImage::unknown(id, request));
        if let Some((width, height)) = image.dimensions {
            return Some(ImageMetrics {
                key,
                width: width as f32,
                height: height as f32,
            });
        }
        if image.metadata_request == RequestState::Unknown {
            image.metadata_request = RequestState::Pending;
            self.metadata_requests.push(image.request.clone());
        }
        None
    }

    fn pixels_with_id(
        &mut self,
        key: &str,
        width: u32,
        height: u32,
    ) -> Option<(u64, Arc<ImagePixels>)> {
        let image_id = self.images.get(key)?.id;
        let cache_key = (image_id, width, height);
        if let Some(pixels) = self.scaled_images.get(&cache_key) {
            return Some((image_id, Arc::clone(pixels)));
        }

        self.use_clock = self.use_clock.wrapping_add(1);
        let image = self.images.get_mut(key)?;
        if let Some(pixels) = &image.pixels {
            image.last_source_use = self.use_clock;
            return Some((image.id, Arc::clone(pixels)));
        }
        if image.dimensions.is_some() && image.pixels_request == RequestState::Unknown {
            image.pixels_request = RequestState::Pending;
            self.pixel_requests
                .push(image.request.for_pixels(width, height));
        }
        None
    }

    #[cfg(test)]
    pub fn pixels(&mut self, key: &str) -> Option<Arc<ImagePixels>> {
        let dimensions = self.images.get(key)?.dimensions?;
        self.pixels_with_id(key, dimensions.0, dimensions.1)
            .map(|(_, pixels)| pixels)
    }

    pub fn scaled_pixels(
        &mut self,
        key: &str,
        width: u32,
        height: u32,
    ) -> Option<Arc<ImagePixels>> {
        if !valid_dimensions(width, height) {
            return None;
        }
        let (image_id, pixels) = self.pixels_with_id(key, width, height)?;
        if pixels.width() == width && pixels.height() == height {
            let cache_key = (image_id, width, height);
            if !self.scaled_images.contains(&cache_key)
                && self
                    .images
                    .get(key)
                    .is_some_and(|image| image.pixels.is_none())
            {
                self.cache_scaled(cache_key, Arc::clone(&pixels));
            }
            return Some(pixels);
        }

        let cache_key = (image_id, width, height);
        let scaled = Arc::new(ImagePixels::from(pixels.resized_rgba(width, height)));
        self.cache_scaled(cache_key, Arc::clone(&scaled));
        Some(scaled)
    }

    pub fn take_metadata_requests(&mut self) -> Vec<ImageRequest> {
        std::mem::take(&mut self.metadata_requests)
    }

    pub fn take_pixel_requests(&mut self) -> Vec<ImageRequest> {
        std::mem::take(&mut self.pixel_requests)
    }
}

fn valid_dimensions(width: u32, height: u32) -> bool {
    width != 0 && height != 0 && u64::from(width) * u64::from(height) <= MAX_IMAGE_PIXELS
}

fn resource_key(url: &str, base: Option<&str>) -> String {
    let base = base.unwrap_or_default();
    format!("\0resource:{}:{base}{url}", base.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(base: &str) -> ImageRequest {
        ImageRequest::new("images/diagram.png", Some(base.to_owned()))
    }

    #[test]
    fn metadata_and_pixels_are_requested_in_two_phases() {
        let mut cache = ImageCache::default();
        let request = request("https://example.com/docs/");
        assert!(
            cache
                .measure(&request.url, request.resource_base.as_deref())
                .is_none()
        );
        assert_eq!(cache.take_metadata_requests(), vec![request.clone()]);
        assert!(
            cache
                .measure(&request.url, request.resource_base.as_deref())
                .is_none()
        );
        assert!(cache.take_metadata_requests().is_empty());

        assert!(cache.provide_metadata(request.clone(), 3, 2));
        let metrics = cache
            .measure(&request.url, request.resource_base.as_deref())
            .expect("metadata is available");
        assert_eq!((metrics.width, metrics.height), (3.0, 2.0));
        assert!(cache.pixels(&metrics.key).is_none());
        let pixel_request = request.for_pixels(3, 2);
        assert_eq!(cache.take_pixel_requests(), vec![pixel_request.clone()]);
        assert!(cache.pixels(&metrics.key).is_none());
        assert!(cache.take_pixel_requests().is_empty());

        assert_eq!(
            cache.provide_pixels(
                pixel_request,
                RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 255]))
            ),
            Some(false)
        );
        assert_eq!(
            cache.pixels(&metrics.key).unwrap().pixels()[..4],
            [1, 2, 3, 255]
        );
    }

    #[test]
    fn registered_images_are_resolved_by_url_and_resource_base() {
        let mut cache = ImageCache::default();
        let request = request("https://example.com/docs/");
        assert!(cache.register(
            request.clone(),
            RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 255])),
        ));
        let metrics = cache
            .measure(&request.url, request.resource_base.as_deref())
            .expect("registered image is available");
        assert_eq!((metrics.width, metrics.height), (3.0, 2.0));
        assert!(
            cache
                .measure("images/diagram.png", Some("https://example.com/other/"))
                .is_none()
        );
    }

    #[test]
    fn scaled_pixels_are_reused_and_invalidated_with_the_source() {
        let mut cache = ImageCache::default();
        let request = request("https://example.com/docs/");
        assert!(cache.register(
            request.clone(),
            RgbaImage::from_pixel(4, 2, image::Rgba([1, 2, 3, 255])),
        ));
        let key = cache
            .measure(&request.url, request.resource_base.as_deref())
            .unwrap()
            .key;

        let first = cache.scaled_pixels(&key, 2, 1).unwrap();
        let second = cache.scaled_pixels(&key, 2, 1).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.resource_id(), second.resource_id());
        assert_eq!(cache.scaled_images.len(), 1);

        assert_eq!(
            cache.provide_pixels(
                request,
                RgbaImage::from_pixel(4, 2, image::Rgba([9, 8, 7, 255])),
            ),
            Some(false),
        );
        assert!(cache.scaled_images.is_empty());
        let replacement = cache.scaled_pixels(&key, 2, 1).unwrap();
        assert!(!Arc::ptr_eq(&first, &replacement));
        assert_ne!(first.resource_id(), replacement.resource_id());
        assert_eq!(&replacement.pixels()[..4], &[9, 8, 7, 255]);
    }

    #[test]
    fn sized_pixels_preserve_intrinsic_dimensions_and_avoid_a_source_copy() {
        let mut cache = ImageCache::default();
        let request = request("https://example.com/docs/");
        assert!(cache.provide_metadata(request.clone(), 4_000, 2_000));
        let key = cache
            .measure(&request.url, request.resource_base.as_deref())
            .unwrap()
            .key;
        assert!(cache.scaled_pixels(&key, 800, 400).is_none());
        let pixel_request = cache.take_pixel_requests().pop().unwrap();
        assert_eq!(
            (pixel_request.desired_width, pixel_request.desired_height),
            (Some(800), Some(400))
        );
        assert_eq!(
            cache.provide_pixels(
                pixel_request,
                RgbaImage::from_pixel(800, 400, image::Rgba([1, 2, 3, 255])),
            ),
            Some(false),
        );
        assert_eq!(cache.images[&key].dimensions, Some((4_000, 2_000)));
        assert_eq!(cache.source_image_bytes, 0);
        assert_eq!(cache.scaled_image_bytes, 800 * 400 * 4);
        assert!(cache.scaled_pixels(&key, 800, 400).is_some());
        assert!(cache.take_pixel_requests().is_empty());
    }

    #[test]
    fn multiple_sized_images_and_variants_do_not_evict_each_other() {
        let mut cache = ImageCache::default();
        let a = ImageRequest::new("a.png", None);
        let b = ImageRequest::new("b.png", None);
        assert!(cache.provide_metadata(a.clone(), 100, 100));
        assert!(cache.provide_metadata(b.clone(), 100, 100));
        for (request, size) in [(&a, 20), (&b, 30), (&a, 40)] {
            assert!(cache.scaled_pixels(&request.key(), size, size).is_none());
            let pending = cache.take_pixel_requests().pop().unwrap();
            assert_eq!(
                cache.provide_pixels(pending, RgbaImage::new(size, size)),
                Some(false)
            );
        }
        for _ in 0..3 {
            for (request, size) in [(&a, 20), (&b, 30), (&a, 40)] {
                assert!(cache.scaled_pixels(&request.key(), size, size).is_some());
            }
            assert!(cache.take_pixel_requests().is_empty());
        }
        assert_eq!(cache.scaled_image_bytes, (20 * 20 + 30 * 30 + 40 * 40) * 4);
        assert!(cache.register(a.clone(), RgbaImage::new(100, 100)));
        assert!(cache.scaled_pixels(&b.key(), 30, 30).is_some());
        assert!(cache.take_pixel_requests().is_empty());
        assert_eq!(cache.scaled_images.len(), 1);
        let source_variant = cache.scaled_pixels(&a.key(), 50, 50).unwrap();
        assert_eq!(
            cache.provide_pixels(a.for_pixels(60, 60), RgbaImage::new(60, 60)),
            Some(false)
        );
        assert_eq!(cache.source_image_bytes, 100 * 100 * 4);
        assert!(Arc::ptr_eq(
            &source_variant,
            &cache.scaled_pixels(&a.key(), 50, 50).unwrap()
        ));
        assert!(cache.scaled_pixels(&b.key(), 30, 30).is_some());
    }

    #[test]
    fn sized_host_pixels_retain_shared_padded_bgra_backing_without_repacking() {
        let mut cache = ImageCache::default();
        let request = request("https://example.com/docs/");
        assert!(cache.provide_metadata(request.clone(), 2, 2));
        let key = cache
            .measure(&request.url, request.resource_base.as_deref())
            .unwrap()
            .key;
        assert!(cache.scaled_pixels(&key, 2, 2).is_none());
        let pixel_request = cache.take_pixel_requests().pop().unwrap();

        let backing: Arc<[u8]> = Arc::from([
            30, 20, 10, 255, 60, 50, 40, 255, 0xaa, 0xaa, 0xaa, 0xaa, 90, 80, 70, 255, 120, 110,
            100, 255,
        ]);
        let backing_address = backing.as_ptr();
        let pixels =
            ImagePixels::from_backing(2, 2, 12, PixelFormat::Bgra8888, Arc::clone(&backing))
                .unwrap();
        assert_eq!(cache.provide_pixels(pixel_request, pixels), Some(false));

        let retained = cache.scaled_pixels(&key, 2, 2).unwrap();
        assert_eq!(retained.pixels().as_ptr(), backing_address);
        assert_eq!(retained.stride(), 12);
        assert_eq!(retained.format(), PixelFormat::Bgra8888);
        assert_eq!(cache.scaled_image_bytes, backing.len());
    }

    #[test]
    fn source_and_scaled_caches_are_byte_bounded_and_trimmable() {
        let mut cache = ImageCache::default();
        for index in 0..6 {
            assert!(cache.register(
                ImageRequest::new(format!("{index}.png"), None),
                RgbaImage::new(1_024, 1_024),
            ));
        }
        assert!(cache.source_image_bytes <= SOURCE_IMAGE_CACHE_BUDGET);
        cache.trim();
        assert_eq!(cache.source_image_bytes, 0);
        assert_eq!(cache.scaled_image_bytes, 0);
    }

    #[test]
    fn failed_requests_can_be_retried() {
        let mut cache = ImageCache::default();
        let request = request("https://example.com/docs/");
        cache.measure(&request.url, request.resource_base.as_deref());
        cache.take_metadata_requests();
        cache.fail_metadata(&request);
        cache.measure(&request.url, request.resource_base.as_deref());
        assert!(cache.take_metadata_requests().is_empty());
        cache.retry(&request);
        cache.measure(&request.url, request.resource_base.as_deref());
        assert_eq!(cache.take_metadata_requests(), vec![request]);
    }

    #[test]
    fn rejects_empty_registered_images() {
        assert!(
            !ImageCache::default()
                .register(ImageRequest::new("empty.png", None), RgbaImage::new(0, 1))
        );
    }

    #[test]
    fn local_references_are_host_requests_and_never_replace_registered_pixels() {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        let path = format!("{base}/images/landscape.png");
        for request in [
            ImageRequest::new("images/landscape.png", Some(base.into())),
            ImageRequest::new(path.clone(), None),
            ImageRequest::new(format!("file://{path}"), None),
        ] {
            let mut cache = ImageCache::default();
            assert!(
                cache
                    .measure(&request.url, request.resource_base.as_deref())
                    .is_none()
            );
            assert_eq!(cache.take_metadata_requests(), vec![request.clone()]);
            assert!(cache.register(
                request.clone(),
                RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]))
            ));
            let measured = cache
                .measure(&request.url, request.resource_base.as_deref())
                .unwrap();
            assert_eq!((measured.width, measured.height), (4.0, 4.0));
            assert_eq!(
                &cache.pixels(&measured.key).unwrap().pixels()[..4],
                &[1, 2, 3, 255]
            );
            assert!(cache.take_metadata_requests().is_empty());
        }
    }
}
