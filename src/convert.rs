//! The conversion core: no HTTP in here, just bytes in -> bytes out.
//!
//! Pipeline:  sniff input format -> decode/render to straight-alpha RGBA ->
//!            pick output size -> (flatten onto background) -> encode.
//!
//! Everything here is blocking, CPU-bound work; api.rs runs it on a worker thread.

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{ExtendedColorType, ImageEncoder, ImageFormat, RgbImage, RgbaImage};
use resvg::{tiny_skia, usvg};

use crate::errors::ApiError;

/// Family used for SVG text that names no font (and as the sans-serif default).
const DEFAULT_FONT_FAMILY: &str = "DejaVu Sans";

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_dimension: u32,
    pub max_pixels: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Png,
    Jpeg,
}

impl OutputFormat {
    pub fn parse(text: &str) -> Result<OutputFormat, ApiError> {
        match text.to_ascii_lowercase().as_str() {
            "png" => Ok(OutputFormat::Png),
            "jpeg" | "jpg" => Ok(OutputFormat::Jpeg),
            _ => Err(ApiError::invalid_parameter("format must be one of: png, jpeg")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            OutputFormat::Png => "png",
            OutputFormat::Jpeg => "jpeg",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            OutputFormat::Png => "image/png",
            OutputFormat::Jpeg => "image/jpeg",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Background {
    Transparent,
    Color([u8; 3]),
}

/// Validated conversion request. Built once from query string or JSON body.
#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub format: OutputFormat,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub background: Option<Background>,
    pub quality: u8,
}

impl ConvertOptions {
    /// Validates the raw (still textual / optional) parameters from either endpoint.
    pub fn new(
        format: Option<&str>,
        width: Option<u32>,
        height: Option<u32>,
        background: Option<&str>,
        quality: Option<u8>,
    ) -> Result<ConvertOptions, ApiError> {
        let format = OutputFormat::parse(format.ok_or_else(|| ApiError::invalid_parameter("format is required (png or jpeg)"))?)?;

        if width == Some(0) || height == Some(0) {
            return Err(ApiError::invalid_parameter("width and height must be positive"));
        }

        let background = background.map(parse_background).transpose()?;
        if format == OutputFormat::Jpeg && background == Some(Background::Transparent) {
            return Err(ApiError::invalid_parameter("jpeg cannot be transparent, use a color such as #ffffff"));
        }

        if quality.is_some() && format != OutputFormat::Jpeg {
            return Err(ApiError::invalid_parameter("quality only applies to jpeg output"));
        }
        let quality = quality.unwrap_or(90);
        if !(1..=100).contains(&quality) {
            return Err(ApiError::invalid_parameter("quality must be between 1 and 100"));
        }

        Ok(ConvertOptions { format, width, height, background, quality })
    }
}

/// Accepts `transparent`, `#rgb` or `#rrggbb`. Nothing else (no CSS names, no rgb()).
fn parse_background(text: &str) -> Result<Background, ApiError> {
    let bad = || ApiError::invalid_parameter("background must be 'transparent', #rgb or #rrggbb");

    if text.eq_ignore_ascii_case("transparent") {
        return Ok(Background::Transparent);
    }
    let hex = text.strip_prefix('#').ok_or_else(bad)?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let digit = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).unwrap_or(0);
    let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
    match hex.len() {
        3 => Ok(Background::Color([digit(0) * 17, digit(1) * 17, digit(2) * 17])),
        6 => Ok(Background::Color([pair(0), pair(2), pair(4)])),
        _ => Err(bad()),
    }
}

pub struct Converted {
    pub format: OutputFormat,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Svg,
    Png,
    Jpeg,
}

/// Decides the input type from the bytes themselves, never from a header.
pub fn sniff(data: &[u8]) -> Option<InputKind> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(InputKind::Png);
    }
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(InputKind::Jpeg);
    }
    // SVG is text: optional UTF-8 BOM, optional whitespace, then '<'.
    let text = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data);
    let first = text.iter().find(|b| !b.is_ascii_whitespace());
    if first == Some(&b'<') {
        return Some(InputKind::Svg);
    }
    None
}

/// Loads the fonts the SVG renderer may use. Called once at startup.
pub fn load_fonts(dir: &Path) -> usvg::fontdb::Database {
    let mut db = usvg::fontdb::Database::new();
    // Never db.load_system_fonts(): output must not depend on the host.
    db.load_fonts_dir(dir);
    db.set_sans_serif_family(DEFAULT_FONT_FAMILY);
    db.set_serif_family("DejaVu Serif");
    db.set_monospace_family("DejaVu Sans Mono");
    db
}

