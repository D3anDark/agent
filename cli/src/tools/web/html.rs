// shortcut: no Readability scoring; the whole body minus page chrome is converted. Port a scorer if menus or ads swamp real pages.
// shortcut: tag scanner, not an HTML5 parser; unclosed block tags simply end at the next block.

const SKIPPED: [&str; 13] = ["script", "style", "nav", "header", "footer", "aside", "noscript", "svg", "template", "head", "iframe", "form", "button"];

/// The page `<title>` and the body as Markdown. `base` resolves relative links.
pub fn to_markdown(html: &str, base: &str) -> (String, String) {
    let mut out = Writer::default();
    let mut title = String::new();
    let mut skip: Option<String> = None;
    let mut in_title = false;
    let mut rest = html;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            text(rest, skip.is_some(), in_title, &mut title, &mut out);
            break;
        };
        text(&rest[..lt], skip.is_some(), in_title, &mut title, &mut out);
        rest = &rest[lt..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            rest = comment.find("-->").map_or("", |end| &comment[end + 3..]);
            continue;
        }
        let Some(gt) = tag_end(rest) else {
            text(rest, skip.is_some(), in_title, &mut title, &mut out);
            break;
        };
        let tag = Tag::parse(&rest[1..gt]);
        rest = &rest[gt + 1..];
        let Some(tag) = tag else { continue };
        if tag.name == "title" {
            in_title = !tag.closing;
            continue;
        }
        if let Some(skipped) = &skip {
            if tag.closing && tag.name == *skipped {
                skip = None;
            }
            continue;
        }
        if SKIPPED.contains(&tag.name.as_str()) && !tag.closing && !tag.self_closing {
            skip = Some(tag.name);
            continue;
        }
        if tag.name == "pre" && !tag.closing {
            let end = rest.to_ascii_lowercase().find("</pre").unwrap_or(rest.len());
            out.code_block(&decode(&strip_tags(&rest[..end])));
            rest = &rest[end..];
            continue;
        }
        out.tag(&tag, base);
    }
    (decode(title.trim()).split_whitespace().collect::<Vec<_>>().join(" "), out.finish())
}

fn text(raw: &str, skipped: bool, in_title: bool, title: &mut String, out: &mut Writer) {
    if in_title {
        title.push_str(raw);
    } else if !skipped {
        out.inline(&decode(raw));
    }
}

