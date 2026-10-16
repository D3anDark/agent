use super::{MAX_INLINE_CHARS, Page, Web, fetch_entry, github, html, http_agent};
use serde_json::Value;
use std::io::Read;
use std::time::Duration;

const MAX_BYTES: u64 = 5 * 1024 * 1024;
const ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8";

impl Web {
    pub(super) fn fetch_content(&mut self, input: &Value) -> Result<String, String> {
        let mut urls = super::string_list(&input["urls"]);
        if urls.is_empty()
            && let Some(url) = input["url"].as_str()
        {
            urls.push(url.to_string());
        }
        urls.retain(|u| !u.trim().is_empty());
        if urls.is_empty() {
            return Err("Error: No URL provided. Use the 'url' parameter, or 'urls' for parallel fetches.".into());
        }
        let raw = input["mode"].as_str() == Some("raw");
        let pages = fetch_all(&urls, raw, input["forceClone"].as_bool().unwrap_or(false), input["proxy"].as_str());
        let id = self.store(fetch_entry(&pages))?;

        if let [page] = pages.as_slice() {
            if let Some(error) = &page.error {
                return Err(format!("Error: {error}"));
            }
            return Ok(first_page(&page.content, &id));
        }
        let mut output = String::from("## Fetched URLs\n\n");
        for page in &pages {
            match &page.error {
                Some(error) => output.push_str(&format!("- {}: Error - {error}\n", page.url)),
                None => {
                    let name = if page.title.is_empty() { &page.url } else { &page.title };
                    output.push_str(&format!("- {name} ({} chars)\n", page.content.chars().count()));
                }
            }
        }
        output.push_str(&format!("\n---\nUse get_search_content({{ responseId: \"{id}\", urlIndex: 0 }}) to retrieve bounded content slices."));
        Ok(output)
    }
}

/// pi's initialContentSlice: up to MAX_INLINE_CHARS, cut after a newline in the last fifth when one is there.
fn first_page(content: &str, id: &str) -> String {
    let length = content.chars().count();
    if length <= MAX_INLINE_CHARS {
        return content.to_string();
    }
    let mut end = MAX_INLINE_CHARS;
    let head = super::char_slice(content, 0, end + 1);
    if let Some(newline) = head.rfind('\n') {
        let line_end = head[..newline].chars().count();
        if line_end >= MAX_INLINE_CHARS * 4 / 5 {
            end = line_end + 1;
        }
    }
    let page = super::char_slice(content, 0, end);
    format!(
        "{page}\n\n---\nShowing {end} of {length} chars, {} of {} bytes, and {} of {} lines. Use get_search_content({{ responseId: \"{id}\", urlIndex: 0, offset: {end} }}) for the next slice.",
        page.len(),
        content.len(),
        page.split('\n').count(),
        content.split('\n').count()
    )
}

/// Fetches every URL on its own thread; each page carries its error instead of failing the batch.
pub fn fetch_all(urls: &[String], raw: bool, force_clone: bool, proxy: Option<&str>) -> Vec<Page> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = urls.iter().map(|url| scope.spawn(move || fetch_one(url, raw, force_clone, proxy))).collect();
        let pages = handles.into_iter().zip(urls).map(|(handle, url)| {
            handle.join().unwrap_or_else(|_| Err("fetch thread panicked".into())).map_or_else(
                |error| Page { url: url.clone(), title: String::new(), content: String::new(), error: Some(error) },
                |(title, content)| Page { url: url.clone(), title, content, error: None },
            )
        });
        pages.collect()
    })
}

