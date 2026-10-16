use super::OK;
use super::render::{Line, PLAIN, Style, span, truncate, width};

const BRIGHT: Style = Style::fg(255).bold();
const MUTED: Style = Style::fg(245);

pub struct Row {
    pub left: String,
    pub right: Line,
}

/// fx's bottom panel: a blank line, `Title  [Tab]  Tab`, `> search` when it filters, rows with the selected one bright, key hints.
pub struct Panel<'a> {
    pub title: String,
    pub tabs: &'a [&'a str],
    pub tab: usize,
    /// The filter field, `Editor::field` output; `None` for panels that do not filter.
    pub search: Option<String>,
    pub rows: &'a [Row],
    pub selected: usize,
    pub empty: &'a str,
    pub hint: &'a str,
}

impl Panel<'_> {
    pub fn render(&self, columns: usize, max_rows: usize) -> Vec<Line> {
        let mut header = vec![span(self.title.clone(), BRIGHT)];
        for (index, tab) in self.tabs.iter().enumerate() {
            header.push(span("  ", PLAIN));
            header.push(if index == self.tab { span(format!("[{tab}]"), BRIGHT) } else { span(*tab, MUTED) });
        }
        let first = (self.selected + 1).saturating_sub(max_rows);
        let mut lines = vec![Vec::new(), header];
        if let Some(search) = &self.search {
            lines.push(vec![span(format!("> {search}"), BRIGHT)]);
        }
        if self.rows.is_empty() {
            lines.push(vec![span(format!("  {}", self.empty), MUTED)]);
        }
        let left_width = self.rows.iter().map(|r| width(&r.left)).max().unwrap_or(0);
        for (index, row) in self.rows.iter().enumerate().skip(first).take(max_rows) {
            let (marker, style) = if index == self.selected { ("→ ", BRIGHT) } else { ("  ", MUTED) };
            let pad = " ".repeat(left_width - width(&row.left));
            let mut line = vec![span(marker, OK), span(format!("{}{pad}", row.left), style), span("    ", PLAIN)];
            line.extend(row.right.iter().cloned());
            lines.push(truncate(&line, columns));
        }
        if self.rows.len() > max_rows {
            lines.push(vec![span(format!("  {}/{}", self.selected + 1, self.rows.len()), MUTED)]);
        }
        lines.push(vec![span(self.hint, MUTED)]);
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn rows_align_and_the_selected_one_is_marked_and_bright() {
        let rows = [Row { left: "a".into(), right: vec![span("x", MUTED)] }, Row { left: "abc".into(), right: vec![span("y", MUTED)] }];
        let panel = Panel { title: "Sessions 2".into(), tabs: &["One", "Two"], tab: 0, search: None, rows: &rows, selected: 1, empty: "", hint: "esc close" };
        let lines = panel.render(20, 5);
        assert_eq!(text(&lines[1]), "Sessions 2  [One]  Two");
        assert_eq!(text(&lines[2]), "  a      x");
        assert_eq!(text(&lines[3]), "→ abc    y");
        assert_eq!(lines[3][0].style, OK);
        assert_eq!(lines[3][1].style, BRIGHT);
        assert_eq!(text(&lines[4]), "esc close");
    }

    #[test]
    fn a_list_that_scrolls_shows_the_selected_position_under_it() {
        let rows: Vec<Row> = (0..5).map(|i| Row { left: i.to_string(), right: Vec::new() }).collect();
        let panel = |selected| Panel { title: "Settings".into(), tabs: &[], tab: 0, search: None, rows: &rows, selected, empty: "", hint: "" };
        let lines: Vec<String> = panel(3).render(40, 2).iter().map(text).collect();
        assert_eq!(lines[..5], ["", "Settings", "  2    ", "→ 3    ", "  4/5"]);
        assert_eq!(panel(3).render(40, 5).len(), 8);
    }

    #[test]
    fn search_sits_on_its_own_line_under_the_tabs() {
        let rows = [Row { left: "a".into(), right: Vec::new() }];
        let panel = Panel { title: "History".into(), tabs: &["All", "Here"], tab: 0, search: Some("fo".into()), rows: &rows, selected: 0, empty: "", hint: "" };
        let lines: Vec<String> = panel.render(40, 5).iter().map(text).collect();
        assert_eq!(lines[1..4], ["History  [All]  Here", "> fo", "→ a    "]);
    }
}
