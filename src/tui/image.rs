use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The image file a paste names: terminals paste a dropped file as its path, shell-escaped or quoted.
pub fn pasted_path(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    let path = match text.strip_prefix('\'').and_then(|rest| rest.strip_suffix('\'')) {
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

/// Reads an image file into an API image block.
pub fn block(path: &Path) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let media_type = sniffed_media_type(&bytes).ok_or_else(|| format!("Not an image: {}", path.display()))?;
    Ok(json!({"type": "image", "source": {"type": "base64", "media_type": media_type, "data": base64(&bytes)}}))
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

/// Saves the clipboard image as PNG and reads it into an API image block. macOS only, like `pbcopy`.
pub fn from_clipboard() -> Result<Value, String> {
    let path = std::env::temp_dir().join(format!("agent-clipboard-{}.png", std::process::id()));
    let script = format!(
        "set f to open for access (POSIX file \"{}\") with write permission\nset eof f to 0\nwrite (the clipboard as «class PNGf») to f\nclose access f",
        path.display()
    );
    let output = Command::new("osascript").arg("-e").arg(script).output().map_err(|e| format!("Cannot run osascript: {e}"))?;
    let block = if output.status.success() { block(&path) } else { Err("No image in the clipboard".into()) };
    let _ = std::fs::remove_file(&path);
    block
}

fn media_type(path: &Path) -> Option<&'static str> {
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
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
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
}