pub fn convert(
    input: &[u8],
    opts: &ConvertOptions,
    limits: &Limits,
    fonts: &Arc<usvg::fontdb::Database>,
) -> Result<Converted, ApiError> {
    let kind = sniff(input).ok_or_else(|| ApiError::unsupported_media_type("input must be SVG, PNG or JPEG"))?;

    let image = match kind {
        InputKind::Svg => render_svg(input, opts, limits, fonts)?,
        InputKind::Png => decode_raster(input, ImageFormat::Png, opts, limits)?,
        InputKind::Jpeg => decode_raster(input, ImageFormat::Jpeg, opts, limits)?,
    };
    encode(&image, opts)
}

/// Output size policy (see README):
///  - nothing given : intrinsic size, rounded up
///  - one dimension : the other follows the aspect ratio
///  - both          : fit within the box, aspect ratio kept (never stretched)
/// Anything beyond the configured limits is refused before any pixel buffer exists.
pub fn target_size(
    src_w: f32,
    src_h: f32,
    width: Option<u32>,
    height: Option<u32>,
    limits: &Limits,
) -> Result<(u32, u32), ApiError> {
    if !src_w.is_finite() || !src_h.is_finite() || src_w <= 0.0 || src_h <= 0.0 {
        return Err(ApiError::bad_request("invalid_image", "image has no usable size"));
    }
    // f64 so that huge requested sizes cannot overflow before we compare them.
    let (sw, sh) = (f64::from(src_w), f64::from(src_h));

    let (w, h) = match (width, height) {
        (None, None) => (sw.ceil(), sh.ceil()),
        (Some(w), None) => (f64::from(w), (sh * f64::from(w) / sw).round()),
        (None, Some(h)) => ((sw * f64::from(h) / sh).round(), f64::from(h)),
        (Some(bw), Some(bh)) => {
            let scale = (f64::from(bw) / sw).min(f64::from(bh) / sh);
            ((sw * scale).round().min(f64::from(bw)), (sh * scale).round().min(f64::from(bh)))
        }
    };
    let (w, h) = (w.max(1.0), h.max(1.0));

    if w > f64::from(limits.max_dimension) || h > f64::from(limits.max_dimension) || w * h > limits.max_pixels as f64 {
        return Err(ApiError::image_too_large(format!(
            "output size exceeds the limits ({} pixels per side, {} pixels total)",
            limits.max_dimension, limits.max_pixels
        )));
    }
    Ok((w as u32, h as u32))
}

// ---------------------------------------------------------------- SVG

fn render_svg(
    input: &[u8],
    opts: &ConvertOptions,
    limits: &Limits,
    fonts: &Arc<usvg::fontdb::Database>,
) -> Result<RgbaImage, ApiError> {
    // XML entity declarations are only useful for "billion laughs" style
    // expansion bombs. Real-world icons and drawings do not need them.
    if contains(input, b"<!ENTITY") {
        return Err(ApiError::bad_request("unsupported_svg", "SVG entity declarations are not supported"));
    }

    let options = svg_options(fonts, limits.max_pixels);
    let tree = usvg::Tree::from_data(input, &options).map_err(|e| {
        // The parser's own message stays in the log; the client gets a fixed text.
        tracing::debug!("SVG parse failed: {e}");
        ApiError::bad_request("invalid_svg", "SVG input could not be parsed")
    })?;

    let size = tree.size();
    let (src_w, src_h) = (size.width(), size.height());
    let (out_w, out_h) = target_size(src_w, src_h, opts.width, opts.height, limits)?;

    let mut pixmap = tiny_skia::Pixmap::new(out_w, out_h).ok_or_else(ApiError::internal)?;
    let transform = tiny_skia::Transform::from_scale(out_w as f32 / src_w, out_h as f32 / src_h);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // tiny-skia stores premultiplied pixels; the image crate wants straight alpha.
    RgbaImage::from_raw(out_w, out_h, pixmap.take_demultiplied()).ok_or_else(ApiError::internal)
}

