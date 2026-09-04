//! The bridge between the `image` crate and [`brp_core::RawImage`].
//!
//! `brp-core` deliberately knows nothing about PNG or WebP, so every binary that reads or writes
//! real image files needs this conversion. It lives here rather than in `brp-cli` so the CLI and
//! the benchmark can share it without one depending on the other.

use anyhow::{bail, Context, Result};
use brp_core::RawImage;
use image::{DynamicImage, ImageEncoder, ImageFormat};
use std::io::Cursor;
use std::path::Path;

/// An image loaded from disk, and whether loading it was lossless.
pub struct Loaded {
    pub image: RawImage,
    /// True when the source had more than 8 bits per sample and had to be reduced, so a BRP
    /// round-trip will not reproduce the original file exactly.
    pub narrowed: bool,
}

/// Loads an image file into the channel layout that holds it most directly.
pub fn load(path: &Path) -> Result<Loaded> {
    let img = image::open(path).with_context(|| format!("reading {}", path.display()))?;
    from_dynamic(img)
}

/// Decodes an in-memory image, as [`load`] does for a file.
pub fn load_bytes(bytes: &[u8]) -> Result<Loaded> {
    let img = image::load_from_memory(bytes).context("decoding image bytes")?;
    from_dynamic(img)
}

/// Converts a [`DynamicImage`], reporting whether bit depth had to be reduced.
pub fn from_dynamic(img: DynamicImage) -> Result<Loaded> {
    let (w, h) = (img.width(), img.height());

    let (channels, data, narrowed) = match img {
        // The four layouts BRP v1 stores natively.
        DynamicImage::ImageLuma8(b) => (1u8, b.into_raw(), false),
        DynamicImage::ImageLumaA8(b) => (2, b.into_raw(), false),
        DynamicImage::ImageRgb8(b) => (3, b.into_raw(), false),
        DynamicImage::ImageRgba8(b) => (4, b.into_raw(), false),

        // Anything deeper (16-bit, float) keeps its channel count but drops to 8 bits.
        other => {
            let color = other.color();
            let grayscale = color.channel_count() <= 2;
            match (grayscale, color.has_alpha()) {
                (true, false) => (1, other.to_luma8().into_raw(), true),
                (true, true) => (2, other.to_luma_alpha8().into_raw(), true),
                (false, false) => (3, other.to_rgb8().into_raw(), true),
                (false, true) => (4, other.to_rgba8().into_raw(), true),
            }
        }
    };

    let image = RawImage::new(w, h, channels, data).map_err(|e| anyhow::anyhow!(e))?;
    Ok(Loaded { image, narrowed })
}

/// Writes a [`RawImage`] as a PNG, preserving its channel layout.
pub fn save_png(raw: &RawImage, path: &Path) -> Result<()> {
    let bytes = encode_png(raw)?;
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Encodes a [`RawImage`] as PNG bytes, preserving its channel layout.
pub fn encode_png(raw: &RawImage) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(Cursor::new(&mut out))
        .write_image(
            raw.data(),
            raw.width(),
            raw.height(),
            extended_color_type(raw)?,
        )
        .context("encoding PNG")?;
    Ok(out)
}

/// Encodes a [`RawImage`] as lossless WebP bytes.
///
/// WebP has no grayscale form, so 1- and 2-channel images are expanded to RGBA first. That is not
/// a handicap invented for the comparison: it is what any WebP encoder would have to write.
pub fn encode_webp_lossless(raw: &RawImage) -> Result<Vec<u8>> {
    let rgba = to_dynamic(raw)?.to_rgba8();
    let mut out = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(Cursor::new(&mut out))
        .write_image(
            rgba.as_raw(),
            raw.width(),
            raw.height(),
            image::ExtendedColorType::Rgba8,
        )
        .context("encoding lossless WebP")?;
    Ok(out)
}

fn extended_color_type(raw: &RawImage) -> Result<image::ExtendedColorType> {
    Ok(match raw.channels() {
        1 => image::ExtendedColorType::L8,
        2 => image::ExtendedColorType::La8,
        3 => image::ExtendedColorType::Rgb8,
        4 => image::ExtendedColorType::Rgba8,
        n => bail!("unsupported channel count {n}"),
    })
}

fn to_dynamic(raw: &RawImage) -> Result<DynamicImage> {
    let (w, h) = (raw.width(), raw.height());
    let data = raw.data().to_vec();
    let buffer_err = || anyhow::anyhow!("pixel buffer does not match {w}x{h}");
    Ok(match raw.channels() {
        1 => {
            DynamicImage::ImageLuma8(image::GrayImage::from_raw(w, h, data).ok_or_else(buffer_err)?)
        }
        2 => DynamicImage::ImageLumaA8(
            image::GrayAlphaImage::from_raw(w, h, data).ok_or_else(buffer_err)?,
        ),
        3 => DynamicImage::ImageRgb8(image::RgbImage::from_raw(w, h, data).ok_or_else(buffer_err)?),
        4 => {
            DynamicImage::ImageRgba8(image::RgbaImage::from_raw(w, h, data).ok_or_else(buffer_err)?)
        }
        n => bail!("unsupported channel count {n}"),
    })
}

/// True if the path's extension names an image format this build can read.
///
/// Lossless inputs only, and deliberately: a JPEG source is not a neutral measurement subject.
/// Its high frequencies have already been quantised away and it carries 8x8 DCT blocking, so every
/// lossless codec scores far better on it than on the photograph it came from. Refusing to load
/// one keeps a stray file from quietly rewriting a corpus figure.
pub fn is_supported_image(path: &Path) -> bool {
    matches!(
        ImageFormat::from_path(path),
        Ok(ImageFormat::Png | ImageFormat::WebP)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(channels: u8) -> RawImage {
        let data = (0..(8 * 8 * u32::from(channels)))
            .map(|i| (i * 5 % 251) as u8)
            .collect();
        RawImage::new(8, 8, channels, data).unwrap()
    }

    #[test]
    fn png_round_trips_every_channel_layout() {
        for ch in 1..=4u8 {
            let src = sample(ch);
            let png = encode_png(&src).unwrap();
            let loaded = load_bytes(&png).unwrap();
            assert!(!loaded.narrowed, "{ch} channels");
            assert_eq!(loaded.image, src, "{ch} channels");
        }
    }

    #[test]
    fn webp_encoding_succeeds_for_every_layout() {
        for ch in 1..=4u8 {
            let bytes = encode_webp_lossless(&sample(ch)).unwrap();
            assert!(!bytes.is_empty(), "{ch} channels");
        }
    }

    #[test]
    fn recognizes_supported_extensions() {
        assert!(is_supported_image(Path::new("a.png")));
        assert!(is_supported_image(Path::new("a.webp")));
        assert!(!is_supported_image(Path::new("a.jpg")), "lossy sources are refused");
        assert!(!is_supported_image(Path::new("a.brp")));
        assert!(!is_supported_image(Path::new("a.txt")));
    }
}
