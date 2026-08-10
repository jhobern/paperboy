//! Resolving the picture behind an `IMAGE` column's cell value.
//!
//! `IMAGE` is a **render hint, never a value** (see
//! [`super::flow::ImageSpec`]): the cell text stays whatever the flow produced,
//! and this module's job is to work out what picture — if any — that text
//! *refers* to, so a writer that can show one has bytes to embed.
//!
//! Resolution is by the **shape of the value**, in order:
//!
//! 1. a `data:image/…;base64,…` URI, decoded in place;
//! 2. an `http(s)://…` URL, fetched with a plain unauthenticated GET;
//! 3. a local path, resolved against `# root:` like every producer path;
//! 4. a bare base64 blob, decoded in place.
//!
//! No source is privileged and the shapes don't overlap, so a report never has
//! to say which kind it meant. Anything unrecognised, unreachable, or not a
//! decodable image resolves to **nothing** and the cell stays plain text: a
//! broken picture must never fail a report, because the report's actual subject
//! is the API run, not the illustration beside it.
//!
//! Fetching happens here, during the *run*, rather than in the writers:
//! [`super::writer::ReportWriter`] is a pure `(&ReportResult, &Header) ->
//! Vec<u8>` function with no IO, which is what makes every writer trivially
//! testable, and the run is where the network, the root directory and the
//! parallel workers already are.

use std::collections::HashMap;
use std::path::Path;

use super::model::ImageData;

/// The largest single picture that will be embedded, in bytes. A report row is
/// an illustration, not an asset store: past this size the workbook becomes
/// unopenable long before the picture becomes more informative, so the cell
/// falls back to its text.
pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// The largest total picture payload for one run, in bytes. A thousand-row
/// report with an image column would otherwise quietly produce a workbook no
/// spreadsheet program can open.
pub const MAX_TOTAL_IMAGE_BYTES: usize = 256 * 1024 * 1024;

/// How long a convenience fetch of an `http(s)` image value may take.
const FETCH_TIMEOUT_SECS: u64 = 30;

/// Resolves image values to bytes, caching by source string so the same URL or
/// path referenced by many rows is fetched or read exactly once — the common
/// case for a per-row report over a shared set of inputs.
pub struct ImageResolver {
    cache: HashMap<String, Option<ImageData>>,
    total_bytes: usize,
    /// When set, an `http(s)` value is *not* fetched. A dry run sets this: a
    /// run that announces "no requests sent" must not quietly make a hundred
    /// GETs. Local paths and `data:` URIs still resolve, so a dry run of a
    /// file-driven image report still shows its pictures.
    pub offline: bool,
    /// Diagnostics worth surfacing in the run log: a value that looked like a
    /// picture but couldn't be turned into one. Silently dropping these would
    /// leave a user staring at a text cell with no idea why.
    pub notes: Vec<String>,
}

impl Default for ImageResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageResolver {
    pub fn new() -> Self {
        ImageResolver {
            cache: HashMap::new(),
            total_bytes: 0,
            offline: false,
            notes: Vec::new(),
        }
    }

    /// Resolve `value` to picture bytes, or `None` to leave the cell as text.
    /// `root` is the run's base directory, for a relative path value.
    pub fn resolve(&mut self, value: &str, root: Option<&Path>) -> Option<ImageData> {
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        if let Some(hit) = self.cache.get(value) {
            return hit.clone();
        }
        let resolved = self.load(value, root);
        if let Some(img) = &resolved {
            self.total_bytes += img.bytes.len();
        }
        self.cache.insert(value.to_string(), resolved.clone());
        resolved
    }

