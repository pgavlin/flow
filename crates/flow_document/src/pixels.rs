use anyhow::{Context as _, Result};

const MASK_BLOCK_PIXELS: usize = 8;
const BYTES_PER_PIXEL: usize = 4;
const MASK_BLOCK_BYTES: usize = MASK_BLOCK_PIXELS * BYTES_PER_PIXEL;

/// The channel order of four-byte render or image pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    /// Blue, green, red, alpha bytes. Render targets currently write opaque alpha.
    Bgra8888,
    /// Red, green, blue, alpha bytes. Render targets currently write opaque alpha.
    Rgba8888,
}

/// A process-local identity for a reusable paint resource.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PaintResourceId(pub [u8; 16]);

/// The production pixel-compositing boundary used by renderer traversal.
///
/// Implementations retain dimensions, translation, and clipping semantics. The
/// normal renderer monomorphizes this trait for [`PixelSurface`]. Hosts can
/// implement the same operations for their own presentation targets.
pub trait PaintTarget {
    type ViewportState;

    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn clear(&mut self, color: [u8; 3]);
    fn push_viewport(&mut self, origin: (i32, i32), width: u32, height: u32)
    -> Self::ViewportState;
    fn pop_viewport(&mut self, state: Self::ViewportState);
    fn fill_rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: [u8; 3]);
    fn draw_line(&mut self, start: (f32, f32), end: (f32, f32), thickness: f32, color: [u8; 3]);
    fn blend_mask(
        &mut self,
        origin: (i32, i32),
        width: u32,
        height: u32,
        mask: &[u8],
        color: [u8; 3],
    );
    fn blend_mask_resource(
        &mut self,
        _resource: PaintResourceId,
        origin: (i32, i32),
        width: u32,
        height: u32,
        mask: &[u8],
        color: [u8; 3],
    ) {
        self.blend_mask(origin, width, height, mask, color);
    }
    fn blend_pixels(
        &mut self,
        position: (i32, i32),
        width: u32,
        height: u32,
        source_stride: usize,
        source_format: PixelFormat,
        source_pixels: &[u8],
    );
    #[allow(clippy::too_many_arguments)]
    fn blend_pixels_resource(
        &mut self,
        _resource: PaintResourceId,
        position: (i32, i32),
        width: u32,
        height: u32,
        source_stride: usize,
        source_format: PixelFormat,
        source_pixels: &[u8],
    ) {
        self.blend_pixels(
            position,
            width,
            height,
            source_stride,
            source_format,
            source_pixels,
        );
    }

    fn blend_rgba(&mut self, x: i32, y: i32, width: u32, height: u32, rgba: &[u8]) {
        self.blend_pixels(
            (x, y),
            width,
            height,
            width as usize * BYTES_PER_PIXEL,
            PixelFormat::Rgba8888,
            rgba,
        );
    }

    fn with_viewport<R>(
        &mut self,
        origin: (i32, i32),
        width: u32,
        height: u32,
        draw: impl FnOnce(&mut Self) -> R,
    ) -> R
    where
        Self: Sized,
    {
        let state = self.push_viewport(origin, width, height);
        let result = draw(self);
        self.pop_viewport(state);
        result
    }
}

#[cfg(test)]
pub(crate) mod test_support;

/// An owned, tightly packed four-byte render target.
///
/// Hosts inspect the dimensions, stride, format, and pixels to encode images
/// or present them. File encoding and storage are host responsibilities.
pub struct PixelBuffer {
    width: u32,
    height: u32,
    format: PixelFormat,
    pixels: Vec<u8>,
}

impl Default for PixelBuffer {
    fn default() -> Self {
        Self::with_format(PixelFormat::Bgra8888)
    }
}

impl PixelBuffer {
    pub fn new(width: u32, height: u32, color: [u8; 3]) -> Result<Self> {
        Self::new_with_format(width, height, color, PixelFormat::Bgra8888)
    }

    pub fn new_with_format(
        width: u32,
        height: u32,
        color: [u8; 3],
        format: PixelFormat,
    ) -> Result<Self> {
        let mut buffer = Self::with_format(format);
        buffer.resize(width, height)?;
        buffer.surface_mut().clear(color);
        Ok(buffer)
    }

