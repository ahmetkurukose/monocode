//! A page's icon for its tab in Browser mode's sidebar (docs/native/modes.md):
//! fetched once by the address Chromium reports, or read from a `data:` URL,
//! and told apart by its first bytes. Nothing here touches a browser profile:
//! the request carries no cookies, as Chrome's own favicon fetches do not.

use std::io::Read;
use std::time::Duration;

/// Icons bigger than this are not icons.
const MAX_BYTES: u64 = 512 * 1024;
const TIMEOUT: Duration = Duration::from_secs(6);

/// What an icon's bytes are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Ico,
    Bmp,
    Svg,
}

/// The format of `bytes`, by their signature; `None` for anything else.
pub fn sniff(bytes: &[u8]) -> Option<IconFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(IconFormat::Png);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(IconFormat::Jpeg);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(IconFormat::Gif);
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(IconFormat::Webp);
    }
    if bytes.starts_with(&[0, 0, 1, 0]) {
        return Some(IconFormat::Ico);
    }
    if bytes.starts_with(b"BM") {
        return Some(IconFormat::Bmp);
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_lowercase();
    if head.trim_start().starts_with('<') && head.contains("<svg") {
        return Some(IconFormat::Svg);
    }
    None
}

/// The icon at `url` (`http`, `https` or a base64 `data:` URL) and its
/// format. Blocks: call it off the main thread.
pub fn fetch(url: &str) -> Result<(Vec<u8>, IconFormat), String> {
    let bytes = if let Some(data) = url.strip_prefix("data:") {
        decode_data_url(data)?
    } else if url.starts_with("https://") || url.starts_with("http://") {
        download(url)?
    } else {
        return Err(format!("not an icon address: {url}"));
    };
    let format = sniff(&bytes).ok_or_else(|| "not an image".to_string())?;
    Ok((bytes, format))
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(TIMEOUT)
        .redirects(4)
        .user_agent("Mozilla/5.0 (Macintosh) Monocode")
        .build();
    let response = agent.get(url).call().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("icon too large".into());
    }
    Ok(bytes)
}

/// The bytes of a `data:` URL's body (after `data:`), base64 only.
fn decode_data_url(data: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let (meta, body) = data.split_once(',').ok_or("malformed data URL")?;
    if !meta.ends_with(";base64") {
        // Inline SVG is the only plain-text icon worth keeping.
        let text = percent_decode(body);
        return Ok(text.into_bytes());
    }
    base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .map_err(|e| e.to_string())
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        let hex = (bytes[ix] == b'%' && ix + 2 < bytes.len())
            .then(|| text.get(ix + 1..ix + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        if let Some(byte) = hex {
            out.push(byte);
            ix += 3;
            continue;
        }
        out.push(bytes[ix]);
        ix += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_are_told_apart_by_their_first_bytes() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some(IconFormat::Png));
        assert_eq!(sniff(&[0, 0, 1, 0, 1, 0]), Some(IconFormat::Ico));
        assert_eq!(sniff(b"GIF89a..."), Some(IconFormat::Gif));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(IconFormat::Webp));
        assert_eq!(
            sniff(b"  <?xml version='1.0'?><svg xmlns='x'/>"),
            Some(IconFormat::Svg)
        );
        assert_eq!(sniff(b"<!doctype html><html>"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn data_urls_decode_without_a_request() {
        let (bytes, format) = fetch("data:image/png;base64,iVBORw0KGgo=").unwrap();
        assert_eq!(format, IconFormat::Png);
        assert_eq!(bytes.len(), 8);
        let (svg, format) =
            fetch("data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'/%3E").unwrap();
        assert_eq!(format, IconFormat::Svg);
        assert!(String::from_utf8(svg).unwrap().starts_with("<svg"));
        assert!(fetch("chrome://favicon").is_err());
    }
}