    fn load(&mut self, value: &str, root: Option<&Path>) -> Option<ImageData> {
        if self.total_bytes >= MAX_TOTAL_IMAGE_BYTES {
            self.notes.push(format!(
                "image budget of {} MB exhausted; remaining image cells left as text",
                MAX_TOTAL_IMAGE_BYTES / (1024 * 1024)
            ));
            return None;
        }
        let bytes = match value_shape(value) {
            Shape::DataUri(payload) => decode_base64(payload)?,
            Shape::Url if self.offline => return None,
            Shape::Url => match fetch_url(value) {
                Ok(b) => b,
                Err(e) => {
                    self.notes
                        .push(format!("image fetch failed for {value}: {e}"));
                    return None;
                }
            },
            Shape::Path => {
                let path = super::producers::resolve_path(root, value);
                match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(e) => {
                        self.notes
                            .push(format!("image file unreadable: {}: {e}", path.display()));
                        return None;
                    }
                }
            }
            Shape::Base64 => decode_base64(value)?,
            Shape::Unknown => return None,
        };
        self.build(value, bytes)
    }

    fn build(&mut self, value: &str, bytes: Vec<u8>) -> Option<ImageData> {
        if bytes.len() > MAX_IMAGE_BYTES {
            self.notes.push(format!(
                "image at {value} is {} MB, over the {} MB per-image limit; left as text",
                bytes.len() / (1024 * 1024),
                MAX_IMAGE_BYTES / (1024 * 1024)
            ));
            return None;
        }
        let Some((mime, natural)) = probe(&bytes) else {
            // Only worth a note when the value clearly *meant* to be a picture;
            // an ordinary text cell that happens to look like a path shouldn't
            // fill the run log with noise.
            self.notes
                .push(format!("value at {value} is not a recognised image format"));
            return None;
        };
        let (bytes, mime, natural) = match downscale(&bytes, mime, natural) {
            Some(smaller) => smaller,
            None => (bytes, mime.to_string(), natural),
        };
        Some(ImageData {
            bytes,
            mime,
            natural,
        })
    }
}

/// The longest edge, in pixels, that a report will embed a picture at.
///
/// A report row's picture is an illustration with two jobs: a thumbnail in the
/// grid (`IMAGE(HEIGHT 110)` and friends), and a larger view in the HTML
/// drill-down panel. One embedded copy serves both — the grid scales it down in
/// CSS — so this is sized for the *larger* of the two, with enough detail to be
/// worth opening and not a byte more.
///
/// The alternative, embedding the source file untouched, is what made this
/// necessary: a thousand-row report over 2 MB camera JPEGs produced a file no
/// browser would open, to show pictures displayed 110 pixels tall.
pub const MAX_EMBED_EDGE: u32 = 640;

/// JPEG quality for a re-encoded picture. High enough that the drill-down view
/// shows no artefacts at a glance, low enough that the whole point of this
/// exercise survives.
const JPEG_QUALITY: u8 = 82;

/// Re-encode a picture no larger than [`MAX_EMBED_EDGE`] on its longest edge.
///
/// Returns `None` — meaning "embed the original untouched" — whenever shrinking
/// isn't possible or isn't worth it: a picture already within the cap, a format
/// this build has no codec for (GIF, BMP), or a decode/encode that fails. A
/// picture that can't be shrunk is never a reason to lose the picture.
fn downscale(
    bytes: &[u8],
    mime: &str,
    natural: (u32, u32),
) -> Option<(Vec<u8>, String, (u32, u32))> {
    let (w, h) = natural;
    if w.max(h) <= MAX_EMBED_EDGE || w == 0 || h == 0 {
        return None;
    }
    let format = match mime {
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/png" => image::ImageFormat::Png,
        _ => return None,
    };
    let decoded = image::load_from_memory_with_format(bytes, format).ok()?;
    // `thumbnail` is a box filter: markedly faster than Lanczos and visually
    // indistinguishable at these ratios, which matters when a run resolves a
    // thousand of them.
    let scale = f64::from(MAX_EMBED_EDGE) / f64::from(w.max(h));
    let (tw, th) = (
        (f64::from(w) * scale).round().max(1.0) as u32,
        (f64::from(h) * scale).round().max(1.0) as u32,
    );
    let small = decoded.thumbnail(tw, th);
    let mut out = std::io::Cursor::new(Vec::new());
    // A photograph re-encoded as PNG is often *larger* than the JPEG it came
    // from, so each format keeps its own encoder rather than normalising.
    let mime = match format {
        image::ImageFormat::Jpeg => {
            let rgb = small.to_rgb8();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
                .encode_image(&rgb)
                .ok()?;
            "image/jpeg"
        }
        _ => {
            small.write_to(&mut out, image::ImageFormat::Png).ok()?;
            "image/png"
        }
    };
    let out = out.into_inner();
    // Shrinking that didn't shrink anything is just a re-encode: keep the
    // original, which is at worst the same size and at best better quality.
    if out.len() >= bytes.len() {
        return None;
    }
    Some((out, mime.to_string(), (small.width(), small.height())))
}

