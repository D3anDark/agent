use super::render::{CURSOR, Line, Style, span};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

const PROMPT: Style = Style::rgb(0x79c0ff).bold();
const SHELL_PROMPT: Style = Style::rgb(0xf778ba).bold();

/// Multi-line prompt editor. `col` counts chars, not bytes.
#[derive(Default)]
pub struct Editor {
    lines: Vec<String>,
    row: usize,
    col: usize,
    history: Vec<String>,
    history_pos: Option<usize>,
    undo: Vec<(Vec<String>, usize, usize)>,
    /// Cursor after the last typed character, so a run of typing undoes as one word.
    typing_at: Option<(usize, usize)>,
}

/// Where a cursor position `(line, col)` lands on screen.
#[derive(Clone, Copy)]
struct Spot {
    line: usize,
    col: usize,
    x: usize,
    row: usize,
}

impl Editor {
    pub fn new() -> Editor {
        Editor { lines: vec![String::new()], ..Default::default() }
    }

    /// An empty editor whose Up arrow starts from `history`, oldest first.
    pub fn with_history(history: Vec<String>) -> Editor {
        Editor { history, ..Editor::new() }
    }

    pub fn from_text(text: &str) -> Editor {
        let mut editor = Editor::new();
        editor.set(text);
        editor
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn before_cursor(&self) -> &str {
        &self.lines[self.row][..self.byte_at(self.col)]
    }

    pub fn set(&mut self, text: &str) {
        self.checkpoint();
        self.lines = text.split('\n').map(String::from).collect();
        self.row = self.lines.len() - 1;
        self.col = self.lines[self.row].chars().count();
    }

    /// Returns the text and clears the editor; non-empty text goes to history.
    pub fn take(&mut self) -> String {
        let text = self.text();
        if !text.trim().is_empty() && self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        self.history_pos = None;
        self.set("");
        self.undo.clear();
        text
    }

    pub fn insert(&mut self, text: &str) {
        let typed = text.chars().count() == 1 && !text.chars().all(char::is_whitespace);
        if !typed || self.typing_at != Some((self.row, self.col)) {
            self.checkpoint();
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ");
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                self.break_line();
            }
            let at = self.byte_at(self.col);
            self.lines[self.row].insert_str(at, part);
            self.col += part.chars().count();
        }
        self.typing_at = typed.then_some((self.row, self.col));
    }

    pub fn newline(&mut self) {
        self.checkpoint();
        self.break_line();
    }

    /// Restores the text from before the last edit; a typed word counts as one edit.
    pub fn undo(&mut self) {
        while let Some((lines, row, col)) = self.undo.pop() {
            if lines != self.lines {
                (self.lines, self.row, self.col) = (lines, row, col);
                break;
            }
        }
        self.typing_at = None;
    }

    // Edits that change nothing (backspace at the start) push a duplicate, which undo skips.
    fn checkpoint(&mut self) {
        self.undo.push((self.lines.clone(), self.row, self.col));
        // Each entry is a full copy, so a long paste followed by many edits would grow without end.
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.typing_at = None;
    }

    fn break_line(&mut self) {
        let at = self.byte_at(self.col);
        let rest = self.lines[self.row].split_off(at);
        self.row += 1;
        self.lines.insert(self.row, rest);
        self.col = 0;
    }

    pub fn backspace(&mut self) {
        self.checkpoint();
        if self.col > 0 {
            self.col -= 1;
            let at = self.byte_at(self.col);
            self.lines[self.row].remove(at);
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.lines[self.row].chars().count();
            self.lines[self.row].push_str(&line);
        }
    }

