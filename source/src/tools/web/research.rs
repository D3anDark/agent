//! source_check: search a claim, keep ranked sources with cited passages, and store the research artifact.

use super::{Page, Web, fetch, providers, string_list};
use serde_json::{Map, Value, json};

pub(super) const MAX_QUERIES: usize = 8;
const MAX_SOURCES: usize = 20;
const FETCHED_SOURCES: usize = 5;

impl Web {
    pub(super) fn source_check(&mut self, input: &Value) -> Result<String, String> {
        let claim = input["claim"].as_str().map(str::trim).unwrap_or_default();
        if claim.is_empty() {
            return Err("Error: 'claim' is required.".into());
        }
        let mut queries: Vec<String> = string_list(&input["queries"]).iter().map(|q| q.trim().to_string()).filter(|q| !q.is_empty()).collect();
        if queries.is_empty() {
            queries.push(claim.to_string());
        }
        queries.truncate(MAX_QUERIES);
        let config = providers::load_config(&self.config_path, self.provider_order())?;
        let domains = string_list(&input["domainFilter"]);
        let recency = super::recency(&input["recencyFilter"]);
        let proxy = input["proxy"].as_str();

        let mut provider = None;
        let mut summaries = Vec::new();
        let mut errors = Vec::new();
        let mut hits = Vec::new();
        for text in &queries {
            let query = providers::Query {
                text,
                num_results: super::num_results(&input["numResults"]),
                recency,
                domains: &domains,
                include_content: false,
                category: None,
                proxy,
            };
            match providers::search(&config, &input["provider"], &query) {
                Ok(response) => {
                    provider = provider.or(response.providers.first().cloned());
                    if !response.answer.is_empty() {
                        summaries.push(format!("{text}: {}", response.answer));
                    }
                    hits.extend(response.results);
                }
                Err(error) => errors.push(json!({"query": text, "error": error})),
            }
        }
        let mut seen = Vec::new();
        hits.retain(|hit| !seen.contains(&hit.url) && {
            seen.push(hit.url.clone());
            true
        });
        hits.truncate(MAX_SOURCES);
        let fetched = if input["fetchContent"].as_bool().unwrap_or(false) {
            let urls: Vec<String> = hits.iter().take(FETCHED_SOURCES).map(|h| h.url.clone()).collect();
            fetch::fetch_all(&urls, false, false, proxy)
        } else {
            Vec::new()
        };

        let now = crate::process::now_ms();
        let mut sources = Vec::new();
        let mut passages = Vec::new();
        for (index, hit) in hits.iter().enumerate() {
            let rank = index + 1;
            let page = fetched.iter().find(|p| p.url == hit.url);
            let mut source = Map::new();
            source.insert("rank".into(), json!(rank));
            source.insert("url".into(), json!(hit.url));
            source.insert("title".into(), json!(hit.title));
            if !hit.snippet.is_empty() {
                source.insert("snippet".into(), json!(hit.snippet));
            }
            source.insert("quality".into(), json!(classify(&hit.url)));
            source.insert("fetched".into(), json!(page.is_some_and(|p| p.error.is_none())));
            if page.is_some() {
                source.insert("fetch_timestamp".into(), json!(now));
            }
            match page {
                Some(Page { error: Some(error), .. }) => _ = source.insert("fetch_error".into(), json!(error)),
                Some(page) => _ = source.insert("content_hash".into(), json!(hash(&page.content))),
                None => {}
            }
            sources.push(Value::Object(source));
            passages.extend(source_passages(rank, &hit.url, &hit.snippet, page, claim));
        }

        let (status, confidence, rationale) = if passages.is_empty() {
            ("missing-evidence", 0.2, "No passages were retrieved for the claim.")
        } else {
            (
                "unclear",
                0.3,
                "Passages were retrieved, but automated semantic support or contradiction assessment is unavailable; review the cited passages manually.",
            )
        };
        let mut filters = json!({
            "domain_include": domains.iter().filter(|d| !d.starts_with('-')).collect::<Vec<_>>(),
            "domain_exclude": domains.iter().filter_map(|d| d.strip_prefix('-')).collect::<Vec<_>>(),
        });
        if let Some(recency) = recency {
            filters["recency"] = json!(recency);
        }
        let mut artifact = json!({
            "type": "research",
            "timestamp": now,
            "query": claim,
            "sources": sources,
            "passages": passages,
            "claims": [{"claim": claim, "status": status, "supporting_passages": [], "contradicting_passages": [], "rationale": rationale, "confidence": confidence}],
            "filters": filters,
        });
        if let Some(provider) = &provider {
            artifact["provider"] = json!(provider);
        }
        if !summaries.is_empty() {
            artifact["summary"] = json!(summaries.join("\n\n"));
        }
        if !passages.is_empty() {
            let texts: Vec<&str> = passages.iter().filter_map(|p| p["text"].as_str()).collect();
            artifact["content_hash"] = json!(hash(&texts.join("\n")));
        }
        if !errors.is_empty() {
            artifact["errors"] = json!(errors);
        }
        let id = self.store(artifact)?;

        let mut lines = vec![
            format!("# Source check: {claim}"),
            String::new(),
            format!("**Status:** {status} (confidence {confidence:.2})"),
            format!("**Rationale:** {rationale}"),
            String::new(),
        ];
        if !hits.is_empty() {
            lines.push("## Sources".into());
            for (source, hit) in sources.iter().zip(&hits) {
                lines.push(format!("{}. [{}] {}\n   {}", source["rank"], source["quality"].as_str().unwrap_or_default(), hit.title, hit.url));
            }
            lines.push(String::new());
        }
        if !errors.is_empty() {
            let listed: Vec<String> = errors.iter().map(|e| format!("{}: {}", e["query"].as_str().unwrap_or_default(), e["error"].as_str().unwrap_or_default())).collect();
            lines.push(format!("Search errors: {}", listed.join("; ")));
        }
        lines.push(format!("Artifact responseId: {id} (retrievable via get_search_content)."));
        Ok(lines.join("\n"))
    }
}

