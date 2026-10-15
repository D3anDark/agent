use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use super::render::{Line, PLAIN, Span, Style, span, wrap};

const BULLET: Style = Style::fg(245);
const INLINE_CODE: Style = Style::fg(245);
const CODE: Style = Style::fg(252);
const CODE_BORDER: Style = Style::rgb(0x555555);
const QUOTE: Style = Style::fg(245);
const LINK: Style = Style::fg(245).underline();

/// Renders assistant markdown to `width` columns with pi's indent of one space.
pub fn render(text: &str, width: usize) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut styles: Vec<Style> = vec![PLAIN];
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut item_prefix: Option<Span> = None;
    let mut quote_depth = 0;
    let mut in_code = false;
    let base = |depth: usize, quote: usize| -> Line {
        let mut indent = vec![span(" ", PLAIN)];
        for _ in 0..quote {
            indent.push(span("│ ", QUOTE));
        }
        if depth > 1 {
            indent.push(span("  ".repeat(depth - 1), PLAIN));
        }
        indent
    };
    let flush = |out: &mut Vec<Line>, spans: &mut Vec<Span>, prefix: &mut Option<Span>, indent: Line| {
        if spans.is_empty() && prefix.is_none() {
            return;
        }
        let mut first_indent = indent.clone();
        let mut rest_indent = indent;
        if let Some(prefix) = prefix.take() {
            rest_indent.push(span(" ".repeat(super::render::width(&prefix.text)), PLAIN));
            first_indent.push(prefix);
        }
        let mut lines = wrap(spans, width, &rest_indent);
        if let Some(first) = lines.first_mut() {
            first.splice(0..rest_indent.len(), first_indent);
        }
        out.extend(lines);
        spans.clear();
    };
    let gap = |out: &mut Vec<Line>| {
        if out.last().is_some_and(|l| !l.is_empty()) {
            out.push(Vec::new());
        }
    };
    for event in Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS) {
        let style = *styles.last().unwrap();
        match event {
            Event::Start(Tag::Paragraph) => {
                if lists.is_empty() {
                    gap(&mut out);
                }
            }
            Event::End(TagEnd::Paragraph) => flush(&mut out, &mut spans, &mut item_prefix, base(lists.len(), quote_depth)),
            Event::Start(Tag::Heading { level, .. }) => {
                gap(&mut out);
                let marker = match level {
                    HeadingLevel::H1 => "# ",
                    HeadingLevel::H2 => "## ",
                    _ => "### ",
                };
                spans.push(span(marker, Style::fg(245).bold()));
                styles.push(PLAIN.bold());
            }
            Event::End(TagEnd::Heading(_)) => {
                styles.pop();
                flush(&mut out, &mut spans, &mut item_prefix, base(0, quote_depth));
            }
            Event::Start(Tag::BlockQuote(_)) => {
                gap(&mut out);
                quote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => quote_depth -= 1,
            Event::Start(Tag::List(start)) => {
                flush(&mut out, &mut spans, &mut item_prefix, base(lists.len(), quote_depth));
                if lists.is_empty() {
                    gap(&mut out);
                }
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                flush(&mut out, &mut spans, &mut item_prefix, base(lists.len(), quote_depth));
                let marker = match lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}. ", *n - 1)
                    }
                    _ => "- ".to_string(),
                };
                item_prefix = Some(span(marker, BULLET));
            }
            Event::End(TagEnd::Item) => flush(&mut out, &mut spans, &mut item_prefix, base(lists.len(), quote_depth)),
            Event::TaskListMarker(done) => spans.push(span(if done { "[x] " } else { "[ ] " }, BULLET)),
            Event::Start(Tag::CodeBlock(_)) => {
                flush(&mut out, &mut spans, &mut item_prefix, base(lists.len(), quote_depth));
                gap(&mut out);
                in_code = true;
            }
            Event::End(TagEnd::CodeBlock) => in_code = false,
            Event::Text(text) if in_code => {
                let indent = base(lists.len(), quote_depth);
                let rule_width = width.saturating_sub(super::render::line_width(&indent)).max(1);
                out.push([indent.clone(), vec![span("─".repeat(rule_width), CODE_BORDER)]].concat());
                for line in text.trim_end_matches('\n').split('\n') {
                    let line = line.replace('\t', "    ");
                    out.extend(wrap(&[span(line, CODE)], width, &indent));
                }
                out.push([indent, vec![span("─".repeat(rule_width), CODE_BORDER)]].concat());
            }
            Event::Text(text) => spans.push(span(text.to_string(), style)),
            Event::Code(text) => spans.push(span(text.to_string(), INLINE_CODE)),
            Event::Start(Tag::Strong) => styles.push(Style { bold: true, ..style }),
            Event::Start(Tag::Emphasis) => styles.push(Style { italic: true, ..style }),
            Event::Start(Tag::Strikethrough) => styles.push(style),
            Event::Start(Tag::Link { .. }) => styles.push(LINK),
            Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link) => {
                styles.pop();
            }
            Event::SoftBreak => spans.push(span(" ", style)),
            Event::HardBreak => spans.push(span("\n", style)),
            Event::Rule => {
                gap(&mut out);
                out.push(vec![span(" ", PLAIN), span("─".repeat(width.saturating_sub(1)), CODE_BORDER)]);
            }
            Event::Html(html) | Event::InlineHtml(html) => spans.push(span(html.to_string(), style)),
            Event::End(TagEnd::TableCell) => spans.push(span("  ", style)),
            Event::End(TagEnd::TableRow | TagEnd::TableHead) => flush(&mut out, &mut spans, &mut item_prefix, base(lists.len(), quote_depth)),
            _ => {}
        }
    }
    flush(&mut out, &mut spans, &mut item_prefix, base(lists.len(), quote_depth));
    while out.first().is_some_and(|l| l.is_empty()) {
        out.remove(0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.iter().map(|s| s.text.as_str()).collect::<String>().trim_end().to_string()).collect()
    }

    #[test]
    fn list_and_code_block_match_pi_layout() {
        let lines = render("- one `a.txt`\n- two\n\n```\nhello\n```", 10);
        assert_eq!(text(&lines), [" - one", "   a.txt", " - two", "", " ─────────", " hello", " ─────────"]);
        assert_eq!(lines[1][2].style, INLINE_CODE);
    }

    #[test]
    fn paragraphs_are_separated_by_one_blank_line() {
        assert_eq!(text(&render("a\n\nb", 20)), [" a", "", " b"]);
    }
}