/// What kind of source a cell value looks like.
enum Shape<'a> {
    /// `data:image/png;base64,<payload>` — the payload.
    DataUri(&'a str),
    Url,
    Path,
    Base64,
    Unknown,
}

fn value_shape(value: &str) -> Shape<'_> {
    if let Some(rest) = value.strip_prefix("data:")
        && let Some((meta, payload)) = rest.split_once(',')
        && meta.contains("base64")
    {
        return Shape::DataUri(payload);
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        return Shape::Url;
    }
    // A bare base64 blob is long, has no path separators and no whitespace.
    // Checking that *before* treating the value as a path avoids a pointless
    // filesystem probe on a 40 KB string, and no real path looks like this.
    if value.len() > 64
        && !value.contains('/')
        && !value.contains('\\')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
    {
        return Shape::Base64;
    }
    // Anything else is treated as a path. A value that isn't one simply fails
    // to read and the cell stays text, which is the required behaviour anyway.
    if value.len() < 4096 && !value.contains('\n') {
        return Shape::Path;
    }
    Shape::Unknown
}

/// Decode a base64 payload, tolerating whitespace, the URL-safe alphabet and
/// missing padding — all of which turn up in real `data:` URIs and in bodies
/// that have been through a JSON pretty-printer. `None` on anything malformed,
/// which leaves the cell as text.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    let cleaned: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    // `Engine` isn't dyn-compatible, so the alphabets are tried by hand rather
    // than over a slice of trait objects.
    let attempts = [
        STANDARD.decode(&cleaned),
        STANDARD_NO_PAD.decode(&cleaned),
        URL_SAFE.decode(&cleaned),
        URL_SAFE_NO_PAD.decode(&cleaned),
    ];
    attempts
        .into_iter()
        .flatten()
        .find(|bytes| !bytes.is_empty())
}

/// A plain, unauthenticated GET for an `http(s)` image value.
///
/// This deliberately bypasses the bound collection: it exists because
/// pre-signed URLs returned in a response are common in exactly this kind of
/// report, and forcing a helper request for one would be ceremony. When the
/// fetch *does* need the collection's auth, proxy or retry settings, the answer
/// is a real request in the flow whose response feeds the column.
fn fetch_url(url: &str) -> Result<Vec<u8>, String> {
    let mut handle = curl::easy::Easy::new();
    handle.url(url).map_err(|e| e.to_string())?;
    handle.follow_location(true).map_err(|e| e.to_string())?;
    handle
        .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    {
        let mut transfer = handle.transfer();
        transfer
            .write_function(|data| {
                buf.extend_from_slice(data);
                Ok(data.len())
            })
            .map_err(|e| e.to_string())?;
        transfer.perform().map_err(|e| e.to_string())?;
    }
    let code = handle.response_code().map_err(|e| e.to_string())?;
    if !(200..300).contains(&code) {
        return Err(format!("HTTP {code}"));
    }
    Ok(buf)
}