/// The `>` closing the tag at the start of `rest`, skipping `>` inside quoted attribute values.
fn tag_end(rest: &str) -> Option<usize> {
    let mut quote = None;
    for (index, ch) in rest.char_indices().skip(1) {
        match (quote, ch) {
            (None, '"' | '\'') => quote = Some(ch),
            (Some(open), _) if ch == open => quote = None,
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

struct Tag {
    name: String,
    closing: bool,
    self_closing: bool,
    href: Option<String>,
}

impl Tag {
    fn parse(inner: &str) -> Option<Tag> {
        let closing = inner.starts_with('/');
        let inner = inner.trim_start_matches('/');
        let name: String = inner.chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
        if name.is_empty() {
            return None;
        }
        Some(Tag { closing, self_closing: inner.ends_with('/'), href: attribute(inner, "href"), name })
    }
}

fn attribute(inner: &str, name: &str) -> Option<String> {
    let lower = inner.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(name) {
        let start = from + found;
        from = start + name.len();
        let before_ok = lower[..start].ends_with(|c: char| c.is_whitespace());
        let after = lower[from..].trim_start();
        if !before_ok || !after.starts_with('=') {
            continue;
        }
        let value = inner[inner.len() - after.len() + 1..].trim_start();
        return Some(match value.chars().next() {
            Some(quote @ ('"' | '\'')) => value[1..].split(quote).next().unwrap_or_default().to_string(),
            _ => value.split(|c: char| c.is_whitespace() || c == '>').next().unwrap_or_default().to_string(),
        });
    }
    None
}

fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut chars = html.chars().peekable();
    while let Some(ch) = chars.next() {
        let opens_tag = ch == '<' && chars.peek().is_some_and(|next| next.is_ascii_alphabetic() || matches!(next, '/' | '!'));
        match ch {
            _ if opens_tag => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

enum List {
    Bullet,
    Ordered(usize),
}

#[derive(Default)]
struct Writer {
    out: String,
    lists: Vec<List>,
    /// Output offset and target of each open `<a>`, wrapped into `[text](href)` on `</a>`.
    links: Vec<(usize, Option<String>)>,
    quotes: Vec<usize>,
    row_cells: usize,
    header_done: bool,
}

impl Writer {
    /// Text outside `<pre>`: whitespace runs collapse to one space, none at a line start.
    fn inline(&mut self, text: &str) {
        if text.starts_with(char::is_whitespace) {
            self.space();
        }
        let mut words = text.split_whitespace();
        if let Some(first) = words.next() {
            self.out.push_str(first);
            for word in words {
                self.out.push(' ');
                self.out.push_str(word);
            }
            if text.ends_with(char::is_whitespace) {
                self.space();
            }
        }
    }

    fn space(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with([' ', '\n']) {
            self.out.push(' ');
        }
    }

    fn block(&mut self) {
        let trimmed = self.out.trim_end_matches(' ').len();
        self.out.truncate(trimmed);
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.out.push_str(if self.out.ends_with('\n') { "\n" } else { "\n\n" });
        }
    }

    fn line(&mut self) {
        let trimmed = self.out.trim_end_matches(' ').len();
        self.out.truncate(trimmed);
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
    }

    fn code_block(&mut self, code: &str) {
        self.block();
        self.out.push_str("```\n");
        self.out.push_str(code.trim_matches('\n'));
        self.out.push_str("\n```\n\n");
    }

    fn tag(&mut self, tag: &Tag, base: &str) {
        let open = !tag.closing;
        match tag.name.as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.block();
                if open {
                    let level = tag.name[1..].parse().unwrap_or(1);
                    self.out.push_str(&"#".repeat(level));
                    self.out.push(' ');
                }
            }
            "p" | "div" | "section" | "article" | "main" | "dl" | "figure" | "table" => {
                self.block();
                if tag.name == "table" {
                    self.header_done = false;
                }
            }
            "br" => self.out.push('\n'),
            "hr" => {
                self.block();
                self.out.push_str("---\n\n");
            }
            "ul" | "ol" => {
                if open {
                    self.line();
                    self.lists.push(if tag.name == "ol" { List::Ordered(0) } else { List::Bullet });
                } else {
                    self.lists.pop();
                    if self.lists.is_empty() {
                        self.block();
                    }
                }
            }
            "li" if open => {
                self.line();
                let depth = self.lists.len().saturating_sub(1);
                self.out.push_str(&"  ".repeat(depth));
                let marker = match self.lists.last_mut() {
                    Some(List::Ordered(n)) => {
                        *n += 1;
                        format!("{n}. ")
                    }
                    _ => "- ".to_string(),
                };
                self.out.push_str(&marker);
            }
            "dt" | "dd" if open => self.line(),
            "a" if open => {
                let href = tag.href.as_deref().filter(|href| !href.starts_with('#') && !href.starts_with("javascript:")).map(|href| resolve(base, &decode(href)));
                self.links.push((self.out.len(), href));
            }
            "a" => {
                if let Some((start, Some(href))) = self.links.pop() {
                    let label = self.out[start..].trim().to_string();
                    self.out.truncate(start);
                    if !label.is_empty() {
                        self.out.push_str(&format!("[{label}]({href})"));
                    }
                }
            }
            "strong" | "b" => self.out.push_str("**"),
            "em" | "i" => self.out.push('*'),
            "code" => self.out.push('`'),
            "blockquote" if open => {
                self.block();
                self.quotes.push(self.out.len());
            }
            "blockquote" => {
                if let Some(start) = self.quotes.pop() {
                    let quoted: Vec<String> = self.out[start..].trim().lines().map(|line| format!("> {line}").trim_end().to_string()).collect();
                    self.out.truncate(start);
                    self.out.push_str(&quoted.join("\n"));
                    self.block();
                }
            }
            "tr" if open => {
                self.line();
                self.out.push('|');
                self.row_cells = 0;
            }
            "tr" => {
                if !self.header_done && self.row_cells > 0 {
                    self.out.push('\n');
                    self.out.push('|');
                    self.out.push_str(&" --- |".repeat(self.row_cells));
                    self.header_done = true;
                }
                self.out.push('\n');
            }
            "td" | "th" if open => self.out.push(' '),
            "td" | "th" => {
                let trimmed = self.out.trim_end().len();
                self.out.truncate(trimmed);
                self.out.push_str(" |");
                self.row_cells += 1;
            }
            _ => {}
        }
    }

    fn finish(self) -> String {
        let mut lines: Vec<&str> = Vec::new();
        for line in self.out.lines().map(str::trim_end) {
            if line.is_empty() && lines.last().is_none_or(|last| last.is_empty()) {
                continue;
            }
            lines.push(line);
        }
        lines.join("\n").trim().to_string()
    }
}

/// `href` as an absolute URL against the page URL `base`.
pub fn resolve(base: &str, href: &str) -> String {
    if href.contains("://") || href.starts_with("mailto:") {
        return href.to_string();
    }
    let scheme_end = base.find("://").map_or(0, |i| i + 3);
    if let Some(rest) = href.strip_prefix("//") {
        return format!("{}{rest}", &base[..scheme_end]);
    }
    let origin_end = base[scheme_end..].find('/').map_or(base.len(), |i| scheme_end + i);
    if href.starts_with('/') {
        return format!("{}{href}", &base[..origin_end]);
    }
    let path = base[origin_end..].split(['?', '#']).next().unwrap_or_default();
    let dir = path.rfind('/').map_or("/", |i| &path[..=i]);
    format!("{}{dir}{href}", &base[..origin_end])
}

pub fn decode(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let entity = rest[1..].find(';').filter(|end| *end <= 10).map(|end| &rest[1..=end]);
        match entity.and_then(entity_char) {
            Some(ch) => {
                out.push(ch);
                rest = &rest[entity.map_or(0, str::len) + 2..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity_char(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "copy" => '©',
        "reg" => '®',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_keeps_headings_paragraphs_and_links_and_drops_chrome() {
        let html = r#"<html><head><title>Rust &amp; Web</title><style>p{}</style></head><body>
            <nav><a href="/">Home</a></nav><header>Site</header>
            <h1>Intro</h1><p>Read the <a href="/docs/guide.html">guide</a> and <strong>enjoy</strong>.</p>
            <script>alert("x > y")</script><aside>Ads</aside><p>Second&nbsp;para &lt;ok&gt;</p>
            <footer>Copyright</footer></body></html>"#;
        let (title, markdown) = to_markdown(html, "https://example.com/blog/post");
        assert_eq!(title, "Rust & Web");
        assert_eq!(markdown, "# Intro\n\nRead the [guide](https://example.com/docs/guide.html) and **enjoy**.\n\nSecond para <ok>");
    }

    #[test]
    fn lists_and_code_blocks() {
        let html = "<ul><li>one</li><li>two<ol><li>a</li><li>b</li></ol></li></ul><p>Run <code>cargo test</code>:</p><pre><code>fn main() {\n    if a < b {}\n}</code></pre>";
        let (_, markdown) = to_markdown(html, "https://example.com/");
        assert_eq!(markdown, "- one\n- two\n  1. a\n  2. b\n\nRun `cargo test`:\n\n```\nfn main() {\n    if a < b {}\n}\n```");
    }

    #[test]
    fn tables_become_pipe_tables() {
        let html = "<table><tr><th>Name</th><th>Size</th></tr><tr><td>a.rs</td><td>3 KB</td></tr></table><p>After</p>";
        let (_, markdown) = to_markdown(html, "https://example.com/");
        assert_eq!(markdown, "| Name | Size |\n| --- | --- |\n| a.rs | 3 KB |\n\nAfter");
    }
}