/// (title, content) of one URL; GitHub code URLs are cloned, the rest go over HTTP.
fn fetch_one(url: &str, raw: bool, force_clone: bool, proxy: Option<&str>) -> Result<(String, String), String> {
    let url = url.trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!("Unsupported URL: {url}. Use an http or https URL."));
    }
    if !raw && let Some(repo) = github::parse(url) {
        // shortcut: a failed clone falls back to the plain page fetch, pi tries the GitHub API first.
        if let Ok(content) = github::fetch(&repo, force_clone) {
            return Ok((format!("{}/{}", repo.owner, repo.repo), content));
        }
    }
    let agent = http_agent(proxy, Duration::from_secs(30))?;
    let mut response = agent.get(url).header("Accept", ACCEPT).header("User-Agent", "Mozilla/5.0 (compatible; agent)").call().map_err(|e| e.to_string())?;
    let status = response.status();
    if !raw && !status.is_success() {
        return Err(format!("HTTP {}: {}", status.as_u16(), status.canonical_reason().unwrap_or_default()));
    }
    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    let content_type = header("content-type").unwrap_or_default().to_lowercase();
    if let Some(size) = header("content-length").and_then(|l| l.parse::<u64>().ok())
        && size > MAX_BYTES
    {
        return Err(format!("Response too large ({:.1}MB)", size as f64 / 1024.0 / 1024.0));
    }
    let mime = content_type.split(';').next().unwrap_or_default().trim().to_string();
    if raw && !is_text(&mime) {
        return Err(format!("Unsupported content type in raw mode: {}", if mime.is_empty() { "missing" } else { &mime }));
    }
    if !raw && ["octet-stream", "image/", "audio/", "video/", "application/zip"].iter().any(|kind| content_type.contains(kind)) {
        return Err(format!("Unsupported content type: {mime}"));
    }
    let mut bytes = Vec::new();
    response.body_mut().as_reader().take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("Response too large (over {}MB)", MAX_BYTES / 1024 / 1024));
    }
    let body = String::from_utf8_lossy(&bytes).into_owned();
    if !raw && (content_type.contains("text/html") || content_type.contains("xhtml+xml")) {
        return Ok(html::to_markdown(&body, url));
    }
    Ok((text_title(&body, url), body))
}

fn is_text(mime: &str) -> bool {
    mime.starts_with("text/")
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
        || ["application/json", "application/xml", "application/javascript", "application/x-javascript"].contains(&mime)
}

/// pi's extractTextTitle: the first `#` or `##` heading, else the last path segment, else the URL.
fn text_title(text: &str, url: &str) -> String {
    for line in text.lines() {
        let hashes = line.len() - line.trim_start_matches('#').len();
        if (1..=2).contains(&hashes) && line[hashes..].starts_with(char::is_whitespace) {
            let title = line[hashes..].trim().replace('*', "");
            if !title.is_empty() {
                return title;
            }
        }
    }
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let path = path.split_once("://").map_or(path, |(_, rest)| rest);
    match path.split_once('/') {
        Some((_, path)) => path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(url).to_string(),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_page_cuts_at_a_late_newline_and_points_to_the_rest() {
        let content = format!("{}\n{}", "a".repeat(29_000), "b".repeat(2_000));
        let page = first_page(&content, "x1");
        assert!(page.starts_with(&format!("{}\n\n\n---\nShowing 29001 of 31001 chars, 29001 of 31001 bytes, and 2 of 2 lines.", "a".repeat(29_000))), "{}", &page[29_000..]);
        assert!(page.ends_with("get_search_content({ responseId: \"x1\", urlIndex: 0, offset: 29001 }) for the next slice."));
        assert_eq!(first_page("short", "x1"), "short");
    }

    #[test]
    fn text_title_prefers_a_heading_then_the_path() {
        assert_eq!(text_title("intro\n## **Usage** guide\n", "https://a.dev/x"), "Usage guide");
        assert_eq!(text_title("### deep\nplain", "https://a.dev/docs/notes.txt?v=1"), "notes.txt");
        assert_eq!(text_title("", "https://a.dev"), "https://a.dev");
    }

    #[test]
    fn raw_mode_accepts_text_like_types_only() {
        assert!(is_text("application/ld+json") && is_text("text/plain") && is_text("application/xhtml+xml"));
        assert!(!is_text("image/png") && !is_text(""));
    }
}
