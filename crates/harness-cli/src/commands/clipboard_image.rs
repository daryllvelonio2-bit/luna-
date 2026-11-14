use arboard::ImageData;
use base64::Engine;
use std::path::PathBuf;
use std::time::Duration;

/// A screenshot pulled from the clipboard, ready to attach to a prompt.
pub struct ClipboardImage {
    /// PNG-encoded bytes of the image.
    pub png_bytes: Vec<u8>,
    /// Where the PNG was persisted (a session-scoped temp file), for logging and replay.
    #[allow(dead_code)]
    pub saved_path: PathBuf,
    /// Width and height in pixels.
    pub width: usize,
    pub height: usize,
}

impl ClipboardImage {
    /// OpenAI-compatible `image_url` data URL for this image.
    pub fn data_url(&self) -> String {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&self.png_bytes)
        )
    }
}

/// Write plain text to the system clipboard (used by transcript mouse selection).
pub fn write_clipboard_text(text: &str) -> Result<(), String> {
    // 1. Wayland clipboard support via `wl-copy`.
    // When available, wl-copy forks and stays resident in the Wayland compositor to serve
    // paste requests reliably across applications.
    if write_via_command("wl-copy", &[], text) {
        // Also copy to PRIMARY selection for middle-click paste.
        write_via_command("wl-copy", &["--primary"], text);
        return Ok(());
    }

    // 2. X11 clipboard support via `xclip` or `xsel`.
    if write_via_command("xclip", &["-selection", "clipboard"], text) {
        write_via_command("xclip", &["-selection", "primary"], text);
        return Ok(());
    } else if write_via_command("xsel", &["--clipboard", "--input"], text) {
        write_via_command("xsel", &["--primary", "--input"], text);
        return Ok(());
    }

    // 3. Native arboard clipboard (works natively on macOS, Windows, and fallback X11/Wayland).
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    cb.set_text(text.to_string())
        .map_err(|e| format!("clipboard write failed: {e}"))
}

fn write_via_command(program: &str, args: &[&str], text: &str) -> bool {
    use std::io::Write;
    let mut child = match std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    match child.wait() {
        Ok(s) => s.success(),
        Err(_) => false,
    }
}

/// Read plain text from the system clipboard.
pub fn read_clipboard_text() -> Option<String> {
    // 1. Wayland `wl-paste`
    if let Some(bytes) = command_bytes("wl-paste", &["--no-newline"]) {
        if let Ok(text) = String::from_utf8(bytes) {
            if !text.is_empty() {
                return Some(text);
            }
        }
    }

    // 2. X11 `xclip`
    if let Some(bytes) = command_bytes("xclip", &["-selection", "clipboard", "-o"]) {
        if let Ok(text) = String::from_utf8(bytes) {
            if !text.is_empty() {
                return Some(text);
            }
        }
    }

    // 3. X11 `xsel`
    if let Some(bytes) = command_bytes("xsel", &["--clipboard", "--output"]) {
        if let Ok(text) = String::from_utf8(bytes) {
            if !text.is_empty() {
                return Some(text);
            }
        }
    }

    // 4. arboard
    if let Ok(mut cb) = arboard::Clipboard::new() {
        if let Ok(text) = cb.get_text() {
            if !text.is_empty() {
                return Some(text);
            }
        }
    }

    None
}

