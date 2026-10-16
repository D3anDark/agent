//! `findText` for get_search_content, ported from pi-web-access `content-find.ts`.

const CONTEXT_BYTES: usize = 400;
const MAX_OUTPUT_CHARS: usize = 20_000;
/// Room kept for an excerpt's number, query labels and ellipses.
const EXCERPT_OVERHEAD: usize = 1_000;

#[derive(Clone, Copy)]
pub enum Mode {
    Exact,
    CaseInsensitive,
    Fuzzy,
}

impl Mode {
    pub fn parse(name: &str) -> Option<Mode> {
        match name {
            "exact" => Some(Mode::Exact),
            "case-insensitive" => Some(Mode::CaseInsensitive),
            "fuzzy" => Some(Mode::Fuzzy),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Mode::Exact => "exact",
            Mode::CaseInsensitive => "case-insensitive",
            Mode::Fuzzy => "fuzzy",
        }
    }
}

/// Byte range of one match in the searched text.
#[derive(Clone, Copy)]
struct Match {
    query: usize,
    start: usize,
    end: usize,
}

/// Excerpts around every match of `queries`, merged where they overlap.
pub fn find(text: &str, queries: &[String], mode: Mode) -> String {
    let mut unique: Vec<&str> = Vec::new();
    for query in queries.iter().map(|q| q.trim()).filter(|q| !q.is_empty()) {
        if !unique.contains(&query) {
            unique.push(query);
        }
    }
    let mut matches = Vec::new();
    for (index, query) in unique.iter().enumerate() {
        let found = match mode {
            Mode::Fuzzy => fuzzy_matches(text, query),
            Mode::Exact => literal_matches(text, query, false),
            Mode::CaseInsensitive => literal_matches(text, query, true),
        };
        matches.extend(found.into_iter().map(|(start, end)| Match { query: index, start, end }));
    }

    let heading = if matches.is_empty() { format!("Text matches ({}): no matches", mode.name()) } else { format!("Text matches ({})", mode.name()) };
    let mut sections = vec![heading];
    let mut length = sections[0].len();
    let mut returned = 0;
    for (index, (start, end)) in context_ranges(text, &matches).into_iter().enumerate() {
        let room = MAX_OUTPUT_CHARS.saturating_sub(length + 2);
        // shortcut: the excerpt that crosses the budget is cut short and later ones dropped; pi picks one witness per query instead.
        let end = end.min(text.floor_char_boundary(start + room.saturating_sub(EXCERPT_OVERHEAD)));
        let inside: Vec<&Match> = matches.iter().filter(|m| m.start >= start && m.end <= end).collect();
        if inside.is_empty() {
            break;
        }
        let section = format!("{}. {}\n{}", index + 1, counts(&unique, &inside), snippet(text, start, end));
        if section.len() > room {
            break;
        }
        length += 2 + section.len();
        returned += inside.len();
        sections.push(section);
    }
    let missing: Vec<String> = unique.iter().enumerate().filter(|(i, _)| !matches.iter().any(|m| m.query == *i)).map(|(_, q)| format!("\"{q}\"")).collect();
    if !missing.is_empty() {
        sections.push(format!("No matches: {}", missing.join(", ")));
    }
    if returned < matches.len() {
        sections.push(format!("Showing {returned} of {} matches.", matches.len()));
    }
    sections.join("\n\n")
}

/// `"query" ×count` labels, ordered by each query's first match in the range.
fn counts(queries: &[&str], inside: &[&Match]) -> String {
    let mut labels: Vec<(usize, usize, usize)> = Vec::new();
    for m in inside {
        match labels.iter_mut().find(|(query, _, _)| *query == m.query) {
            Some(label) => {
                label.1 += 1;
                label.2 = label.2.min(m.start);
            }
            None => labels.push((m.query, 1, m.start)),
        }
    }
    labels.sort_by_key(|&(query, _, first)| (first, query));
    let labels: Vec<String> = labels.iter().map(|&(query, count, _)| format!("\"{}\" ×{count}", queries[query])).collect();
    labels.join(", ")
}

fn snippet(text: &str, start: usize, end: usize) -> String {
    let body = text[start..end].split_whitespace().collect::<Vec<_>>().join(" ");
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if end < text.len() { "…" } else { "" };
    format!("{prefix}{body}{suffix}")
}

fn context_ranges(text: &str, matches: &[Match]) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = matches
        .iter()
        .map(|m| (text.floor_char_boundary(m.start.saturating_sub(CONTEXT_BYTES)), text.ceil_char_boundary(m.end + CONTEXT_BYTES)))
        .collect();
    ranges.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

fn literal_matches(text: &str, query: &str, case_insensitive: bool) -> Vec<(usize, usize)> {
    if !case_insensitive {
        return text.match_indices(query).map(|(start, _)| (start, start + query.len())).collect();
    }
    // Lowercasing can change byte lengths, so each lowered byte remembers where its char started in `text`.
    let mut lowered = String::new();
    let mut origin = Vec::new();
    for (at, c) in text.char_indices() {
        for lower in c.to_lowercase() {
            lowered.push(lower);
            origin.resize(lowered.len(), at);
        }
    }
    origin.push(text.len());
    let needle = query.to_lowercase();
    lowered.match_indices(&needle).map(|(start, _)| (origin[start], origin[start + needle.len()])).collect()
}