/// pi's buildPassages for one source: the snippet, then the fetched sentences that share the most terms with it.
fn source_passages(rank: usize, url: &str, snippet: &str, page: Option<&Page>, claim: &str) -> Vec<Value> {
    let mut passages = Vec::new();
    if !snippet.trim().is_empty() {
        passages.push(json!({"passage_id": format!("p-{rank}-0"), "source_url": url, "source_rank": rank, "text": snippet, "content_hash": hash(snippet)}));
    }
    let Some(page) = page.filter(|p| p.error.is_none() && !p.content.is_empty()) else { return passages };
    let hint = if snippet.trim().is_empty() { claim } else { snippet.trim() };
    for (index, (text, start, end)) in relevant_spans(&page.content, hint).into_iter().enumerate() {
        passages.push(json!({
            "passage_id": format!("p-{rank}-{}", index + 1),
            "source_url": url,
            "source_rank": rank,
            "text": text,
            "extraction_span": {"start": start, "end": end},
            "content_hash": hash(&text),
        }));
    }
    passages
}

/// Up to three sentences (text, char start, char end) ranked by how many hint terms they contain.
fn relevant_spans(content: &str, hint: &str) -> Vec<(String, usize, usize)> {
    let mut terms: Vec<&str> = Vec::new();
    let hint = hint.to_lowercase();
    for term in hint.split(|c: char| !c.is_alphanumeric()) {
        if term.chars().count() > 3 && !terms.contains(&term) {
            terms.push(term);
        }
    }
    if terms.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, (String, usize, usize))> = Vec::new();
    for sentence in sentences(content) {
        let lower = sentence.0.to_lowercase();
        let score = terms.iter().filter(|term| lower.contains(*term)).count();
        if score > 0 {
            scored.push((score, sentence));
        }
    }
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().take(3).map(|(_, sentence)| sentence).collect()
}

/// pi's `/[^.!?]+(?:[.!?]+(?=\s|$)|$)/g`, trimmed, 1 to 400 chars, with char offsets.
fn sentences(content: &str) -> Vec<(String, usize, usize)> {
    let chars: Vec<char> = content.chars().collect();
    let is_end = |c: char| ".!?".contains(c);
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if is_end(chars[i]) {
            i += 1;
            continue;
        }
        let body_end = (i..chars.len()).find(|&j| is_end(chars[j])).unwrap_or(chars.len());
        let punct_end = (body_end..chars.len()).find(|&j| !is_end(chars[j])).unwrap_or(chars.len());
        if body_end < chars.len() && punct_end < chars.len() && !chars[punct_end].is_whitespace() {
            // "3.14": no match can start inside this run, the regex resumes at the dot.
            i = body_end;
            continue;
        }
        let raw: String = chars[i..punct_end].iter().collect();
        let text = raw.trim();
        let length = text.chars().count();
        if (1..=400).contains(&length) {
            let start = i + raw.chars().take_while(|c| c.is_whitespace()).count();
            found.push((text.to_string(), start, start + length));
        }
        i = punct_end;
    }
    found
}

