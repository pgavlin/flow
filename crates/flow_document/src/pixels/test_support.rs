//! Test-only paint command transcript.
use super::{PaintTarget, PixelBuffer, PixelFormat, PixelSurface};

#[derive(Debug, PartialEq)]
pub(crate) enum PaintCommand {
    Clear([u8; 3]),
    Viewport((i32, i32), u32, u32),
    RestoreViewport,
    Rect(f32, f32, f32, f32, [u8; 3]),
    Line((f32, f32), (f32, f32), f32, [u8; 3]),
    Mask((i32, i32), u32, u32, Vec<u8>, [u8; 3]),
    Pixels((i32, i32), u32, u32, usize, PixelFormat, Vec<u8>),
}

pub(crate) struct RecordingPaintTarget {
    pub(crate) buffer: PixelBuffer,
    pub(crate) commands: Vec<PaintCommand>,
    origin: (i32, i32),
    clip: (i32, i32, i32, i32),
}

impl RecordingPaintTarget {
    pub(crate) fn new(width: u32, height: u32) -> anyhow::Result<Self> {
        Ok(Self {
            buffer: PixelBuffer::new(width, height, [0; 3])?,
            commands: Vec::new(),
            origin: (0, 0),
            clip: (0, 0, i32::try_from(width)?, i32::try_from(height)?),
        })
    }

    fn surface(&mut self) -> PixelSurface<'_> {
        let mut surface = self.buffer.surface_mut();
        surface.origin = self.origin;
        surface.clip = self.clip;
        surface
    }
}

impl PaintTarget for RecordingPaintTarget {
    type ViewportState = ((i32, i32), (i32, i32, i32, i32));

    fn width(&self) -> u32 {
        self.buffer.width()
    }

    fn height(&self) -> u32 {
        self.buffer.height()
    }

    fn clear(&mut self, color: [u8; 3]) {
        self.commands.push(PaintCommand::Clear(color));
        self.surface().clear(color);
    }

    fn push_viewport(
        &mut self,
        origin: (i32, i32),
        width: u32,
        height: u32,
    ) -> Self::ViewportState {
        self.commands
            .push(PaintCommand::Viewport(origin, width, height));
        let (state, next_origin, next_clip) = {
            let mut surface = self.surface();
            let state = PaintTarget::push_viewport(&mut surface, origin, width, height);
            (state, surface.origin, surface.clip)
        };
        self.origin = next_origin;
        self.clip = next_clip;
        state
    }

    fn pop_viewport(&mut self, state: Self::ViewportState) {
        self.commands.push(PaintCommand::RestoreViewport);
        (self.origin, self.clip) = state;
    }

    fn fill_rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: [u8; 3]) {
        self.commands
            .push(PaintCommand::Rect(x, y, width, height, color));
        self.surface().fill_rect(x, y, width, height, color);
    }

    fn draw_line(&mut self, start: (f32, f32), end: (f32, f32), thickness: f32, color: [u8; 3]) {
        self.commands
            .push(PaintCommand::Line(start, end, thickness, color));
        self.surface().draw_line(start, end, thickness, color);
    }

    fn blend_mask(
        &mut self,
        origin: (i32, i32),
        width: u32,
        height: u32,
        mask: &[u8],
        color: [u8; 3],
    ) {
        self.commands.push(PaintCommand::Mask(
            origin,
            width,
            height,
            mask.to_vec(),
            color,
        ));
        self.surface()
            .blend_mask(origin, width, height, mask, color);
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
        self.commands.push(PaintCommand::Pixels(
            position,
            width,
            height,
            source_stride,
            source_format,
            source_pixels.to_vec(),
        ));
        self.surface().blend_pixels(
            position,
            width,
            height,
            source_stride,
            source_format,
            source_pixels,
        );
    }
}