    pub fn delete(&mut self) {
        self.checkpoint();
        if self.col < self.lines[self.row].chars().count() {
            let at = self.byte_at(self.col);
            self.lines[self.row].remove(at);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    pub fn left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].chars().count();
        }
    }

    pub fn right(&mut self) {
        if self.col < self.lines[self.row].chars().count() {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    /// Moves up a screen row, or to the previous history entry from the first row.
    pub fn up(&mut self, width: usize) {
        let (_, spots) = self.wrap(width);
        let here = self.cursor_spot(&spots);
        if here.row > 0 && self.move_to_row(&spots, here.row - 1, here.x) {
            return;
        }
        let pos = match self.history_pos {
            None if self.history.is_empty() || !self.is_empty() => return,
            None => self.history.len() - 1,
            Some(0) => return,
            Some(pos) => pos - 1,
        };
        self.history_pos = Some(pos);
        let entry = self.history[pos].clone();
        self.set(&entry);
    }

    pub fn down(&mut self, width: usize) {
        let (_, spots) = self.wrap(width);
        let here = self.cursor_spot(&spots);
        if self.move_to_row(&spots, here.row + 1, here.x) {
            return;
        }
        let Some(pos) = self.history_pos else { return };
        if pos + 1 < self.history.len() {
            self.history_pos = Some(pos + 1);
            let entry = self.history[pos + 1].clone();
            self.set(&entry);
        } else {
            self.history_pos = None;
            self.set("");
        }
    }

    pub fn home(&mut self) {
        self.col = 0;
    }

    pub fn end(&mut self) {
        self.col = self.lines[self.row].chars().count();
    }

    fn word_left(&mut self) {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        while self.col > 0 && chars[self.col - 1].is_whitespace() {
            self.col -= 1;
        }
        while self.col > 0 && !chars[self.col - 1].is_whitespace() {
            self.col -= 1;
        }
    }

    fn word_right(&mut self) {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        while self.col < chars.len() && chars[self.col].is_whitespace() {
            self.col += 1;
        }
        while self.col < chars.len() && !chars[self.col].is_whitespace() {
            self.col += 1;
        }
    }

    fn delete_word_back(&mut self) {
        self.checkpoint();
        let end = self.col;
        self.word_left();
        let (from, to) = (self.byte_at(self.col), self.byte_at(end));
        self.lines[self.row].replace_range(from..to, "");
    }

    /// Deletes to the line start; at the start it joins the line to the previous one, like Cmd+Backspace on macOS.
    fn kill_to_start(&mut self) {
        self.checkpoint();
        if self.col == 0 {
            self.backspace();
            return;
        }
        let at = self.byte_at(self.col);
        self.lines[self.row].replace_range(..at, "");
        self.col = 0;
    }

    /// The line-editing keys every text input shares; false when the key is not one of them.
    pub fn edit_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let cmd = key.modifiers.contains(KeyModifiers::SUPER);
        match key.code {
            KeyCode::Backspace if cmd => self.kill_to_start(),
            KeyCode::Backspace if alt || ctrl => self.delete_word_back(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left if alt || ctrl => self.word_left(),
            KeyCode::Right if alt || ctrl => self.word_right(),
            KeyCode::Left if cmd => self.home(),
            KeyCode::Right if cmd => self.end(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Home => self.home(),
            KeyCode::End => self.end(),
            KeyCode::Char('z') if ctrl || cmd => self.undo(),
            KeyCode::Char('a') if ctrl => self.home(),
            KeyCode::Char('e') if ctrl => self.end(),
            KeyCode::Char('w') if ctrl => self.delete_word_back(),
            // VTE and xterm send Ctrl+Backspace as ^H.
            KeyCode::Char('h') if ctrl => self.delete_word_back(),
            KeyCode::Char('u') if ctrl => self.kill_to_start(),
            KeyCode::Char('k') if ctrl => self.kill_to_end(),
            KeyCode::Char('b') if alt => self.word_left(),
            KeyCode::Char('f') if alt => self.word_right(),
            KeyCode::Char(ch) if !ctrl && !cmd => self.insert(&ch.to_string()),
            _ => return false,
        }
        true
    }

    /// One-line view with a `CURSOR` marker for the terminal cursor, `mask` hides each char (API keys).
    pub fn field(&self, mask: Option<char>) -> String {
        let show = |text: &str| match mask {
            Some(mask) => mask.to_string().repeat(text.chars().count()),
            None => text.to_string(),
        };
        let line = &self.lines[self.row];
        let at = self.byte_at(self.col);
        format!("{}{CURSOR}{}", show(&line[..at]), show(&line[at..]))
    }

    fn kill_to_end(&mut self) {
        self.checkpoint();
        let at = self.byte_at(self.col);
        self.lines[self.row].truncate(at);
    }

    fn byte_at(&self, col: usize) -> usize {
        self.lines[self.row].char_indices().nth(col).map_or(self.lines[self.row].len(), |(i, _)| i)
    }

    /// Rows for `width` columns behind a "❯ " prompt, plus the cursor as (x, row index).
    /// A leading `!` (shell command) shows as a "$ " prompt instead.
    pub fn render(&self, width: usize) -> (Vec<Line>, (usize, usize)) {
        let (rows, spots) = self.wrap(width);
        let cursor = self.cursor_spot(&spots);
        let (first, style) = if self.lines[0].starts_with('!') { ("$ ", SHELL_PROMPT) } else { ("❯ ", PROMPT) };
        let lines = rows.into_iter().enumerate().map(|(index, row)| vec![span(if index == 0 { first } else { "  " }, style), span(row, super::render::PLAIN)]).collect();
        (lines, (cursor.x, cursor.row))
    }

    /// Wraps the text to `width` and returns the rows plus the screen spot of every cursor position.
    fn wrap(&self, width: usize) -> (Vec<String>, Vec<Spot>) {
        let shell = self.lines[0].starts_with('!');
        let room = width.saturating_sub(2).max(1);
        let mut rows = Vec::new();
        let mut spots: Vec<Spot> = Vec::new();
        for (line_index, line) in self.lines.iter().enumerate() {
            let mut current = String::new();
            let mut used = 0;
            // Byte length, spot count and width of `current` right after its last space.
            let mut after_space: Option<(usize, usize, usize)> = None;
            for (col, ch) in line.chars().enumerate() {
                let w = ch.width().unwrap_or(0);
                if used + w > room {
                    match after_space.take() {
                        Some((bytes, spot_count, width)) if ch != ' ' => {
                            let word = current.split_off(bytes);
                            rows.push(std::mem::replace(&mut current, word));
                            used -= width;
                            for spot in &mut spots[spot_count..] {
                                spot.row += 1;
                                spot.x -= width;
                            }
                        }
                        _ => {
                            rows.push(std::mem::take(&mut current));
                            used = 0;
                        }
                    }
                }
                spots.push(Spot { line: line_index, col, x: 2 + used, row: rows.len() });
                // The "$ " prompt stands in for the leading `!`.
                if shell && line_index == 0 && col == 0 {
                    continue;
                }
                current.push(ch);
                used += w;
                if ch == ' ' {
                    after_space = Some((current.len(), spots.len(), used));
                }
            }
            if line_index == self.row && self.col >= line.chars().count() && used >= room {
                rows.push(std::mem::take(&mut current));
                used = 0;
            }
            spots.push(Spot { line: line_index, col: line.chars().count(), x: 2 + used, row: rows.len() });
            rows.push(current);
        }
        (rows, spots)
    }

    fn cursor_spot(&self, spots: &[Spot]) -> Spot {
        *spots.iter().find(|spot| spot.line == self.row && spot.col == self.col).expect("the cursor is always inside the text")
    }

    /// Moves to the closest column on the screen row `row`; returns false when that row does not exist.
    fn move_to_row(&mut self, spots: &[Spot], row: usize, x: usize) -> bool {
        let Some(target) = spots.iter().filter(|spot| spot.row == row).min_by_key(|spot| spot.x.abs_diff(x)) else { return false };
        self.row = target.line;
        self.col = target.col;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_across_lines() {
        let mut editor = Editor::new();
        editor.insert("ab\ncd");
        editor.home();
        editor.backspace();
        assert_eq!(editor.text(), "abcd");
        editor.insert("ż");
        editor.word_left();
        editor.kill_to_end();
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn undo_steps_back_a_typed_word_or_an_edit_at_a_time() {
        let mut editor = Editor::new();
        for ch in "ab cd".chars() {
            editor.insert(&ch.to_string());
        }
        editor.backspace();
        editor.undo();
        assert_eq!(editor.text(), "ab cd");
        editor.undo();
        assert_eq!(editor.text(), "ab ");
        editor.undo();
        assert_eq!(editor.text(), "ab");
        editor.undo();
        editor.undo();
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn kill_to_start_clears_the_line_then_removes_it() {
        let mut editor = Editor::new();
        editor.insert("ab\ncd");
        editor.kill_to_start();
        assert_eq!(editor.text(), "ab\n");
        editor.kill_to_start();
        assert_eq!(editor.text(), "ab");
        assert_eq!(editor.render(10).0.len(), 1);
    }

    #[test]
    fn panel_fields_share_the_macos_editing_keys() {
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        let mut field = Editor::from_text("one two three");
        field.edit_key(key(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(field.text(), "one two ");
        field.edit_key(key(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(field.field(None), "one \u{E0001}two ");
        field.edit_key(key(KeyCode::Backspace, KeyModifiers::SUPER));
        assert_eq!(field.field(Some('•')), "\u{E0001}••••");
    }

    #[test]
    fn ctrl_h_deletes_a_word_like_ctrl_backspace_in_linux_terminals() {
        let mut field = Editor::from_text("one two three");
        field.edit_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(field.text(), "one two ");
    }

    #[test]
    fn history_walks_back_and_forth_from_an_empty_editor() {
        let mut editor = Editor::new();
        editor.insert("one");
        editor.take();
        editor.insert("two");
        editor.take();
        editor.up(80);
        assert_eq!(editor.text(), "two");
        editor.up(80);
        assert_eq!(editor.text(), "one");
        editor.down(80);
        editor.down(80);
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn wraps_at_word_boundaries() {
        let mut editor = Editor::new();
        editor.insert("aa bbb cccc");
        let (lines, cursor) = editor.render(10);
        let rows: Vec<String> = lines.iter().map(|line| line.iter().map(|s| s.text.as_str()).collect()).collect();
        assert_eq!((rows, cursor), (vec!["❯ aa bbb ".to_string(), "  cccc".to_string()], (6, 1)));
    }

    #[test]
    fn a_leading_bang_shows_as_a_dollar_prompt() {
        let mut editor = Editor::new();
        let rows = |editor: &Editor| editor.render(20).0.iter().map(|line| line.iter().map(|s| s.text.as_str()).collect::<String>()).collect::<Vec<_>>();
        editor.insert("hi\nthere");
        assert_eq!(rows(&editor), ["❯ hi", "  there"]);
        editor.set("!ls");
        assert_eq!(rows(&editor), ["$ ls"]);
        assert_eq!(editor.render(20).1, (4, 0));
        assert_eq!(editor.render(20).0[0][0].style, SHELL_PROMPT);
    }

    #[test]
    fn cursor_wraps_with_the_text() {
        let mut editor = Editor::new();
        editor.insert("abcdef");
        let (rows, cursor) = editor.render(6);
        assert_eq!(rows.len(), 2);
        assert_eq!(cursor, (4, 1));
    }

    #[test]
    fn up_and_down_move_through_wrapped_rows() {
        let mut editor = Editor::new();
        editor.insert("abcdef");
        editor.up(6);
        assert_eq!(editor.render(6).1, (4, 0));
        editor.down(6);
        assert_eq!(editor.render(6).1, (4, 1));
        assert_eq!(editor.text(), "abcdef");
    }
}
