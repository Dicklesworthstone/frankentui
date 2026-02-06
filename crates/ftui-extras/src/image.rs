#![forbid(unsafe_code)]

use std::env;
use std::io::Cursor;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ftui_core::terminal_capabilities::TerminalCapabilities;
use image::{DynamicImage, GenericImageView, ImageFormat, imageops::FilterType};

/// Image protocol selection for terminal rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageProtocol {
    Kitty,
    Iterm2,
    Sixel,
    Ascii,
}

/// Fit strategy when resizing images.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFit {
    None,
    Contain,
    Cover,
    Stretch,
}

/// Width/height specification for iTerm2 inline images.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Iterm2Dimension {
    Cells(u32),
    Pixels(u32),
    Percent(u8),
    Auto,
}

impl Iterm2Dimension {
    fn encode(self) -> String {
        match self {
            Self::Cells(value) => value.to_string(),
            Self::Pixels(value) => format!("{value}px"),
            Self::Percent(value) => format!("{value}%"),
            Self::Auto => "auto".to_string(),
        }
    }
}

/// Options for iTerm2 inline image emission.
#[derive(Debug, Clone)]
pub struct Iterm2Options {
    pub width: Option<Iterm2Dimension>,
    pub height: Option<Iterm2Dimension>,
    pub preserve_aspect_ratio: bool,
    pub inline: bool,
    pub name: Option<String>,
}

impl Default for Iterm2Options {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            preserve_aspect_ratio: true,
            inline: true,
            name: None,
        }
    }
}

/// External probe hints for protocol detection.
#[derive(Debug, Clone, Default)]
pub struct DetectionHints {
    pub term: Option<String>,
    pub term_program: Option<String>,
    pub kitty_graphics: Option<bool>,
    pub sixel: Option<bool>,
    pub iterm2_inline: Option<bool>,
}

impl DetectionHints {
    /// Capture hints from the environment.
    #[must_use]
    pub fn from_env() -> Self {
        let term = env::var("TERM").ok();
        let term_program = env::var("TERM_PROGRAM").ok();
        let kitty_graphics = if env::var("KITTY_WINDOW_ID").is_ok() {
            Some(true)
        } else {
            None
        };
        Self {
            term,
            term_program,
            kitty_graphics,
            sixel: None,
            iterm2_inline: None,
        }
    }

    #[must_use]
    pub fn with_kitty_graphics(mut self, supported: bool) -> Self {
        self.kitty_graphics = Some(supported);
        self
    }

    #[must_use]
    pub fn with_sixel(mut self, supported: bool) -> Self {
        self.sixel = Some(supported);
        self
    }

    #[must_use]
    pub fn with_iterm2_inline(mut self, supported: bool) -> Self {
        self.iterm2_inline = Some(supported);
        self
    }
}

/// Cache for protocol detection.
#[derive(Debug, Default)]
pub struct ProtocolCache {
    cached: Option<ImageProtocol>,
}

impl ProtocolCache {
    #[must_use]
    pub const fn new() -> Self {
        Self { cached: None }
    }

    #[must_use]
    pub fn detect(&mut self, caps: TerminalCapabilities, hints: &DetectionHints) -> ImageProtocol {
        if let Some(protocol) = self.cached {
            return protocol;
        }
        let protocol = detect_protocol(caps, hints);
        self.cached = Some(protocol);
        protocol
    }
}

/// Detect the best supported image protocol using caps + hints.
#[must_use]
pub fn detect_protocol(_caps: TerminalCapabilities, hints: &DetectionHints) -> ImageProtocol {
    let term = hints.term.as_deref().unwrap_or_default();
    let term_program = hints.term_program.as_deref().unwrap_or_default();

    let kitty_from_env = term.contains("kitty");
    if hints.kitty_graphics.unwrap_or(kitty_from_env) {
        return ImageProtocol::Kitty;
    }

    let iterm_from_env = term_program.contains("iTerm.app");
    if hints.iterm2_inline.unwrap_or(iterm_from_env) {
        return ImageProtocol::Iterm2;
    }

    let sixel_from_env = term.contains("sixel");
    if hints.sixel.unwrap_or(sixel_from_env) {
        return ImageProtocol::Sixel;
    }

    ImageProtocol::Ascii
}

/// In-memory image wrapper for protocol encoding.
#[derive(Debug, Clone)]
pub struct Image {
    image: DynamicImage,
}