    pub fn with_format(format: PixelFormat) -> Self {
        Self {
            width: 0,
            height: 0,
            format,
            pixels: Vec::new(),
        }
    }

    /// Resizes the backing allocation without defining its pixel contents.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let len = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(height as usize))
            .and_then(|pixels| pixels.checked_mul(BYTES_PER_PIXEL))
            .context("render surface is too large")?;
        i32::try_from(width).context("render surface width is too large")?;
        i32::try_from(height).context("render surface height is too large")?;
        self.pixels.resize(len, 0);
        self.width = width;
        self.height = height;
        Ok(())
    }

    pub fn resize_and_clear(&mut self, width: u32, height: u32, color: [u8; 3]) -> Result<()> {
        self.resize(width, height)?;
        self.surface_mut().clear(color);
        Ok(())
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn stride(&self) -> usize {
        self.width as usize * BYTES_PER_PIXEL
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn surface_mut(&mut self) -> PixelSurface<'_> {
        PixelSurface {
            width: self.width,
            height: self.height,
            stride: self.width as usize * BYTES_PER_PIXEL,
            format: self.format,
            pixels: &mut self.pixels,
            origin: (0, 0),
            clip: (0, 0, self.width as i32, self.height as i32),
        }
    }

    /// Returns writable access to the format-specific pixels for immediate
    /// host presentation. Hosts must not retain a pointer across another render.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }
}

/// A short-lived mutable view over host-owned four-byte pixels.
pub struct PixelSurface<'a> {
    width: u32,
    height: u32,
    stride: usize,
    format: PixelFormat,
    pixels: &'a mut [u8],
    origin: (i32, i32),
    clip: (i32, i32, i32, i32),
}