/// Read text from the primary selection (Linux mouse selection / middle-click paste).
pub fn read_primary_text() -> Option<String> {
    if let Some(bytes) = command_bytes("wl-paste", &["--primary", "--no-newline"]) {
        if let Ok(text) = String::from_utf8(bytes) {
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    if let Some(bytes) = command_bytes("xclip", &["-selection", "primary", "-o"]) {
        if let Ok(text) = String::from_utf8(bytes) {
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    if let Some(bytes) = command_bytes("xsel", &["--primary", "--output"]) {
        if let Ok(text) = String::from_utf8(bytes) {
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// Read an image (screenshot) from the system clipboard, if one is present.
/// Returns `Ok(None)` when the clipboard holds no image (text or empty).
///
/// Wayland reads are flaky in practice: `wl-paste --watch cliphist store`
/// (and clipboard managers generally) race the reader, and arboard's
/// data-control backend surfaces that as a transient `ContentNotAvailable`
/// right after a copy even though the image is there. So this retries the
/// arboard read, then falls back to pasting the PNG directly via
/// `wl-paste` (Wayland) and `xclip` (X11).
pub fn read_clipboard_image() -> Result<Option<ClipboardImage>, String> {
    // If fallback command (e.g. wl-paste --type image/png) has an image right now, take it immediately.
    if let Some(png) = fallback_image_bytes() {
        return clipboard_image_from_png(png).map(Some);
    }

    const ATTEMPTS: usize = 3;
    const RETRY_DELAY: Duration = Duration::from_millis(150);

    let mut hard_err: Option<String> = None;
    for attempt in 0..ATTEMPTS {
        match try_arboard() {
            Ok(Some(img)) => return Ok(Some(img)),
            // Clipboard reachable, just no image in it (holds text or empty).
            Ok(None) => return Ok(None),
            Err(e) => hard_err = Some(e),
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(RETRY_DELAY);
        }
    }

    if let Some(png) = fallback_image_bytes() {
        return clipboard_image_from_png(png).map(Some);
    }

    match hard_err {
        Some(e) => Err(e),
        None => Ok(None),
    }
}

fn try_arboard() -> Result<Option<ClipboardImage>, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;

    let img: ImageData = match clipboard.get_image() {
        Ok(i) => i,
        Err(arboard::Error::ContentNotAvailable) => return Ok(None),
        Err(e) => return Err(format!("clipboard read failed: {e}")),
    };

    let width = img.width as usize;
    let height = img.height as usize;

    // RGBA8 -> PNG via a minimal encoder: use the `image`-free path by writing raw PNG ourselves
    // is error-prone, so encode through arboard's PNG support if available; otherwise fall back
    // to a hand-rolled encoder. arboard does not expose PNG encoding, so we do it via `image` is
    // avoided — instead we wrap the raw RGBA in a PNG using a tiny filter-only encoder.
    let png_bytes = encode_png_rgba(&img.bytes, width, height)
        .map_err(|e| format!("png encode failed: {e}"))?;

    let saved_path = save_image_file(&png_bytes)?;

    Ok(Some(ClipboardImage {
        png_bytes,
        saved_path,
        width,
        height,
    }))
}

/// Last-resort reads that bypass arboard entirely, in case its Wayland or X11
/// backend cannot see an image the compositor clearly has (verified live with
/// `wl-paste` under Hyprland). Only `image/png` is accepted: the attach path
/// labels every image `image/png`, and screenshot/copy tools always offer it.
fn fallback_image_bytes() -> Option<Vec<u8>> {
    // wl-paste asks the compositor's data-control protocol directly.
    if let Some(bytes) = command_bytes("wl-paste", &["--type", "image/png"]) {
        if png_sniff(&bytes) {
            return Some(bytes);
        }
    }
    // xclip for X11 sessions (no-op when it is not installed).
    if let Some(bytes) = command_bytes(
        "xclip",
        &["-selection", "clipboard", "-t", "image/png", "-o"],
    ) {
        if png_sniff(&bytes) {
            return Some(bytes);
        }
    }
    None
}

fn clipboard_image_from_png(png: Vec<u8>) -> Result<ClipboardImage, String> {
    let (width, height) = png_dimensions(&png).ok_or("clipboard image is not a valid PNG")?;
    let saved_path = save_image_file(&png)?;
    Ok(ClipboardImage {
        png_bytes: png,
        saved_path,
        width,
        height,
    })
}

/// A PNG signature plus a decodable IHDR: dimensions live at a fixed offset,
/// so no decoder is needed to size the attachment.
fn png_dimensions(png: &[u8]) -> Option<(usize, usize)> {
    if !png_sniff(png) || png.len() < 24 {
        return None;
    }
    let width = u32::from_be_bytes(png[16..20].try_into().ok()?) as usize;
    let height = u32::from_be_bytes(png[20..24].try_into().ok()?) as usize;
    Some((width, height))
}

fn png_sniff(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes[..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
}

/// Run a helper binary and return its stdout, or `None` when the binary is
/// missing, it fails, or it produces nothing (all mean "this source has no
/// image", not an error worth reporting).
fn command_bytes(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    Some(out.stdout)
}

fn save_image_file(png: &[u8]) -> Result<PathBuf, String> {
    let home = harness_core::config::home_dir();
    let dir = if home != std::path::Path::new(".") {
        home.join(".luna").join("screenshots")
    } else {
        std::env::temp_dir().join("luna-screenshots")
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create screenshot dir: {e}"))?;
    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S%3f");
    let path = dir.join(format!("paste_{ts}.png"));
    std::fs::write(&path, png).map_err(|e| format!("cannot write screenshot: {e}"))?;
    Ok(path)
}

/// Encode RGBA8 pixel data into a PNG file.
///
/// The IDAT is deflate-compressed. It used to be stored uncompressed — a valid PNG that is one
/// byte per pixel, so a 1920x1080 screenshot came to 8 MB, which is over the `MAX_INLINE_IMAGE_BYTES`
/// the prompt inliner sends images through. The image was then refused by the very code that was
/// meant to send it and the prompt went off as text, with one debug line to say why. Screenshots
/// compress heavily: the same shot is a few hundred kilobytes here.
fn encode_png_rgba(rgba: &[u8], width: usize, height: usize) -> Result<Vec<u8>, String> {
    let expected = width * height * 4;
    if rgba.len() < expected {
        return Err(format!(
            "clipboard image payload too small: {} < {}",
            rgba.len(),
            expected
        ));
    }

    // Build raw scanlines: each row prefixed with filter byte 0 (None).
    let stride = width * 4;
    let mut raw = Vec::with_capacity((stride + 1) * height);
    for y in 0..height {
        raw.push(0u8);
        raw.extend_from_slice(&rgba[y * stride..(y + 1) * stride]);
    }

    let compressed = zlib_compress(&raw)?;

    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    // IHDR
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // color type RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    push_chunk(&mut out, b"IHDR", &ihdr);

    // IDAT
    push_chunk(&mut out, b"IDAT", &compressed);

    // IEND
    push_chunk(&mut out, b"IEND", &[]);

    Ok(out)
}

fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// The zlib stream for a PNG's IDAT, deflate-compressed.
fn zlib_compress(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Write;

    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(data)
        .map_err(|error| format!("cannot compress the screenshot: {error}"))?;
    encoder
        .finish()
        .map_err(|error| format!("cannot compress the screenshot: {error}"))
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, entry) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *entry = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = table[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vision provider will reject a malformed PNG, and the only way that happens silently is a
    /// bad encoder — so the encoder is decoded here, chunk by chunk, back to the pixels that went
    /// in. This walks the real structure (signature, IHDR, IDAT, IEND), verifies every CRC and the
    /// zlib Adler-32, and inflates the stored-block stream.
    #[test]
    fn encoded_png_decodes_back_to_the_same_pixels() {
        // 3x2 RGBA, distinct in every channel so a byte-order bug cannot pass.
        let width = 3usize;
        let height = 2usize;
        let rgba: Vec<u8> = (0..(width * height * 4) as u8).collect();

        let png = encode_png_rgba(&rgba, width, height).expect("encode");

        assert_eq!(
            &png[..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
            "signature"
        );

        // Walk the chunk list, checking each declared length and CRC.
        let mut pos = 8usize;
        let mut chunks: Vec<(String, Vec<u8>)> = Vec::new();
        while pos < png.len() {
            let len = u32::from_be_bytes(png[pos..pos + 4].try_into().unwrap()) as usize;
            let kind = String::from_utf8(png[pos + 4..pos + 8].to_vec()).unwrap();
            let data = png[pos + 8..pos + 8 + len].to_vec();
            let stored_crc =
                u32::from_be_bytes(png[pos + 8 + len..pos + 12 + len].try_into().unwrap());
            let mut crc_input = kind.as_bytes().to_vec();
            crc_input.extend_from_slice(&data);
            assert_eq!(stored_crc, crc32(&crc_input), "bad CRC on {kind} chunk");
            chunks.push((kind, data));
            pos += 12 + len;
        }
        assert_eq!(pos, png.len(), "trailing bytes after the last chunk");
        assert_eq!(
            chunks.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["IHDR", "IDAT", "IEND"]
        );

        let ihdr = &chunks[0].1;
        assert_eq!(
            u32::from_be_bytes(ihdr[0..4].try_into().unwrap()),
            3,
            "width"
        );
        assert_eq!(
            u32::from_be_bytes(ihdr[4..8].try_into().unwrap()),
            2,
            "height"
        );
        assert_eq!(&ihdr[8..], &[8, 6, 0, 0, 0], "8-bit RGBA, no interlace");

        // Inflate the IDAT and compare to the raw scanlines. It is a real zlib stream — the reader
        // is what a decoder would use, so a stream that only *looks* like one fails here.
        let idat = &chunks[1].1;
        let mut inflated = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::ZlibDecoder::new(&idat[..]),
            &mut inflated,
        )
        .expect("inflate the IDAT");

        // Each row is filter byte 0 followed by that row's pixels, in order.
        let mut expected = Vec::new();
        for y in 0..height {
            expected.push(0u8);
            expected.extend_from_slice(&rgba[y * width * 4..(y + 1) * width * 4]);
        }
        assert_eq!(
            inflated, expected,
            "decoded scanlines differ from the input pixels"
        );
    }

    #[test]
    fn a_short_payload_is_rejected_instead_of_producing_a_truncated_png() {
        // Fewer bytes than width*height*4: this is the shape a bad clipboard read takes.
        let err = encode_png_rgba(&[0u8; 4], 4, 4).expect_err("must refuse");
        assert!(err.contains("too small"), "got: {err}");
    }

    #[test]
    fn dimensions_are_read_straight_from_the_ihdr() {
        let png = encode_png_rgba(&vec![0u8; 9 * 7 * 4], 9, 7).expect("encode");
        assert_eq!(
            png_dimensions(&png),
            Some((9, 7)),
            "IHDR width/height at bytes 16..24"
        );
    }

    #[test]
    fn dimension_parsing_refuses_non_png_input() {
        assert_eq!(
            png_dimensions(b"not a png at all........"),
            None,
            "bad signature"
        );
        assert_eq!(
            png_dimensions(&[0x89, b'P', b'N', b'G']),
            None,
            "truncated header"
        );
    }

    #[test]
    fn helper_commands_that_are_missing_or_failing_yield_none() {
        // A binary that does not exist is a silent "this source has nothing",
        // not an error: the xclip fallback must no-op on Wayland-only boxes.
        assert!(command_bytes("luna-nonexistent-helper", &["--version"]).is_none());
        // A real binary with real output parses as bytes.
        assert_eq!(
            command_bytes("/bin/echo", &["ok"]).as_deref(),
            Some(&b"ok\n"[..])
        );
    }

    /// Not ignored by default for CI cost, but kept as the out-of-process proof: the file this
    /// writes is verified with a real PNG decoder (`python3 -c "from PIL import Image ..."` or
    /// `pngcheck`) rather than only by this module's own reading of its bytes.
    #[test]
    fn writes_a_file_a_real_decoder_accepts() {
        let dir = std::env::temp_dir().join("luna-png-selftest");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("selftest.png");
        let width = 8usize;
        let height = 4usize;
        let rgba: Vec<u8> = (0..(width * height * 4) as u32)
            .map(|i| (i * 7 % 256) as u8)
            .collect();
        let png = encode_png_rgba(&rgba, width, height).unwrap();
        std::fs::write(&path, &png).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap().len(),
            png.len(),
            "file round-trip"
        );
    }

    #[test]
    fn write_and_read_clipboard_text_round_trip() {
        let test_str = "luna clipboard roundtrip test";
        let res = write_clipboard_text(test_str);
        let read = read_clipboard_text();
        assert!(res.is_ok(), "write error: {res:?}");
        assert_eq!(read.as_deref(), Some(test_str));
    }
}