/// Parser/renderer settings. The important part is what is switched OFF:
/// no file access, no network, no system fonts.
#[allow(clippy::field_reassign_with_default)]
fn svg_options(fonts: &Arc<usvg::fontdb::Database>, max_pixels: u64) -> usvg::Options<'static> {
    let mut options = usvg::Options::default();
    options.fontdb = fonts.clone();
    options.font_family = DEFAULT_FONT_FAMILY.to_string();
    options.resources_dir = None;

    // <image href="file:///etc/passwd"> or href="http://..." or a relative path:
    // the default resolver would read local files; we answer "not available".
    // (usvg never does network access; this only closes the file route.)
    options.image_href_resolver.resolve_string = Box::new(|_, _| None);

    // data: URLs are self-contained and stay allowed, but a tiny PNG can claim
    // a gigantic size, so the size is checked from the header before resvg decodes.
    let default_data = usvg::ImageHrefResolver::default_data_resolver();
    options.image_href_resolver.resolve_data = Box::new(move |mime, data, opt| {
        let kind = default_data(mime, data, opt)?;
        check_embedded_image(kind, max_pixels)
    });
    options
}

fn check_embedded_image(kind: usvg::ImageKind, max_pixels: u64) -> Option<usvg::ImageKind> {
    let data = match &kind {
        usvg::ImageKind::PNG(d) | usvg::ImageKind::JPEG(d) | usvg::ImageKind::GIF(d) | usvg::ImageKind::WEBP(d) => d.clone(),
        _ => return Some(kind), // nested SVG: parsed by usvg itself without further image loading
    };
    // Only PNG and JPEG are known to the image crate in this build; GIF/WebP
    // cannot be size-checked, so they are dropped.
    let (w, h) = image::ImageReader::new(Cursor::new(data.as_slice()))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    if u64::from(w) * u64::from(h) > max_pixels {
        tracing::debug!("embedded image {w}x{h} skipped: over the pixel limit");
        return None;
    }
    Some(kind)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ------------------------------------------------------------- raster

fn decode_raster(input: &[u8], format: ImageFormat, opts: &ConvertOptions, limits: &Limits) -> Result<RgbaImage, ApiError> {
    let bad = |e: image::ImageError| {
        tracing::debug!("image decode failed: {e}");
        ApiError::bad_request("invalid_image", "image data could not be decoded")
    };

    // Read the size from the header first, so a decompression bomb is refused
    // before the decoder allocates anything.
    let (w, h) = image::ImageReader::with_format(Cursor::new(input), format)
        .into_dimensions()
        .map_err(bad)?;
    if u64::from(w) * u64::from(h) > limits.max_pixels {
        return Err(ApiError::image_too_large("input image has too many pixels"));
    }
    let (out_w, out_h) = target_size(w as f32, h as f32, opts.width, opts.height, limits)?;

    let decoded = image::load_from_memory_with_format(input, format).map_err(bad)?;
    let resized = if (out_w, out_h) == (w, h) {
        decoded
    } else {
        decoded.resize_exact(out_w, out_h, FilterType::Lanczos3)
    };
    Ok(resized.to_rgba8())
}

// ----------------------------------------------------------- encoding

/// Composites straight-alpha RGBA over a solid color.
fn flatten(img: &RgbaImage, bg: [u8; 3]) -> RgbImage {
    let mut out = RgbImage::new(img.width(), img.height());
    for (src, dst) in img.pixels().zip(out.pixels_mut()) {
        let alpha = u32::from(src.0[3]);
        for i in 0..3 {
            let blended = u32::from(src.0[i]) * alpha + u32::from(bg[i]) * (255 - alpha);
            dst.0[i] = ((blended + 127) / 255) as u8;
        }
    }
    out
}

fn encode(img: &RgbaImage, opts: &ConvertOptions) -> Result<Converted, ApiError> {
    let (w, h) = img.dimensions();
    let mut data: Vec<u8> = Vec::new();

    let result = match opts.format {
        OutputFormat::Png => {
            let encoder = PngEncoder::new(&mut data);
            match opts.background {
                Some(Background::Color(color)) => {
                    encoder.write_image(flatten(img, color).as_raw(), w, h, ExtendedColorType::Rgb8)
                }
                _ if img.pixels().all(|p| p.0[3] == 255) => {
                    // Fully opaque: an RGB PNG is smaller than RGBA. Alpha is 255
                    // everywhere, so the background color is irrelevant.
                    encoder.write_image(flatten(img, [0, 0, 0]).as_raw(), w, h, ExtendedColorType::Rgb8)
                }
                _ => encoder.write_image(img.as_raw(), w, h, ExtendedColorType::Rgba8),
            }
        }
        OutputFormat::Jpeg => {
            // JPEG has no alpha: flatten onto the chosen color, white by default.
            let color = match opts.background {
                Some(Background::Color(color)) => color,
                _ => [255, 255, 255],
            };
            JpegEncoder::new_with_quality(&mut data, opts.quality).write_image(
                flatten(img, color).as_raw(),
                w,
                h,
                ExtendedColorType::Rgb8,
            )
        }
    };
    result.map_err(|e| {
        tracing::error!("encoding failed: {e}");
        ApiError::internal()
    })?;

    Ok(Converted { format: opts.format, width: w, height: h, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits { max_dimension: 4096, max_pixels: 16_000_000 };

    fn size(w: f32, h: f32, bw: Option<u32>, bh: Option<u32>) -> (u32, u32) {
        target_size(w, h, bw, bh, &LIMITS).unwrap()
    }

    #[test]
    fn size_policy() {
        assert_eq!(size(100.0, 50.0, None, None), (100, 50));
        assert_eq!(size(100.5, 50.2, None, None), (101, 51)); // rounded up, nothing cropped
        assert_eq!(size(100.0, 50.0, Some(200), None), (200, 100));
        assert_eq!(size(100.0, 50.0, None, Some(25)), (50, 25));
        // both: fit inside the box, aspect kept
        assert_eq!(size(100.0, 50.0, Some(200), Some(200)), (200, 100));
        assert_eq!(size(100.0, 50.0, Some(40), Some(40)), (40, 20));
        assert_eq!(size(50.0, 100.0, Some(300), Some(200)), (100, 200));
    }

    #[test]
    fn size_limits() {
        assert!(target_size(100.0, 100.0, Some(5000), None, &LIMITS).is_err()); // over max_dimension
        assert!(target_size(100.0, 100.0, Some(4000), Some(4000), &LIMITS).is_ok());
        let small = Limits { max_dimension: 4096, max_pixels: 10_000 };
        assert!(target_size(100.0, 100.0, Some(200), None, &small).is_err()); // over max_pixels
        assert!(target_size(0.0, 10.0, None, None, &LIMITS).is_err());
        assert!(target_size(f32::NAN, 10.0, None, None, &LIMITS).is_err());
        assert!(target_size(1.0, 1.0, Some(u32::MAX), None, &LIMITS).is_err());
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff(b"<svg xmlns='http://www.w3.org/2000/svg'/>"), Some(InputKind::Svg));
        assert_eq!(sniff(b"\xEF\xBB\xBF  \n<?xml version='1.0'?><svg/>"), Some(InputKind::Svg));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some(InputKind::Png));
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0...."), Some(InputKind::Jpeg));
        assert_eq!(sniff(b"GIF89a"), None);
        assert_eq!(sniff(b""), None);
        assert_eq!(sniff(b"hello"), None);
    }

    #[test]
    fn backgrounds() {
        assert_eq!(parse_background("transparent").unwrap(), Background::Transparent);
        assert_eq!(parse_background("#fff").unwrap(), Background::Color([255, 255, 255]));
        assert_eq!(parse_background("#1A2b3C").unwrap(), Background::Color([0x1A, 0x2B, 0x3C]));
        for bad in ["white", "#ff", "#ggg", "ffffff", "#ffffffff", "rgb(1,2,3)", ""] {
            assert!(parse_background(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn option_validation() {
        assert!(ConvertOptions::new(Some("png"), None, None, None, None).is_ok());
        assert!(ConvertOptions::new(None, None, None, None, None).is_err());
        assert!(ConvertOptions::new(Some("gif"), None, None, None, None).is_err());
        assert!(ConvertOptions::new(Some("png"), Some(0), None, None, None).is_err());
        assert!(ConvertOptions::new(Some("png"), None, None, None, Some(80)).is_err()); // quality is jpeg-only
        assert!(ConvertOptions::new(Some("jpeg"), None, None, None, Some(0)).is_err());
        assert!(ConvertOptions::new(Some("jpeg"), None, None, Some("transparent"), None).is_err());
        assert!(ConvertOptions::new(Some("jpeg"), None, None, Some("#000"), Some(75)).is_ok());
    }

    #[test]
    fn flatten_blends_over_background() {
        let mut img = RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 0])); // fully transparent
        img.put_pixel(1, 0, image::Rgba([255, 0, 0, 255])); // opaque red
        let out = flatten(&img, [255, 255, 255]);
        assert_eq!(out.get_pixel(0, 0).0, [255, 255, 255]); // background shows, not black
        assert_eq!(out.get_pixel(1, 0).0, [255, 0, 0]);
    }
}
