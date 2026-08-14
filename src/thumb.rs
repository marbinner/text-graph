//! Thumbnail decoding: image file → downscaled RGBA pixel buffer. Pure and
//! egui-free — the GUI uploads the result as a texture on its own side.

use std::path::Path;

use anyhow::{Context, Result};

/// Decoded, downscaled pixels in row-major RGBA8.
pub struct Thumb {
    pub w: u32,
    pub h: u32,
    /// `w * h * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Decode `path` and downscale so the longer side is at most `max_px`
/// (aspect preserved; images already within bounds keep their native size).
pub fn decode(path: &Path, max_px: u32) -> Result<Thumb> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    decode_bytes(&bytes, max_px).with_context(|| format!("cannot decode {}", path.display()))
}

/// The pure core of [`decode`]; the format is guessed from the bytes.
pub fn decode_bytes(bytes: &[u8], max_px: u32) -> Result<Thumb> {
    let img = image::load_from_memory(bytes)?;
    // guard the downscale: image's resize family upscales too, and a tiny
    // icon blown up to max_px would just waste texture memory
    let img = if img.width() > max_px || img.height() > max_px {
        img.thumbnail(max_px, max_px)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Ok(Thumb {
        w: rgba.width(),
        h: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};
    use std::io::Cursor;

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = ImageBuffer::from_fn(w, h, |x, y| {
            Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn downscales_preserving_aspect() {
        let t = decode_bytes(&png_bytes(64, 32), 16).unwrap();
        assert_eq!((t.w, t.h), (16, 8));
        assert_eq!(t.rgba.len(), 16 * 8 * 4);
    }

    #[test]
    fn small_images_keep_native_size() {
        let t = decode_bytes(&png_bytes(10, 8), 256).unwrap();
        assert_eq!((t.w, t.h), (10, 8));
        assert_eq!(t.rgba.len(), 10 * 8 * 4);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(decode_bytes(b"not an image", 64).is_err());
        assert!(decode_bytes(&[], 64).is_err());
    }

    #[test]
    fn fixture_diagram_png_decodes() {
        // the fixture image must stay a real decodable PNG — the viewer's
        // thumbnail path depends on it
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/vault/assets/diagram.png");
        let t = decode(&p, 64).unwrap();
        assert_eq!((t.w, t.h), (24, 16));
    }
}