/// Sniff an image's MIME type and pixel size from its leading bytes.
///
/// Done by hand rather than by pulling in an image-decoding crate: the writers
/// embed the *encoded* file untouched, so the only thing actually needed is the
/// natural size to scale against, and the four formats a spreadsheet can embed
/// all carry it in a fixed-offset header.
pub fn probe(bytes: &[u8]) -> Option<(&'static str, (u32, u32))> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return jpeg_size(bytes).map(|d| ("image/jpeg", d));
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 {
        let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return Some(("image/png", (w, h)));
    }
    if (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) && bytes.len() >= 10 {
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        return Some(("image/gif", (w, h)));
    }
    if bytes.starts_with(b"BM") && bytes.len() >= 26 {
        let w = i32::from_le_bytes(bytes[18..22].try_into().ok()?).unsigned_abs();
        let h = i32::from_le_bytes(bytes[22..26].try_into().ok()?).unsigned_abs();
        return Some(("image/bmp", (w, h)));
    }
    None
}

/// Walk a JPEG's marker segments to the start-of-frame, which carries the size.
/// A JPEG has no fixed-offset dimension header, so there is no shortcut.
fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        // Standalone markers carry no length field.
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        // SOF0..SOF15, excluding the DHT/JPG/DAC markers that share the range.
        let is_sof =
            (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC;
        if is_sof {
            let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
            return Some((w, h));
        }
        if len < 2 {
            return None;
        }
        i += 2 + len;
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The smallest valid PNG: a 1x1 pixel, used wherever a test needs real
    /// image bytes rather than a plausible-looking string.
    pub(crate) fn png_1x1() -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&[0, 0, 0, 13]);
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0]);
        v
    }

    fn jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        // An APP0 segment first, so the walk has to actually skip something.
        v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        v.extend_from_slice(&[0u8; 14]);
        // SOF0: length, precision, height, width, components.
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&[3]);
        v
    }

    /// Build a JPEG of `size` square carrying enough detail that it doesn't
    /// compress to nothing — a flat colour would shrink so well that the
    /// "didn't actually shrink" guard would fire and mask the real behaviour.
    fn noisy_jpeg(size: u32) -> Vec<u8> {
        let mut buf = image::RgbImage::new(size, size);
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for px in buf.pixels_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let b = seed.to_le_bytes();
            *px = image::Rgb([b[0], b[1], b[2]]);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
            .encode_image(&buf)
            .unwrap();
        out.into_inner()
    }

    /// The whole point of the exercise: a camera-sized picture must not reach
    /// the report at camera size, because the report shows it 110 pixels tall.
    #[test]
    fn a_large_picture_is_embedded_downscaled() {
        let big = noisy_jpeg(1600);
        let mut r = ImageResolver::new();
        let got = r.build("photo.jpg", big.clone()).unwrap();
        assert_eq!(
            got.natural,
            (MAX_EMBED_EDGE, MAX_EMBED_EDGE),
            "the recorded size must describe the bytes actually embedded, or \
             every writer's layout maths is wrong"
        );
        assert!(
            got.bytes.len() < big.len() / 2,
            "expected a real saving, got {} from {}",
            got.bytes.len(),
            big.len()
        );
        assert_eq!(got.mime, "image/jpeg");
        assert!(probe(&got.bytes).is_some(), "and it must still be an image");
    }

    /// A picture already small enough is embedded untouched: re-encoding it
    /// would only lose quality to save nothing.
    #[test]
    fn a_small_picture_is_left_exactly_as_it_was() {
        let small = noisy_jpeg(64);
        let mut r = ImageResolver::new();
        let got = r.build("thumb.jpg", small.clone()).unwrap();
        assert_eq!(got.bytes, small);
        assert_eq!(got.natural, (64, 64));
    }

    /// A format this build has no encoder for keeps its original bytes rather
    /// than losing the picture.
    #[test]
    fn a_format_without_a_codec_is_still_embedded() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&2000u16.to_le_bytes());
        gif.extend_from_slice(&2000u16.to_le_bytes());
        gif.extend_from_slice(&[0u8; 64]);
        let mut r = ImageResolver::new();
        let got = r.build("big.gif", gif.clone()).unwrap();
        assert_eq!(got.bytes, gif, "unshrinkable is not the same as unusable");
        assert_eq!(got.natural, (2000, 2000));
    }

    #[test]
    fn probe_reads_png_gif_bmp_and_jpeg_dimensions() {
        assert_eq!(probe(&png_1x1()), Some(("image/png", (1, 1))));
        assert_eq!(probe(&jpeg(640, 480)), Some(("image/jpeg", (640, 480))));
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0x20, 0x00, 0x10, 0x00]);
        assert_eq!(probe(&gif), Some(("image/gif", (32, 16))));
        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&[0u8; 16]);
        bmp.extend_from_slice(&100i32.to_le_bytes());
        // A bottom-up BMP stores a negative height; the magnitude is the size.
        bmp.extend_from_slice(&(-50i32).to_le_bytes());
        assert_eq!(probe(&bmp), Some(("image/bmp", (100, 50))));
    }

    #[test]
    fn probe_rejects_non_image_bytes() {
        assert_eq!(probe(b"{\"not\": \"an image\"}"), None);
        assert_eq!(probe(b""), None);
    }

    #[test]
    fn base64_round_trips_through_a_data_uri() {
        let png = png_1x1();
        let b64 = encode_base64(&png);
        let uri = format!("data:image/png;base64,{b64}");
        let mut r = ImageResolver::new();
        let got = r.resolve(&uri, None).expect("decoded");
        assert_eq!(got.bytes, png);
        assert_eq!(got.mime, "image/png");
        assert_eq!(got.natural, (1, 1));
    }

    #[test]
    fn a_local_path_resolves_against_the_run_root() {
        let d = std::env::temp_dir().join(format!(
            "paperboy_img_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("face.png"), png_1x1()).unwrap();
        let mut r = ImageResolver::new();
        let got = r.resolve("face.png", Some(&d)).expect("read");
        assert_eq!(got.mime, "image/png");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A value that isn't a picture leaves the cell as text and records why,
    /// rather than failing the run: the report's subject is the API run, not
    /// the illustration beside it.
    #[test]
    fn an_unreadable_value_resolves_to_nothing_with_a_note() {
        let mut r = ImageResolver::new();
        assert!(r.resolve("no/such/file.png", None).is_none());
        assert!(
            r.notes.iter().any(|n| n.contains("unreadable")),
            "{:?}",
            r.notes
        );
    }

    #[test]
    fn a_value_that_is_not_an_image_format_is_left_as_text() {
        let d = std::env::temp_dir().join(format!(
            "paperboy_img_bad_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("notes.txt"), "just some text").unwrap();
        let mut r = ImageResolver::new();
        assert!(r.resolve("notes.txt", Some(&d)).is_none());
        assert!(r.notes.iter().any(|n| n.contains("not a recognised")));
        std::fs::remove_dir_all(&d).ok();
    }

    /// The same source referenced by many rows is read once -- the common shape
    /// of a per-row report over a shared set of inputs.
    #[test]
    fn the_same_source_is_only_loaded_once() {
        let d = std::env::temp_dir().join(format!(
            "paperboy_img_cache_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("face.png");
        std::fs::write(&f, png_1x1()).unwrap();
        let mut r = ImageResolver::new();
        assert!(r.resolve("face.png", Some(&d)).is_some());
        // Deleting the file proves the second resolve came from the cache.
        std::fs::remove_file(&f).unwrap();
        assert!(r.resolve("face.png", Some(&d)).is_some());
        std::fs::remove_dir_all(&d).ok();
    }

    fn encode_base64(bytes: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }
}