/// pi's classifySource: the first quality whose path or host pattern matches.
fn classify(url: &str) -> &'static str {
    let Some(host) = super::host(url) else { return "unknown" };
    let after_scheme = url.split_once("://").map_or("", |(_, rest)| rest);
    let path = after_scheme.split(['?', '#']).next().unwrap_or_default();
    let path = path.find('/').map_or(String::new(), |slash| path[slash..].to_lowercase());
    let slash = |c: Option<char>| c == Some('/');
    let word_end = |c: Option<char>| !c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let news_end = |c: Option<char>| c.is_none() || c == Some('/');
    let path_has = |names: &[&str], next_ok: &dyn Fn(Option<char>) -> bool| {
        names.iter().any(|name| path.match_indices(&format!("/{name}")).any(|(at, m)| next_ok(path[at + m.len()..].chars().next())))
    };
    let host_has = |parts: &[&str]| parts.iter().any(|part| host.contains(part));

    if path_has(&["issues", "pull", "pulls"], &slash) {
        return "repo_issue";
    }
    if path_has(&["doc", "docs", "reference"], &word_end)
        || ["developers.", "docs.", "learn.", "reference."].iter().any(|p| host.starts_with(p))
        || host.ends_with(".github.io")
    {
        return "official_docs";
    }
    if path_has(&["documentation", "doc", "docs"], &slash) {
        return "vendor_docs";
    }
    if path_has(&["news"], &news_end)
        || host_has(&["reuters.com", "bloomberg.com", "techcrunch.com", "theverge.com", "arstechnica.com", "wired.com", "cnet.com", "zdnet.com"])
    {
        return "news";
    }
    if path_has(&["forum", "forums", "threads"], &slash) || host_has(&["stackoverflow.com", "serverfault.com", "superuser.com", "discourse.", "community."]) {
        return "forum";
    }
    if path_has(&["blog", "blogs"], &slash) || host_has(&["medium.com", "substack.com", "dev.to", "hashnode."]) {
        return "blog";
    }
    "unknown"
}

pub fn hash(text: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    let hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_follows_pi_order() {
        assert_eq!(classify("https://github.com/a/b/issues/3"), "repo_issue");
        assert_eq!(classify("https://doc.rust-lang.org/std/docs-v2"), "official_docs");
        assert_eq!(classify("https://rust-lang.github.io/x"), "official_docs");
        assert_eq!(classify("https://vendor.com/documentation/x"), "vendor_docs");
        assert_eq!(classify("https://example.com/news"), "news");
        assert_eq!(classify("https://stackoverflow.com/q/1"), "forum");
        assert_eq!(classify("https://example.com/blog/post"), "blog");
        assert_eq!(classify("https://example.com/newsletter"), "unknown");
        assert_eq!(classify("nonsense"), "unknown");
    }

    #[test]
    fn relevant_spans_rank_sentences_by_shared_terms() {
        let content = "Rust is fast. Version 3.14 shipped today! Rust memory safety matters. Unrelated text";
        let spans = relevant_spans(content, "Rust memory safety");
        let texts: Vec<&str> = spans.iter().map(|(text, ..)| text.as_str()).collect();
        assert_eq!(texts, ["Rust memory safety matters.", "Rust is fast."]);
        assert_eq!((spans[1].1, spans[1].2), (0, 13));
        assert_eq!(sentences(content)[1].0, "14 shipped today!");
        let texts: Vec<String> = relevant_spans("Inne zdanie. Zażółć gęślą jaźń dziś.", "zażółć jaźń").into_iter().map(|(text, ..)| text).collect();
        assert_eq!(texts, ["Zażółć gęślą jaźń dziś."]);
    }

    #[test]
    fn source_check_requires_a_claim() {
        let dir = std::env::temp_dir().join("agent-web-claim");
        let mut web = Web::new(&dir, dir.join("store"));
        assert_eq!(web.source_check(&json!({"claim": " "})).unwrap_err(), "Error: 'claim' is required.");
    }
}