/// Paragraphs where enough query words appear within a small edit distance; the match is the first such word.
fn fuzzy_matches(text: &str, query: &str) -> Vec<(usize, usize)> {
    let query_words: Vec<Vec<char>> = words(&normalize(query)).into_iter().map(|(_, word)| word.chars().collect()).collect();
    if query_words.is_empty() {
        return Vec::new();
    }
    let required = if query_words.len() == 1 { 1 } else { (query_words.len() * 3).div_ceil(5) };
    let mut matches = Vec::new();
    for (offset, paragraph) in paragraphs(text) {
        let tokens: Vec<(usize, Vec<char>)> = words(paragraph).into_iter().map(|(at, word)| (at, normalize(word).chars().collect())).collect();
        let matched: Vec<&Vec<char>> = query_words.iter().filter(|q| tokens.iter().any(|(_, t)| close(q, t))).collect();
        if matched.len() < required {
            continue;
        }
        let Some((at, token)) = tokens.iter().find(|(_, t)| matched.iter().any(|q| close(q, t))) else {
            continue;
        };
        let start = offset + at;
        let word_len = words(&paragraph[*at..]).first().map_or(token.len(), |(_, w)| w.len());
        matches.push((start, start + word_len));
    }
    matches
}

// shortcut: lowercases only; pi also strips diacritics via NFD, which needs Unicode tables we do not ship.
fn normalize(value: &str) -> String {
    value.to_lowercase()
}

fn close(query: &[char], token: &[char]) -> bool {
    let maximum = match query.len() {
        9.. => 2,
        5..=8 => 1,
        _ => 0,
    };
    edit_distance_within(query, token, maximum)
}

fn edit_distance_within(left: &[char], right: &[char], maximum: usize) -> bool {
    if left.len().abs_diff(right.len()) > maximum {
        return false;
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for i in 1..=left.len() {
        let mut current = vec![i];
        for j in 1..=right.len() {
            let substitution = previous[j - 1] + usize::from(left[i - 1] != right[j - 1]);
            current.push((previous[j] + 1).min(current[j - 1] + 1).min(substitution));
        }
        if current.iter().min().is_some_and(|&m| m > maximum) {
            return false;
        }
        previous = current;
    }
    previous[right.len()] <= maximum
}

/// Runs of letters and digits with their byte offsets.
fn words(text: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    let mut start = None;
    for (at, c) in text.char_indices() {
        match (c.is_alphanumeric(), start) {
            (true, None) => start = Some(at),
            (false, Some(s)) => {
                found.push((s, &text[s..at]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        found.push((s, &text[s..]));
    }
    found
}

/// Blocks of consecutive non-empty lines with their byte offsets; blank-only blocks are skipped.
fn paragraphs(text: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    let mut start: Option<usize> = None;
    let mut at = 0;
    for line in text.split('\n') {
        match (line.is_empty(), start) {
            (false, None) => start = Some(at),
            (true, Some(s)) => {
                found.push((s, &text[s..at - 1]));
                start = None;
            }
            _ => {}
        }
        at += line.len() + 1;
    }
    if let Some(s) = start {
        found.push((s, &text[s..]));
    }
    found.retain(|(_, p)| !p.trim().is_empty());
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queries(list: &[&str]) -> Vec<String> {
        list.iter().map(|q| q.to_string()).collect()
    }

    #[test]
    fn case_insensitive_merges_nearby_matches_and_reports_missing_queries() {
        let text = "Intro.\n\nThe Tokio runtime schedules tasks. tokio is async.\n\nEnd.";
        let out = find(text, &queries(&["TOKIO", "absent"]), Mode::CaseInsensitive);
        assert_eq!(out, "Text matches (case-insensitive)\n\n1. \"TOKIO\" ×2\nIntro. The Tokio runtime schedules tasks. tokio is async. End.\n\nNo matches: \"absent\"");
        assert!(find(text, &queries(&["TOKIO"]), Mode::Exact).starts_with("Text matches (exact): no matches"));
    }

    #[test]
    fn fuzzy_finds_misspelled_words_in_one_paragraph() {
        let text = "First paragraph about cats.\n\nThe asynchronous runtime handles scheduling.";
        let out = find(text, &queries(&["asynchronus scheduling"]), Mode::Fuzzy);
        assert!(out.starts_with("Text matches (fuzzy)\n\n1. \"asynchronus scheduling\" ×1\n"), "{out}");
        assert!(out.contains("The asynchronous runtime"), "{out}");
    }

    #[test]
    fn excerpts_stay_on_char_boundaries_and_within_budget() {
        let text = "żółw ".repeat(10_000);
        let out = find(&text, &queries(&["ŻÓŁW"]), Mode::CaseInsensitive);
        assert!(out.chars().count() <= MAX_OUTPUT_CHARS);
        assert!(out.ends_with("Showing 10000 of 10000 matches.") || out.contains("Showing "), "{}", &out[out.len() - 80..]);
    }
}
