use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The image file a paste names: terminals paste a dropped file as its path, shell-escaped or quoted.
pub fn pasted_path(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    let quoted = ['\'', '"'].into_iter().find_map(|quote| text.strip_prefix(quote)?.strip_suffix(quote));
    let path = match quoted {
        Some(quoted) => PathBuf::from(quoted),
        None => PathBuf::from(unescape(text)),
    };
    (media_type(&path).is_some() && path.is_file()).then_some(path)
}

/// Undoes shell escaping: `\x` becomes `x` for any `x`.
fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => out.extend(chars.next()),
            _ => out.push(ch),
        }
    }
    out
}

/// The Anthropic API refuses images whose base64 data is over 5 MB; base64 carries 3 bytes in 4 chars.
const MAX_IMAGE_BYTES: u64 = (5 << 20) / 4 * 3;

/// Reads an image file into an API image block.
pub fn block(path: &Path) -> Result<Value, String> {
    let size = std::fs::metadata(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?.len();
    if size > MAX_IMAGE_BYTES {
        return Err(format!(
            "{} is {}; images over {} are not attached. Shrink it first, e.g. magick in.png -resize 50% out.jpg.",
            path.display(),
            crate::tools::format_size(size as usize),
            crate::tools::format_size(MAX_IMAGE_BYTES as usize)
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    bytes_block(&bytes).ok_or_else(|| format!("Not a PNG, JPEG, GIF or WebP image: {}", path.display()))
}

fn bytes_block(bytes: &[u8]) -> Option<Value> {
    let media_type = sniffed_media_type(bytes)?;
    Some(json!({"type": "image", "source": {"type": "base64", "media_type": media_type, "data": base64(bytes)}}))
}

/// The API rejects a media type that disagrees with the bytes, and a `.png` file can hold a JPEG.
fn sniffed_media_type(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xff, 0xd8, 0xff, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

const CLIPBOARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Reads the clipboard image into an API image block: `osascript` on macOS, `wl-paste` on Wayland, `xclip` on X11.
pub fn from_clipboard() -> Result<Value, String> {
    if cfg!(target_os = "macos") {
        return from_macos_clipboard();
    }
    let (program, args): (&str, &[&str]) =
        if std::env::var_os("WAYLAND_DISPLAY").is_some() { ("wl-paste", &["--type", "image/png"]) } else { ("xclip", &["-selection", "clipboard", "-target", "image/png", "-out"]) };
    // Runs on the UI thread; a clipboard owner that never answers would freeze the TUI.
    let output = crate::process::output_within(crate::process::command(program).args(args), CLIPBOARD_TIMEOUT).map_err(|e| format!("Cannot run {program}: {e}"))?.ok_or("Clipboard read timed out.")?;
    let block = if output.status.success() { bytes_block(&output.stdout) } else { None };
    block.ok_or_else(|| "No image in the clipboard".into())
}

/// Saves the clipboard image as PNG and reads it into an API image block.
fn from_macos_clipboard() -> Result<Value, String> {
    let path = std::env::temp_dir().join(format!("agent-clipboard-{}.png", std::process::id()));
    // The path goes in as an argument, so a `"` in $TMPDIR cannot end a string literal and run as AppleScript.
    let script = "on run argv\nset f to open for access (POSIX file (item 1 of argv)) with write permission\nset eof f to 0\nwrite (the clipboard as «class PNGf») to f\nclose access f\nend run";
    let output = crate::process::output_within(crate::process::command("osascript").args(["-e", script]).arg(&path), CLIPBOARD_TIMEOUT)
        .map_err(|e| format!("Cannot run osascript: {e}"))?
        .ok_or("Clipboard read timed out.")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    // -1700 is "can't make into type": the clipboard holds no image. Anything else (a sandbox denial) is shown.
    let block = if output.status.success() {
        block(&path)
    } else if stderr.contains("-1700") {
        Err("No image in the clipboard".into())
    } else {
        Err(format!("osascript: {}", stderr.trim()))
    };
    let _ = std::fs::remove_file(&path);
    block
}

pub fn media_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn base64(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let mut padded = [0; 4];
        padded[1..=chunk.len()].copy_from_slice(chunk);
        let n = u32::from_be_bytes(padded);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads_short_chunks() {
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn a_dropped_image_path_is_recognised() {
        let dir = std::env::temp_dir().join(format!("agent-image-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("capture screen (1) & co.png");
        std::fs::write(&file, b"png").unwrap();
        let escaped = file.display().to_string().replace(' ', "\\ ").replace('(', "\\(").replace(')', "\\)").replace('&', "\\&");
        assert_eq!(pasted_path(&escaped), Some(file.clone()));
        assert_eq!(pasted_path(&format!("'{}'", file.display())), Some(file.clone()));
        assert_eq!(pasted_path(&format!("\"{}\"", file.display())), Some(file.clone()));
        assert_eq!(pasted_path("just some text"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn media_type_follows_the_bytes_not_the_extension() {
        let file = std::env::temp_dir().join(format!("agent-jpeg-as-png-{}.png", std::process::id()));
        std::fs::write(&file, [0xff, 0xd8, 0xff, 0xe0]).unwrap();
        let block = block(&file);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(block.unwrap()["source"]["media_type"], "image/jpeg");
    }

    #[test]
    fn an_image_over_the_api_limit_is_refused_before_reading() {
        let file = std::env::temp_dir().join(format!("agent-big-image-{}.png", std::process::id()));
        std::fs::File::create(&file).unwrap().set_len(MAX_IMAGE_BYTES + 1).unwrap();
        let block = block(&file);
        std::fs::remove_file(&file).unwrap();
        assert!(block.unwrap_err().contains("images over"));
    }
}