impl Image {
    /// Decode image bytes using the `image` crate.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ImageError> {
        let image = image::load_from_memory(bytes)?;
        Ok(Self { image })
    }

    /// Convert the image to PNG bytes, optionally resizing with a fit strategy.
    pub fn to_png_bytes(
        &self,
        max_width: Option<u32>,
        max_height: Option<u32>,
        fit: ImageFit,
    ) -> Result<Vec<u8>, ImageError> {
        let resized = resize_image(&self.image, max_width, max_height, fit);
        let mut out = Cursor::new(Vec::new());
        resized
            .write_to(&mut out, ImageFormat::Png)
            .map_err(ImageError::Encode)?;
        Ok(out.into_inner())
    }

    /// Encode this image for kitty graphics protocol (PNG payload).
    pub fn encode_kitty(
        &self,
        max_width: Option<u32>,
        max_height: Option<u32>,
        fit: ImageFit,
    ) -> Result<Vec<String>, ImageError> {
        let png = self.to_png_bytes(max_width, max_height, fit)?;
        Ok(encode_kitty_png(&png))
    }

    /// Encode this image for iTerm2 inline images (PNG payload).
    pub fn encode_iterm2(
        &self,
        max_width: Option<u32>,
        max_height: Option<u32>,
        fit: ImageFit,
        options: &Iterm2Options,
    ) -> Result<String, ImageError> {
        let png = self.to_png_bytes(max_width, max_height, fit)?;
        Ok(encode_iterm2_png(&png, options))
    }

    /// Render a grayscale ASCII fallback.
    #[must_use]
    pub fn render_ascii(&self, width: u32, height: u32, fit: ImageFit) -> Vec<String> {
        render_ascii(&self.image, width, height, fit)
    }
}

/// Encode PNG payload as kitty graphics protocol escape sequences.
#[must_use]
pub fn encode_kitty_png(png_bytes: &[u8]) -> Vec<String> {
    let encoded = STANDARD.encode(png_bytes);
    let mut chunks = Vec::new();
    let mut offset = 0usize;
    let chunk_size = 4096usize;
    let mut first = true;

    while offset < encoded.len() {
        let end = (offset + chunk_size).min(encoded.len());
        let chunk = &encoded[offset..end];
        let more = end < encoded.len();
        let metadata = if first { "a=T,f=100," } else { "" };
        let m_value = if more { 1 } else { 0 };
        let seq = format!("\x1b_G{metadata}m={m_value};{chunk}\x1b\\");
        chunks.push(seq);
        offset = end;
        first = false;
    }

    if chunks.is_empty() {
        chunks.push("\x1b_Ga=T,f=100,m=0;\x1b\\".to_string());
    }

    chunks
}

/// Encode PNG payload as iTerm2 inline image escape sequence.
#[must_use]
pub fn encode_iterm2_png(png_bytes: &[u8], options: &Iterm2Options) -> String {
    let mut args = Vec::new();
    if options.inline {
        args.push("inline=1".to_string());
    }
    args.push(format!("size={}", png_bytes.len()));
    if let Some(width) = options.width {
        args.push(format!("width={}", width.encode()));
    }
    if let Some(height) = options.height {
        args.push(format!("height={}", height.encode()));
    }
    if !options.preserve_aspect_ratio {
        args.push("preserveAspectRatio=0".to_string());
    }
    if let Some(name) = &options.name {
        let encoded_name = STANDARD.encode(name.as_bytes());
        args.push(format!("name={encoded_name}"));
    }

    let header = format!("\x1b]1337;File={};", args.join(";"));
    let payload = STANDARD.encode(png_bytes);
    format!("{header}{payload}\x07")
}

fn resize_image(
    image: &DynamicImage,
    max_width: Option<u32>,
    max_height: Option<u32>,
    fit: ImageFit,
) -> DynamicImage {
    if matches!(fit, ImageFit::None) || (max_width.is_none() && max_height.is_none()) {
        return image.clone();
    }

    let (orig_w, orig_h) = image.dimensions();
    let target_w = max_width.unwrap_or(orig_w).max(1);
    let target_h = max_height.unwrap_or(orig_h).max(1);

    let (new_w, new_h) = match fit {
        ImageFit::Stretch => (target_w, target_h),
        ImageFit::Contain => scale_to_fit(orig_w, orig_h, target_w, target_h, false),
        ImageFit::Cover => scale_to_fit(orig_w, orig_h, target_w, target_h, true),
        ImageFit::None => (orig_w, orig_h),
    };

    if new_w == orig_w && new_h == orig_h {
        image.clone()
    } else {
        image.resize_exact(new_w, new_h, FilterType::Triangle)
    }
}

fn scale_to_fit(
    width: u32,
    height: u32,
    max_width: u32,
    max_height: u32,
    cover: bool,
) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (max_width.max(1), max_height.max(1));
    }
    let width_f = width as f32;
    let height_f = height as f32;
    let max_w = max_width as f32;
    let max_h = max_height as f32;

    let scale_w = max_w / width_f;
    let scale_h = max_h / height_f;
    let scale = if cover {
        scale_w.max(scale_h)
    } else {
        scale_w.min(scale_h)
    };

    let new_w = (width_f * scale).round().max(1.0) as u32;
    let new_h = (height_f * scale).round().max(1.0) as u32;
    (new_w, new_h)
}

