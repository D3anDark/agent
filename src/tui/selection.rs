use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// A transcript cell; `line` counts up from the last transcript line, so scrolling does not move it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Point {
    pub line: usize,
    pub column: usize,
}

/// pj's mouse selection: drag to select, copy on release, the highlight stays until the next click.
/// A double-click selects a word and a drag that follows it grows by whole words.
#[derive(Default)]
pub struct Selection {
    anchor: Option<Point>,
    cursor: Option<Point>,
    pressed: Option<Point>,
    pub dragging: bool,
    word: Option<(Point, Point)>,
    last_click: Option<(Instant, Point)>,
}

impl Selection {
    /// Left button down at `point`, whose line reads `text`.
    pub fn press(&mut self, point: Point, text: &str) {
        let double = self.last_click.is_some_and(|(at, last)| last == point && at.elapsed() < DOUBLE_CLICK);
        if double && let Some(word) = word_at(point, text) {
            self.last_click = None;
            self.word = Some(word);
            (self.anchor, self.cursor) = (Some(word.0), Some(word.1));
            self.dragging = true;
            return;
        }
        *self = Selection { last_click: Some((Instant::now(), point)), pressed: Some(point), ..Selection::default() };
    }

    /// Mouse moved to `point` with the button held.
    pub fn drag(&mut self, point: Point, text: &str) {
        if !self.dragging {
            // Kitty reports pixel motion, so a click with hand jitter sends drags within the pressed cell.
            let Some(pressed) = self.pressed.filter(|pressed| *pressed != point) else { return };
            self.dragging = true;
            self.anchor = Some(pressed);
        }
        let Some((word_start, word_end)) = self.word else {
            self.cursor = Some(point);
            return;
        };
        let (start, end) = word_at(point, text).unwrap_or((point, point));
        (self.anchor, self.cursor) = if before(point, word_start) { (Some(word_end), Some(start)) } else { (Some(word_start), Some(end)) };
    }

    /// Button released; true when a drag just finished and its text should be copied.
    pub fn release(&mut self) -> bool {
        self.pressed = None;
        self.word = None;
        std::mem::take(&mut self.dragging)
    }

    /// First and last selected cell, top to bottom.
    pub fn range(&self) -> Option<(Point, Point)> {
        let (anchor, cursor) = (self.anchor?, self.cursor?);
        Some(if before(cursor, anchor) { (cursor, anchor) } else { (anchor, cursor) })
    }

    /// Selected display columns on transcript `line`.
    pub fn columns(&self, line: usize) -> Option<(usize, usize)> {
        let (start, end) = self.range()?;
        if line > start.line || line < end.line {
            return None;
        }
        let from = if line == start.line { start.column } else { 0 };
        let to = if line == end.line { end.column + 1 } else { usize::MAX };
        Some((from, to))
    }
}

fn before(a: Point, b: Point) -> bool {
    a.line > b.line || (a.line == b.line && a.column < b.column)
}

/// First and last cell of the word under `point`; none on whitespace or past the line end.
fn word_at(point: Point, text: &str) -> Option<(Point, Point)> {
    let (start, end) = word_bounds(text, point.column)?;
    Some((Point { column: start, ..point }, Point { column: end - 1, ..point }))
}

#[derive(Clone, Copy, PartialEq)]
enum Class {
    Word,
    Space,
    Other,
}

fn class(ch: char) -> Class {
    if ch.is_alphanumeric() || ch == '_' {
        Class::Word
    } else if ch.is_whitespace() {
        Class::Space
    } else {
        Class::Other
    }
}

/// Display columns `start..end` of the run of same-class characters under `column`.
fn word_bounds(text: &str, column: usize) -> Option<(usize, usize)> {
    let mut cells: Vec<(usize, usize, Class)> = Vec::new();
    let mut col = 0;
    for ch in text.chars() {
        let width = ch.width().unwrap_or(0);
        let class = match cells.last() {
            Some(&(_, _, previous)) if width == 0 => previous,
            _ => class(ch),
        };
        cells.push((col, col + width, class));
        col += width;
    }
    let hit = cells.iter().position(|&(start, end, _)| start <= column && column < end)?;
    let class = cells[hit].2;
    if class == Class::Space {
        return None;
    }
    let mut lo = hit;
    while lo > 0 && cells[lo - 1].2 == class {
        lo -= 1;
    }
    let mut hi = hit;
    while hi + 1 < cells.len() && cells[hi + 1].2 == class {
        hi += 1;
    }
    Some((cells[lo].0, cells[hi].1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: usize, column: usize) -> Point {
        Point { line, column }
    }

    #[test]
    fn drag_up_selects_from_the_upper_cell_through_the_pressed_one() {
        let mut selection = Selection::default();
        selection.press(at(0, 4), "");
        selection.drag(at(2, 6), "");
        assert!(selection.release());
        assert_eq!(selection.columns(2), Some((6, usize::MAX)));
        assert_eq!(selection.columns(1), Some((0, usize::MAX)));
        assert_eq!(selection.columns(0), Some((0, 5)));
        assert_eq!(selection.columns(3), None);
    }

    #[test]
    fn a_click_with_jitter_selects_nothing() {
        let mut selection = Selection::default();
        selection.press(at(0, 4), "");
        selection.drag(at(0, 4), "");
        assert!(!selection.release());
        assert_eq!(selection.range(), None);
    }

    #[test]
    fn double_click_selects_the_word_and_drags_grow_by_words() {
        let text = "let foo_bar = 世界;";
        let mut selection = Selection::default();
        selection.press(at(0, 6), text);
        selection.release();
        selection.press(at(0, 6), text);
        assert_eq!(selection.columns(0), Some((4, 11)));
        selection.drag(at(0, 15), text);
        assert!(selection.release());
        assert_eq!(selection.columns(0), Some((4, 18)));
        assert_eq!(word_bounds(text, 3), None);
    }
}
