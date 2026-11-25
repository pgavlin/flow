//! Headless rendering with explicitly registered fonts.
//! Run: cargo run -p flow_document --features render --example render_document -- output.png
//! Optionally pass a font directory as the second argument (required outside the repository).
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stdout,
    reason = "This example is a host application"
)]
use std::{
    io::{BufWriter, Write},
    path::PathBuf,
    sync::Arc,
};

use flow_document::{
    formats::markdown::{MarkdownOptions, parse},
    pixels::{PixelBuffer, PixelFormat},
    render::Renderer,
    settings::RenderSettings,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: render_document OUTPUT.png")?,
    );
    let imported = parse(
        "# Hello, flow\n\nThis document is painted **without a window system**.\n\n- Explicit fonts\n- Semantic content\n- Portable pixels\n\n`let reader = host + engine;`",
        MarkdownOptions::default(),
    )?;
    let mut renderer = Renderer::new(RenderSettings {
        body_fonts: vec!["Charter".to_owned()],
        code_fonts: vec!["Fira Code".to_owned()],
        ..RenderSettings::default()
    });
    // Fonts are host-supplied, not embedded in the published crate.
    let font_directory = std::env::args_os()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fonts"));
    for name in [
        "charter.ttf",
        "charter-bold.ttf",
        "charter-italic.ttf",
        "charter-bold-italic.ttf",
        "fira-code.ttf",
    ] {
        let bytes = std::fs::read(font_directory.join(name))?;
        if !renderer.register_font(Arc::from(bytes)) {
            return Err("could not register a demo font".into());
        }
    }
    let pixels = renderer.render_full_document(&imported.document, 1.0)?;
    let mut writer = BufWriter::new(std::fs::File::create(&output)?);
    encode_png(&pixels, &mut writer)?;
    writer.flush()?;
    println!("Wrote {}", output.display());
    Ok(())
}

// This example host chooses RGB PNG output; another host may preserve alpha,
// use another codec, send pixels to a display, or keep them entirely in memory.
fn encode_png(buffer: &PixelBuffer, output: impl Write) -> Result<(), png::EncodingError> {
    let mut encoder = png::Encoder::new(output, buffer.width(), buffer.height());
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    let mut rgb = Vec::with_capacity(buffer.width() as usize * buffer.height() as usize * 3);
    for row in buffer.pixels().chunks_exact(buffer.stride()) {
        for pixel in row[..buffer.width() as usize * 4].chunks_exact(4) {
            match buffer.format() {
                PixelFormat::Bgra8888 => rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]),
                PixelFormat::Rgba8888 => rgb.extend_from_slice(&pixel[..3]),
            }
        }
    }
    writer.write_image_data(&rgb)?;
    writer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_encoding_preserves_dimensions_and_rgb_for_both_pixel_formats() {
        let expected = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        for format in [PixelFormat::Bgra8888, PixelFormat::Rgba8888] {
            let mut buffer = PixelBuffer::new_with_format(2, 2, [0; 3], format).unwrap();
            for (index, (pixel, rgb)) in buffer
                .pixels_mut()
                .chunks_exact_mut(4)
                .zip(expected.chunks_exact(3))
                .enumerate()
            {
                let channels = match format {
                    PixelFormat::Bgra8888 => [rgb[2], rgb[1], rgb[0]],
                    PixelFormat::Rgba8888 => [rgb[0], rgb[1], rgb[2]],
                };
                pixel[..3].copy_from_slice(&channels);
                pixel[3] = (index * 85) as u8; // The host deliberately discards alpha.
            }
            let mut encoded = Vec::new();
            encode_png(&buffer, &mut encoded).unwrap();
            let reader = png::Decoder::new(std::io::Cursor::new(&encoded))
                .read_info()
                .unwrap();
            assert_eq!(reader.info().color_type, png::ColorType::Rgb);
            assert_eq!(reader.info().bit_depth, png::BitDepth::Eight);
            let decoded = image::load_from_memory(&encoded).unwrap().to_rgb8();
            assert_eq!(decoded.dimensions(), (2, 2));
            assert_eq!(decoded.as_raw(), &expected);
        }
    }

    #[test]
    fn host_encoding_propagates_writer_failures() {
        struct Unwritable;
        impl Write for Unwritable {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("host writer failed"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buffer = PixelBuffer::new(1, 1, [1, 2, 3]).unwrap();
        assert!(encode_png(&buffer, Unwritable).is_err());
    }

    #[test]
    fn host_encoding_rejects_empty_buffers() {
        assert!(encode_png(&PixelBuffer::default(), Vec::new()).is_err());
    }
}