fn render_ascii(image: &DynamicImage, width: u32, height: u32, fit: ImageFit) -> Vec<String> {
    let resized = resize_image(image, Some(width), Some(height), fit);
    let grayscale = resized.to_luma8();
    let ramp = b" .:-=+*#%@";
    let mut lines = Vec::with_capacity(grayscale.height() as usize);

    for y in 0..grayscale.height() {
        let mut line = String::with_capacity(grayscale.width() as usize);
        for x in 0..grayscale.width() {
            let luma = grayscale.get_pixel(x, y)[0] as usize;
            let idx = (luma * (ramp.len() - 1)) / 255;
            line.push(ramp[idx] as char);
        }
        lines.push(line);
    }

    lines
}

/// Errors raised by image decoding/encoding or protocol handling.
#[derive(Debug)]
pub enum ImageError {
    Decode(image::ImageError),
    Encode(image::ImageError),
}

impl From<image::ImageError> for ImageError {
    fn from(err: image::ImageError) -> Self {
        Self::Decode(err)
    }
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(err) => write!(f, "image decode error: {err}"),
            Self::Encode(err) => write!(f, "image encode error: {err}"),
        }
    }
}

impl std::error::Error for ImageError {}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_1X1_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
    const GIF_1X1_BASE64: &str = "R0lGODdhAQABAIEAAP8AAAAAAAAAAAAAACwAAAAAAQABAAAIBAABBAQAOw==";

    fn decode_fixture_bytes(label: &str, data_b64: &str) -> Vec<u8> {
        STANDARD
            .decode(data_b64)
            .unwrap_or_else(|err| panic!("fixture {label} base64 decode failed: {err}"))
    }

    fn encode_bytes(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
        let image = DynamicImage::new_rgba8(width, height);
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, format).expect("encode test image");
        out.into_inner()
    }

    #[test]
    fn detects_kitty_from_env_hint() {
        let caps = TerminalCapabilities::basic();
        let hints = DetectionHints {
            term: Some("xterm-kitty".to_string()),
            ..DetectionHints::default()
        };
        assert_eq!(detect_protocol(caps, &hints), ImageProtocol::Kitty);
    }

    #[test]
    fn iterm2_dimensions_encode() {
        assert_eq!(Iterm2Dimension::Cells(10).encode(), "10");
        assert_eq!(Iterm2Dimension::Pixels(120).encode(), "120px");
        assert_eq!(Iterm2Dimension::Percent(50).encode(), "50%");
        assert_eq!(Iterm2Dimension::Auto.encode(), "auto");
    }

    #[test]
    fn kitty_chunks_include_metadata_once() {
        let payload = vec![0u8; 32];
        let encoded = encode_kitty_png(&payload);
        assert!(encoded.first().unwrap().contains("a=T,f=100"));
        for chunk in encoded.iter().skip(1) {
            assert!(!chunk.contains("a=T,f=100"));
        }
    }

    #[test]
    fn kitty_empty_payload_emits_single_chunk() {
        let encoded = encode_kitty_png(&[]);
        assert_eq!(encoded.len(), 1, "empty payload should emit a single chunk");
        assert!(encoded[0].contains("a=T,f=100"), "metadata must be present");
        assert!(encoded[0].contains("m=0;"), "final chunk marker required");
    }

    #[test]
    fn iterm2_encodes_inline_sequence() {
        let payload = vec![1u8; 4];
        let seq = encode_iterm2_png(&payload, &Iterm2Options::default());
        assert!(seq.starts_with("\x1b]1337;File="));
        assert!(seq.ends_with('\x07'));
        assert!(seq.contains("inline=1"));
    }

    #[test]
    fn ascii_fallback_renders_lines() {
        let image = DynamicImage::new_rgb8(4, 4);
        let lines = render_ascii(&image, 4, 4, ImageFit::None);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].len(), 4);
    }

    #[test]
    fn ascii_fallback_contain_preserves_aspect_ratio() {
        let image = DynamicImage::new_rgb8(4, 2);
        let lines = render_ascii(&image, 4, 4, ImageFit::Contain);
        assert_eq!(
            lines.len(),
            2,
            "contain fit should preserve aspect ratio height"
        );
        assert_eq!(lines[0].len(), 4, "contain fit should keep width");
    }

    #[test]
    fn scale_to_fit_handles_zero_dimensions() {
        let (w, h) = scale_to_fit(0, 0, 0, 0, false);
        assert_eq!((w, h), (1, 1), "zero sizes clamp to 1x1");
    }

    #[test]
    fn decode_png_roundtrip_preserves_dimensions() {
        let bytes = encode_bytes(ImageFormat::Png, 3, 2);
        let image = Image::from_bytes(&bytes).expect("decode png");
        let out = image
            .to_png_bytes(None, None, ImageFit::None)
            .expect("encode png");
        let decoded = image::load_from_memory(&out).expect("decode roundtrip");
        assert_eq!(decoded.dimensions(), (3, 2));
    }

    #[test]
    fn decode_png_fixture_resize_stretch() {
        let bytes = decode_fixture_bytes("png_1x1", PNG_1X1_BASE64);
        let image = Image::from_bytes(&bytes).expect("decode png fixture");
        let out = image
            .to_png_bytes(Some(3), Some(2), ImageFit::Stretch)
            .expect("resize stretch");
        let decoded = image::load_from_memory(&out).expect("decode resized png");
        assert_eq!(decoded.dimensions(), (3, 2), "stretch should hit bounds");
    }

    #[test]
    fn decode_gif_fixture_resize_contain() {
        let bytes = decode_fixture_bytes("gif_1x1", GIF_1X1_BASE64);
        let image = Image::from_bytes(&bytes).expect("decode gif fixture");
        let out = image
            .to_png_bytes(Some(2), Some(2), ImageFit::Contain)
            .expect("encode png");
        let decoded = image::load_from_memory(&out).expect("decode roundtrip");
        assert_eq!(decoded.dimensions(), (2, 2), "contain should scale up");
    }

    #[test]
    fn decode_jpeg_format_roundtrip() {
        let bytes = encode_bytes(ImageFormat::Jpeg, 2, 2);
        let image = Image::from_bytes(&bytes).expect("decode jpeg");
        let out = image
            .to_png_bytes(None, None, ImageFit::None)
            .expect("encode png");
        let decoded = image::load_from_memory(&out).expect("decode roundtrip");
        assert_eq!(decoded.dimensions(), (2, 2));
    }

    #[test]
    fn decode_invalid_bytes_returns_error() {
        let err = Image::from_bytes(b"not an image").expect_err("expected decode error");
        assert!(matches!(err, ImageError::Decode(_)));
    }

    #[test]
    fn detect_protocol_iterm2() {
        let caps = TerminalCapabilities::basic();
        let hints = DetectionHints::default().with_iterm2_inline(true);
        assert_eq!(detect_protocol(caps, &hints), ImageProtocol::Iterm2);
    }

    #[test]
    fn detect_protocol_sixel() {
        let caps = TerminalCapabilities::basic();
        let hints = DetectionHints::default().with_sixel(true);
        assert_eq!(detect_protocol(caps, &hints), ImageProtocol::Sixel);
    }

    #[test]
    fn detect_protocol_ascii_fallback() {
        let caps = TerminalCapabilities::basic();
        let hints = DetectionHints::default();
        assert_eq!(detect_protocol(caps, &hints), ImageProtocol::Ascii);
    }

    #[test]
    fn protocol_cache_returns_cached() {
        let caps = TerminalCapabilities::basic();
        let hints = DetectionHints::default().with_kitty_graphics(true);
        let mut cache = ProtocolCache::new();

        let first = cache.detect(caps, &hints);
        assert_eq!(first, ImageProtocol::Kitty);

        // Second call with different hints should still return cached value
        let other_hints = DetectionHints::default().with_sixel(true);
        let second = cache.detect(caps, &other_hints);
        assert_eq!(second, ImageProtocol::Kitty);
    }

    #[test]
    fn iterm2_options_defaults() {
        let opts = Iterm2Options::default();
        assert!(opts.width.is_none());
        assert!(opts.height.is_none());
        assert!(opts.preserve_aspect_ratio);
        assert!(opts.inline);
        assert!(opts.name.is_none());
    }

    #[test]
    fn image_error_display() {
        let err = Image::from_bytes(b"bad").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("decode error"), "got: {msg}");
    }

    #[test]
    fn scale_to_fit_contain_smaller_than_max() {
        // 2x4 image in 10x10 box, contain should scale uniformly
        let (w, h) = scale_to_fit(2, 4, 10, 10, false);
        // Scale = min(10/2, 10/4) = min(5, 2.5) = 2.5 → (5, 10)
        assert_eq!((w, h), (5, 10));
    }

    #[test]
    fn scale_to_fit_cover_fills_box() {
        let (w, h) = scale_to_fit(2, 4, 10, 10, true);
        // Scale = max(10/2, 10/4) = max(5, 2.5) = 5 → (10, 20)
        assert_eq!((w, h), (10, 20));
    }
}