impl<'a> PixelSurface<'a> {
    pub fn new(
        pixels: &'a mut [u8],
        width: u32,
        height: u32,
        stride: usize,
        format: PixelFormat,
    ) -> Result<Self> {
        let row_bytes = (width as usize)
            .checked_mul(BYTES_PER_PIXEL)
            .context("render surface is too large")?;
        if stride < row_bytes {
            anyhow::bail!("render surface stride is smaller than its width");
        }
        let required = if height == 0 {
            0
        } else {
            (height as usize - 1)
                .checked_mul(stride)
                .and_then(|offset| offset.checked_add(row_bytes))
                .context("render surface is too large")?
        };
        if pixels.len() < required {
            anyhow::bail!("render surface storage is too small");
        }
        let clip_width = i32::try_from(width).context("render surface width is too large")?;
        let clip_height = i32::try_from(height).context("render surface height is too large")?;
        Ok(Self {
            width,
            height,
            stride,
            format,
            pixels,
            origin: (0, 0),
            clip: (0, 0, clip_width, clip_height),
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

    pub fn clear(&mut self, color: [u8; 3]) {
        let pixel = self.packed_color(color);
        let row_bytes = self.width as usize * BYTES_PER_PIXEL;
        for y in 0..self.height as usize {
            let start = y * self.stride;
            for target in self.pixels[start..start + row_bytes].chunks_exact_mut(4) {
                target.copy_from_slice(&pixel);
            }
        }
        self.origin = (0, 0);
        self.clip = (0, 0, self.width as i32, self.height as i32);
    }

    pub fn with_viewport<R>(
        &mut self,
        origin: (i32, i32),
        width: u32,
        height: u32,
        draw: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let previous_origin = self.origin;
        let previous_clip = self.clip;
        self.origin = (
            previous_origin.0.saturating_add(origin.0),
            previous_origin.1.saturating_add(origin.1),
        );
        let right = self.origin.0.saturating_add_unsigned(width);
        let bottom = self.origin.1.saturating_add_unsigned(height);
        self.clip = (
            previous_clip.0.max(self.origin.0),
            previous_clip.1.max(self.origin.1),
            previous_clip.2.min(right),
            previous_clip.3.min(bottom),
        );
        let result = draw(self);
        self.origin = previous_origin;
        self.clip = previous_clip;
        result
    }

    fn packed_color(&self, color: [u8; 3]) -> [u8; 4] {
        match self.format {
            PixelFormat::Bgra8888 => [color[2], color[1], color[0], 0xff],
            PixelFormat::Rgba8888 => [color[0], color[1], color[2], 0xff],
        }
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: [u8; 3]) {
        let x = x + self.origin.0 as f32;
        let y = y + self.origin.1 as f32;
        let x0 = x.floor().max(self.clip.0 as f32) as u32;
        let y0 = y.floor().max(self.clip.1 as f32) as u32;
        let x1 = (x + width).ceil().min(self.clip.2 as f32).max(0.0) as u32;
        let y1 = (y + height).ceil().min(self.clip.3 as f32).max(0.0) as u32;
        let color = self.packed_color(color);
        for row in y0..y1 {
            for column in x0..x1 {
                let offset = row as usize * self.stride + column as usize * BYTES_PER_PIXEL;
                self.pixels[offset..offset + 4].copy_from_slice(&color);
            }
        }
    }

    pub fn draw_line(
        &mut self,
        start: (f32, f32),
        end: (f32, f32),
        thickness: f32,
        color: [u8; 3],
    ) {
        let start = (
            start.0 + self.origin.0 as f32,
            start.1 + self.origin.1 as f32,
        );
        let end = (end.0 + self.origin.0 as f32, end.1 + self.origin.1 as f32);
        let radius = thickness.max(1.0) / 2.0;
        let x0 = (start.0.min(end.0) - radius)
            .floor()
            .max(self.clip.0 as f32) as u32;
        let y0 = (start.1.min(end.1) - radius)
            .floor()
            .max(self.clip.1 as f32) as u32;
        let x1 = (start.0.max(end.0) + radius)
            .ceil()
            .min(self.clip.2 as f32)
            .max(0.0) as u32;
        let y1 = (start.1.max(end.1) + radius)
            .ceil()
            .min(self.clip.3 as f32)
            .max(0.0) as u32;
        let delta = (end.0 - start.0, end.1 - start.1);
        let length_squared = delta.0 * delta.0 + delta.1 * delta.1;
        let color = self.packed_color(color);
        for y in y0..y1 {
            for x in x0..x1 {
                let point = (x as f32 + 0.5, y as f32 + 0.5);
                let projection = if length_squared == 0.0 {
                    0.0
                } else {
                    (((point.0 - start.0) * delta.0 + (point.1 - start.1) * delta.1)
                        / length_squared)
                        .clamp(0.0, 1.0)
                };
                let nearest = (
                    start.0 + projection * delta.0,
                    start.1 + projection * delta.1,
                );
                let distance_squared =
                    (point.0 - nearest.0).powi(2) + (point.1 - nearest.1).powi(2);
                if distance_squared <= radius * radius {
                    let offset = y as usize * self.stride + x as usize * BYTES_PER_PIXEL;
                    self.pixels[offset..offset + 4].copy_from_slice(&color);
                }
            }
        }
    }

    pub fn blend_mask(
        &mut self,
        origin: (i32, i32),
        width: u32,
        height: u32,
        mask: &[u8],
        color: [u8; 3],
    ) {
        let origin = (
            origin.0.saturating_add(self.origin.0),
            origin.1.saturating_add(self.origin.1),
        );
        let width = width.min(i32::MAX as u32) as i32;
        let height = height.min(i32::MAX as u32) as i32;
        let source_x0 = self.clip.0.saturating_sub(origin.0).clamp(0, width) as u32;
        let source_y0 = self.clip.1.saturating_sub(origin.1).clamp(0, height) as u32;
        let source_x1 = self.clip.2.saturating_sub(origin.0).clamp(0, width) as u32;
        let source_y1 = self.clip.3.saturating_sub(origin.1).clamp(0, height) as u32;
        let width = width as u32;
        if source_x0 >= source_x1 || source_y0 >= source_y1 {
            return;
        }

        let packed = self.packed_color(color);
        let color = [packed[0], packed[1], packed[2]];
        let block_color = packed;
        let source_stride = width as usize;
        let row_width = (source_x1 - source_x0) as usize;
        let target_stride = self.stride;
        let target_row_bytes = row_width * BYTES_PER_PIXEL;

        let block_pixels = row_width / MASK_BLOCK_PIXELS * MASK_BLOCK_PIXELS;
        let block_bytes = block_pixels * BYTES_PER_PIXEL;
        let remainder = row_width - block_pixels;
        let has_four = remainder & 4 != 0;
        let has_two = remainder & 2 != 0;
        let has_one = remainder & 1 != 0;
        let two_offset = if has_four { 4 } else { 0 };
        let one_offset = two_offset + if has_two { 2 } else { 0 };

        let mut source_start = source_y0 as usize * source_stride + source_x0 as usize;
        let target_y = (origin.1 + source_y0 as i32) as usize;
        let target_x = (origin.0 + source_x0 as i32) as usize;
        let mut target_start = target_y * target_stride + target_x * BYTES_PER_PIXEL;

        for _ in source_y0..source_y1 {
            let source_end = source_start + row_width;
            let target_end = target_start + target_row_bytes;
            let mask_row = &mask[source_start..source_end];
            let target_row = &mut self.pixels[target_start..target_end];
            let (mask_blocks, mask_tail) = mask_row.split_at(block_pixels);
            let (target_blocks, target_tail) = target_row.split_at_mut(block_bytes);

            for (mask, pixels) in mask_blocks
                .chunks_exact(MASK_BLOCK_PIXELS)
                .zip(target_blocks.chunks_exact_mut(MASK_BLOCK_BYTES))
            {
                blend_mask_block_8(
                    mask.try_into().expect("mask block has eight pixels"),
                    pixels.try_into().expect("target block has eight pixels"),
                    block_color,
                );
            }
            if has_four {
                blend_mask_block_4(
                    mask_tail[..4]
                        .try_into()
                        .expect("mask block has four pixels"),
                    (&mut target_tail[..16])
                        .try_into()
                        .expect("target block has four pixels"),
                    block_color,
                );
            }
            if has_two {
                let target_offset = two_offset * BYTES_PER_PIXEL;
                blend_mask_block_2(
                    mask_tail[two_offset..two_offset + 2]
                        .try_into()
                        .expect("mask block has two pixels"),
                    (&mut target_tail[target_offset..target_offset + 8])
                        .try_into()
                        .expect("target block has two pixels"),
                    block_color,
                );
            }
            if has_one {
                let alpha = u32::from(mask_tail[one_offset]);
                let target_offset = one_offset * BYTES_PER_PIXEL;
                let [blue, green, red, _] =
                    &mut target_tail[target_offset..target_offset + BYTES_PER_PIXEL]
                else {
                    unreachable!();
                };
                if alpha == 255 {
                    *blue = color[0];
                    *green = color[1];
                    *red = color[2];
                } else if alpha != 0 {
                    let inverse = 255 - alpha;
                    *blue = blend_channel(color[0], *blue, alpha, inverse);
                    *green = blend_channel(color[1], *green, alpha, inverse);
                    *red = blend_channel(color[2], *red, alpha, inverse);
                }
            }

            source_start += source_stride;
            target_start += target_stride;
        }
    }

    pub fn copy_from(&mut self, source: &PixelBuffer, x: i32, y: i32) {
        let x = x.saturating_add(self.origin.0);
        let y = y.saturating_add(self.origin.1);
        for source_y in 0..source.height {
            let target_y = y + source_y as i32;
            if !(self.clip.1..self.clip.3).contains(&target_y) {
                continue;
            }
            let source_x0 = (self.clip.0 - x).max(0) as u32;
            let source_x1 = source.width.min((self.clip.2 - x).max(0) as u32);
            if source_x0 >= source_x1 {
                continue;
            }
            let source_start = ((source_y * source.width + source_x0) * 4) as usize;
            let source_end = ((source_y * source.width + source_x1) * 4) as usize;
            let target_x = x.max(self.clip.0) as u32;
            let target_start =
                target_y as usize * self.stride + target_x as usize * BYTES_PER_PIXEL;
            let target_end = target_start + source_end - source_start;
            if self.format == source.format {
                self.pixels[target_start..target_end]
                    .copy_from_slice(&source.pixels[source_start..source_end]);
            } else {
                for (target, source) in self.pixels[target_start..target_end]
                    .chunks_exact_mut(4)
                    .zip(source.pixels[source_start..source_end].chunks_exact(4))
                {
                    target.copy_from_slice(&[source[2], source[1], source[0], source[3]]);
                }
            }
        }
    }

    pub fn blend_rgba(&mut self, x: i32, y: i32, width: u32, height: u32, rgba: &[u8]) {
        self.blend_pixels(
            (x, y),
            width,
            height,
            width as usize * BYTES_PER_PIXEL,
            PixelFormat::Rgba8888,
            rgba,
        );
    }

    /// Blends four-channel source pixels, honoring their channel order and row stride.
    pub fn blend_pixels(
        &mut self,
        position: (i32, i32),
        width: u32,
        height: u32,
        source_stride: usize,
        source_format: PixelFormat,
        source_pixels: &[u8],
    ) {
        let Some(row_bytes) = (width as usize).checked_mul(BYTES_PER_PIXEL) else {
            return;
        };
        let Some(required) = (height as usize)
            .checked_sub(1)
            .and_then(|rows| rows.checked_mul(source_stride))
            .and_then(|offset| offset.checked_add(row_bytes))
        else {
            return;
        };
        if width == 0 || height == 0 || source_stride < row_bytes || source_pixels.len() < required
        {
            return;
        }
        let x = position.0.saturating_add(self.origin.0);
        let y = position.1.saturating_add(self.origin.1);
        let channels = match (self.format, source_format) {
            (PixelFormat::Bgra8888, PixelFormat::Rgba8888)
            | (PixelFormat::Rgba8888, PixelFormat::Bgra8888) => [(0, 2), (1, 1), (2, 0)],
            _ => [(0, 0), (1, 1), (2, 2)],
        };
        for source_y in 0..height {
            let target_y = y + source_y as i32;
            if !(self.clip.1..self.clip.3).contains(&target_y) {
                continue;
            }
            for source_x in 0..width {
                let target_x = x + source_x as i32;
                if !(self.clip.0..self.clip.2).contains(&target_x) {
                    continue;
                }
                let source_offset =
                    source_y as usize * source_stride + source_x as usize * BYTES_PER_PIXEL;
                let alpha = source_pixels[source_offset + 3] as u16;
                let target_offset =
                    target_y as usize * self.stride + target_x as usize * BYTES_PER_PIXEL;
                for (target_channel, source_channel) in channels {
                    let source = source_pixels[source_offset + source_channel] as u16;
                    let destination = self.pixels[target_offset + target_channel] as u16;
                    self.pixels[target_offset + target_channel] =
                        ((source * alpha + destination * (255 - alpha) + 127) / 255) as u8;
                }
            }
        }
    }
}

impl PaintTarget for PixelSurface<'_> {
    type ViewportState = ((i32, i32), (i32, i32, i32, i32));

    fn width(&self) -> u32 {
        PixelSurface::width(self)
    }

    fn height(&self) -> u32 {
        PixelSurface::height(self)
    }

    fn clear(&mut self, color: [u8; 3]) {
        PixelSurface::clear(self, color);
    }

    fn push_viewport(
        &mut self,
        origin: (i32, i32),
        width: u32,
        height: u32,
    ) -> Self::ViewportState {
        let state = (self.origin, self.clip);
        self.origin = (
            self.origin.0.saturating_add(origin.0),
            self.origin.1.saturating_add(origin.1),
        );
        let right = self.origin.0.saturating_add_unsigned(width);
        let bottom = self.origin.1.saturating_add_unsigned(height);
        self.clip = (
            self.clip.0.max(self.origin.0),
            self.clip.1.max(self.origin.1),
            self.clip.2.min(right),
            self.clip.3.min(bottom),
        );
        state
    }

    fn pop_viewport(&mut self, state: Self::ViewportState) {
        (self.origin, self.clip) = state;
    }

    fn fill_rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: [u8; 3]) {
        PixelSurface::fill_rect(self, x, y, width, height, color);
    }

    fn draw_line(&mut self, start: (f32, f32), end: (f32, f32), thickness: f32, color: [u8; 3]) {
        PixelSurface::draw_line(self, start, end, thickness, color);
    }

    fn blend_mask(
        &mut self,
        origin: (i32, i32),
        width: u32,
        height: u32,
        mask: &[u8],
        color: [u8; 3],
    ) {
        PixelSurface::blend_mask(self, origin, width, height, mask, color);
    }

    fn blend_pixels(
        &mut self,
        position: (i32, i32),
        width: u32,
        height: u32,
        source_stride: usize,
        source_format: PixelFormat,
        source_pixels: &[u8],
    ) {
        PixelSurface::blend_pixels(
            self,
            position,
            width,
            height,
            source_stride,
            source_format,
            source_pixels,
        );
    }
}

// Fixed-size, branchless inputs let LLVM's SLP vectorizer lower this same safe
// Rust to NEON, x86 SIMD, and WASM SIMD128 while retaining a scalar fallback.
macro_rules! define_blend_mask_block {
    ($name:ident, $pixels:expr, $bytes:expr) => {
        #[inline(always)]
        fn $name(
            mask: &[u8; $pixels],
            destination: &mut [u8; $bytes],
            color: [u8; BYTES_PER_PIXEL],
        ) {
            for pixel_index in 0..$pixels {
                let alpha = u16::from(mask[pixel_index]);
                let inverse = 255 - alpha;
                for channel in 0..BYTES_PER_PIXEL {
                    let offset = pixel_index * BYTES_PER_PIXEL + channel;
                    let sum = u16::from(color[channel]) * alpha
                        + u16::from(destination[offset]) * inverse;
                    destination[offset] = divide_255_rounded(sum);
                }
            }
        }
    };
}

define_blend_mask_block!(blend_mask_block_8, MASK_BLOCK_PIXELS, MASK_BLOCK_BYTES);
define_blend_mask_block!(blend_mask_block_4, 4, 16);
define_blend_mask_block!(blend_mask_block_2, 2, 8);

#[inline(always)]
fn divide_255_rounded(value: u16) -> u8 {
    let value = value + 128;
    ((value + (value >> 8)) >> 8) as u8
}

#[inline(always)]
fn blend_channel(source: u8, destination: u8, alpha: u32, inverse: u32) -> u8 {
    ((u32::from(source) * alpha + u32::from(destination) * inverse + 127) / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_reuses_storage_when_dimensions_are_unchanged() {
        let mut surface = PixelBuffer::new(8, 6, [1, 2, 3]).unwrap();
        let pixels = surface.pixels().as_ptr();
        surface.resize_and_clear(8, 6, [4, 5, 6]).unwrap();
        assert_eq!(surface.pixels().as_ptr(), pixels);
        assert_eq!(&surface.pixels()[..4], &[6, 5, 4, 255]);
    }

    #[test]
    fn owned_buffers_preserve_their_selected_format() {
        let rgba = PixelBuffer::new_with_format(1, 1, [1, 2, 3], PixelFormat::Rgba8888).unwrap();
        assert_eq!(rgba.format(), PixelFormat::Rgba8888);
        assert_eq!(rgba.pixels(), &[1, 2, 3, 255]);

        let mut bgra = PixelBuffer::new(1, 1, [0, 0, 0]).unwrap();
        bgra.surface_mut().copy_from(&rgba, 0, 0);
        assert_eq!(bgra.pixels(), &[3, 2, 1, 255]);
    }

    #[test]
    fn borrowed_rgba_surface_honors_stride_and_preserves_padding() {
        let mut pixels = [0xaa; 24];
        {
            let mut surface =
                PixelSurface::new(&mut pixels, 2, 2, 12, PixelFormat::Rgba8888).unwrap();
            surface.clear([1, 2, 3]);
            surface.fill_rect(1.0, 0.0, 1.0, 1.0, [9, 8, 7]);
        }

        assert_eq!(&pixels[..8], &[1, 2, 3, 255, 9, 8, 7, 255]);
        assert_eq!(&pixels[8..12], &[0xaa; 4]);
        assert_eq!(&pixels[12..20], &[1, 2, 3, 255, 1, 2, 3, 255]);
        assert_eq!(&pixels[20..24], &[0xaa; 4]);
    }

    #[test]
    fn borrowed_rgba_surface_blends_masks_and_images_in_rgba_order() {
        let mut pixels = [0; 8];
        {
            let mut surface =
                PixelSurface::new(&mut pixels, 2, 1, 8, PixelFormat::Rgba8888).unwrap();
            surface.clear([10, 20, 30]);
            surface.blend_mask((0, 0), 1, 1, &[255], [110, 70, 50]);
            surface.blend_rgba(1, 0, 1, 1, &[90, 80, 70, 255]);
        }

        assert_eq!(pixels, [110, 70, 50, 255, 90, 80, 70, 255]);
    }

    #[test]
    fn image_blending_honors_bgra_order_and_source_stride() {
        let mut target =
            PixelBuffer::new_with_format(2, 2, [0, 0, 0], PixelFormat::Rgba8888).unwrap();
        let source = [
            30, 20, 10, 255, 60, 50, 40, 255, 0xaa, 0xaa, 0xaa, 0xaa, 90, 80, 70, 255, 120, 110,
            100, 255,
        ];
        target
            .surface_mut()
            .blend_pixels((0, 0), 2, 2, 12, PixelFormat::Bgra8888, &source);
        assert_eq!(
            target.pixels(),
            &[
                10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255
            ]
        );
    }

    #[test]
    fn borrowed_surface_rejects_invalid_storage() {
        let mut pixels = [0; 15];
        assert!(PixelSurface::new(&mut pixels, 2, 2, 7, PixelFormat::Bgra8888).is_err());
        assert!(PixelSurface::new(&mut pixels, 2, 2, 8, PixelFormat::Bgra8888).is_err());
    }

    #[test]
    fn recording_target_preserves_clipping_and_production_operations() {
        use super::test_support::{PaintCommand, RecordingPaintTarget};
        let mut target = RecordingPaintTarget::new(4, 4).unwrap();
        target.clear([255, 255, 255]);
        target.with_viewport((1, 1), 2, 2, |target| {
            target.fill_rect(-10.0, -10.0, 20.0, 20.0, [0, 0, 0]);
            target.blend_mask((0, 0), 2, 2, &[255; 4], [10, 20, 30]);
            target.blend_rgba(0, 0, 1, 1, &[1, 2, 3, 255]);
        });

        assert_eq!(
            target.commands,
            vec![
                PaintCommand::Clear([255; 3]),
                PaintCommand::Viewport((1, 1), 2, 2),
                PaintCommand::Rect(-10.0, -10.0, 20.0, 20.0, [0; 3]),
                PaintCommand::Mask((0, 0), 2, 2, vec![255; 4], [10, 20, 30]),
                PaintCommand::Pixels((0, 0), 1, 1, 4, PixelFormat::Rgba8888, vec![1, 2, 3, 255]),
                PaintCommand::RestoreViewport,
            ]
        );
        let pixel = |x: usize, y: usize| {
            let offset = y * target.buffer.stride() + x * 4;
            &target.buffer.pixels()[offset..offset + 4]
        };
        assert_eq!(pixel(0, 0), [255; 4]);
        assert_eq!(pixel(1, 1), [3, 2, 1, 255]);
        assert_eq!(pixel(2, 2), [30, 20, 10, 255]);
    }

    #[test]
    fn viewport_translates_and_clips_drawing() {
        let mut surface = PixelBuffer::new(4, 4, [255, 255, 255]).unwrap();
        surface
            .surface_mut()
            .with_viewport((1, 1), 2, 2, |surface| {
                surface.fill_rect(-10.0, -10.0, 20.0, 20.0, [0, 0, 0]);
            });
        for y in 0..4 {
            for x in 0..4 {
                let offset = ((y * 4 + x) * 4) as usize;
                let expected = if (1..3).contains(&x) && (1..3).contains(&y) {
                    &[0, 0, 0, 255]
                } else {
                    &[255, 255, 255, 255]
                };
                assert_eq!(&surface.pixels()[offset..offset + 4], expected);
            }
        }
    }

    #[test]
    fn rounded_divide_by_255_matches_the_scalar_formula() {
        for value in 0..=u16::from(u8::MAX) * u16::from(u8::MAX) {
            assert_eq!(
                divide_255_rounded(value),
                ((u32::from(value) + 127) / 255) as u8
            );
        }
    }

    fn expected_block<const PIXELS: usize, const BYTES: usize>(
        mask: &[u8; PIXELS],
        pixels: &[u8; BYTES],
        color: [u8; 4],
    ) -> [u8; BYTES] {
        let mut expected = *pixels;
        for (&mask, pixel) in mask.iter().zip(expected.chunks_exact_mut(4)) {
            let alpha = u32::from(mask);
            let inverse = 255 - alpha;
            for (&source, destination) in color.iter().zip(pixel) {
                *destination = blend_channel(source, *destination, alpha, inverse);
            }
        }
        expected
    }

    #[test]
    fn branchless_mask_blocks_match_per_channel_blending() {
        let color = [50, 70, 110, 255];

        let mask_8 = [0, 1, 64, 127, 128, 192, 254, 255];
        let mut pixels_8 = std::array::from_fn(|index| {
            if index % 4 == 3 {
                255
            } else {
                (index * 7 + 13) as u8
            }
        });
        let expected_8 = expected_block(&mask_8, &pixels_8, color);
        blend_mask_block_8(&mask_8, &mut pixels_8, color);
        assert_eq!(pixels_8, expected_8);

        let mask_4 = [0, 64, 192, 255];
        let mut pixels_4 = std::array::from_fn(|index| {
            if index % 4 == 3 {
                255
            } else {
                (index * 11 + 7) as u8
            }
        });
        let expected_4 = expected_block(&mask_4, &pixels_4, color);
        blend_mask_block_4(&mask_4, &mut pixels_4, color);
        assert_eq!(pixels_4, expected_4);

        let mask_2 = [1, 254];
        let mut pixels_2 = std::array::from_fn(|index| {
            if index % 4 == 3 {
                255
            } else {
                (index * 17 + 3) as u8
            }
        });
        let expected_2 = expected_block(&mask_2, &pixels_2, color);
        blend_mask_block_2(&mask_2, &mut pixels_2, color);
        assert_eq!(pixels_2, expected_2);
    }

    #[test]
    fn mask_blending_uses_fractional_alpha() {
        let mut surface = PixelBuffer::new(1, 1, [10, 20, 30]).unwrap();
        surface
            .surface_mut()
            .blend_mask((0, 0), 1, 1, &[64], [110, 70, 50]);

        let alpha = 64;
        let inverse = 255 - alpha;
        assert_eq!(
            surface.pixels(),
            &[
                blend_channel(50, 30, alpha, inverse),
                blend_channel(70, 20, alpha, inverse),
                blend_channel(110, 10, alpha, inverse),
                255,
            ]
        );
    }

    #[test]
    fn mask_blending_selects_clipped_source_pixels() {
        let mut surface = PixelBuffer::new(3, 3, [10, 20, 30]).unwrap();
        let mask = [0, 64, 128, 192, 255, 32];
        surface
            .surface_mut()
            .blend_mask((-1, 1), 3, 2, &mask, [110, 70, 50]);

        for (x, y, alpha) in [(0, 1, 64), (1, 1, 128), (0, 2, 255), (1, 2, 32)] {
            let offset = ((y * 3 + x) * 4) as usize;
            let inverse = 255 - alpha;
            assert_eq!(
                &surface.pixels()[offset..offset + 4],
                &[
                    blend_channel(50, 30, alpha, inverse),
                    blend_channel(70, 20, alpha, inverse),
                    blend_channel(110, 10, alpha, inverse),
                    255,
                ]
            );
        }
        for (x, y) in [(2, 1), (2, 2), (0, 0), (1, 0), (2, 0)] {
            let offset = ((y * 3 + x) * 4) as usize;
            assert_eq!(&surface.pixels()[offset..offset + 4], &[30, 20, 10, 255]);
        }
    }

    #[test]
    fn mask_blending_respects_viewport_translation_and_clipping() {
        let mut surface = PixelBuffer::new(4, 4, [255, 255, 255]).unwrap();
        surface
            .surface_mut()
            .with_viewport((1, 1), 2, 2, |surface| {
                surface.blend_mask((-1, -1), 4, 4, &[255; 16], [255, 0, 0]);
            });
        for y in 0..4 {
            for x in 0..4 {
                let offset = ((y * 4 + x) * 4) as usize;
                let expected = if (1..3).contains(&x) && (1..3).contains(&y) {
                    &[0, 0, 255, 255]
                } else {
                    &[255, 255, 255, 255]
                };
                assert_eq!(&surface.pixels()[offset..offset + 4], expected);
            }
        }
    }
}
