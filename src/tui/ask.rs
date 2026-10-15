use crossterm::event::{KeyCode, KeyEvent};
use std::sync::mpsc::Sender;

use super::editor::Editor;
use super::render::{Line, PLAIN, Style, span, wrap};
use crate::ask::{Answer, Question, TYPE_SOMETHING};

const RULE: Style = Style::fg(240);
const BRIGHT: Style = Style::fg(255);
const MUTED: Style = Style::fg(245);
const PREVIEW_LINES: usize = 8;

/// The ask_user_question panel: one question at a time, the agent thread waits on `reply`.
pub struct AskPanel {
    questions: Vec<Question>,
    reply: Sender<Vec<Option<Answer>>>,
    answers: Vec<Option<Answer>>,
    current: usize,
    /// Focused row; `options.len()` is the "Type something." row.
    row: usize,
    picked: Vec<usize>,
    typed: Editor,
}

impl AskPanel {
    pub fn new(questions: Vec<Question>, reply: Sender<Vec<Option<Answer>>>) -> AskPanel {
        AskPanel { answers: vec![None; questions.len()], questions, reply, current: 0, row: 0, picked: Vec::new(), typed: Editor::new() }
    }

    /// Pasted text can only be a typed answer, so it moves to the "Type something." row.
    pub fn paste(&mut self, text: &str) {
        self.row = self.questions[self.current].options.len();
        self.typed.insert(text);
    }

    /// Returns true once the answers (or the cancel) went back to the agent.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        let question = &self.questions[self.current];
        let custom_row = question.options.len();
        match key.code {
            KeyCode::Esc => {
                let _ = self.reply.send(vec![None; self.questions.len()]);
                return true;
            }
            KeyCode::Up => self.row = self.row.saturating_sub(1),
            KeyCode::Down => self.row = (self.row + 1).min(custom_row),
            KeyCode::Char(' ') if question.multi && self.row < custom_row => {
                match self.picked.iter().position(|i| *i == self.row) {
                    Some(at) => _ = self.picked.remove(at),
                    None => self.picked.push(self.row),
                }
            }
            KeyCode::Enter => {
                let answer = match (question.multi, self.row == custom_row) {
                    (true, _) if self.picked.is_empty() && self.typed.is_empty() => return false,
                    (true, _) => {
                        self.picked.sort();
                        Answer::Multi { picked: std::mem::take(&mut self.picked), typed: self.typed.take() }
                    }
                    (false, true) if self.typed.is_empty() => return false,
                    (false, true) => Answer::Typed(self.typed.take()),
                    (false, false) => Answer::Picked(self.row),
                };
                self.answers[self.current] = Some(answer);
                self.current += 1;
                self.row = 0;
                if self.current == self.questions.len() {
                    let _ = self.reply.send(std::mem::take(&mut self.answers));
                    return true;
                }
            }
            _ if self.row == custom_row => _ = self.typed.edit_key(key),
            _ => {}
        }
        false
    }

    pub fn render(&self, width: usize) -> Vec<Line> {
        let question = &self.questions[self.current];
        let rule = vec![span("─".repeat(width), RULE)];
        let count = format!("  {}/{}", self.current + 1, self.questions.len());
        let mut lines = vec![rule.clone(), vec![span(question.header.clone(), BRIGHT.bold()), span(count, MUTED)]];
        lines.extend(wrap(&[span(question.question.clone(), PLAIN.bold())], width, &Vec::new()));
        lines.push(Vec::new());
        for (index, option) in question.options.iter().enumerate() {
            let check = match (question.multi, self.picked.contains(&index)) {
                (false, _) => "",
                (true, true) => "[x] ",
                (true, false) => "[ ] ",
            };
            let mut line = self.lead(index);
            line.push(span(format!("{check}{}", option.label), self.style(index)));
            if !option.description.is_empty() {
                line.push(span(format!("  {}", option.description), MUTED));
            }
            lines.push(line);
        }
        let custom = question.options.len();
        let mut line = self.lead(custom);
        line.push(span(TYPE_SOMETHING, self.style(custom)));
        lines.push(line);
        if self.row == custom || !self.typed.is_empty() {
            let indent = vec![span("       ", PLAIN)];
            lines.extend(wrap(&[span(self.typed.field(None), BRIGHT)], width, &indent));
        }
        if let Some(preview) = question.options.get(self.row).and_then(|o| o.preview.as_ref()) {
            lines.push(Vec::new());
            lines.extend(preview.lines().take(PREVIEW_LINES).map(|l| vec![span(format!("    {l}"), MUTED)]));
        }
        let hint = if question.multi { "↑↓ move     space toggle     enter submit     esc cancel" } else { "↑↓ move     enter choose     esc cancel" };
        lines.extend([Vec::new(), rule, vec![span(hint, MUTED)]]);
        lines
    }

    fn lead(&self, row: usize) -> Line {
        vec![span(if row == self.row { "  ❯ " } else { "    " }, PLAIN), span(format!("{}. ", row + 1), MUTED)]
    }

    fn style(&self, row: usize) -> Style {
        if row == self.row { BRIGHT.bold() } else { PLAIN }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::mpsc;

    #[test]
    fn a_long_typed_answer_wraps_inside_the_panel() {
        let input = json!({"questions": [{"question": "Q?", "header": "H", "options": [{"label": "a", "description": ""}, {"label": "b", "description": ""}]}]});
        let (reply, _answers) = mpsc::channel();
        let mut panel = AskPanel::new(crate::ask::parse(&input).unwrap(), reply);
        panel.paste("one two three four five six seven eight nine ten");
        let text: Vec<String> = panel.render(24).iter().map(|l| l.iter().map(|s| s.text.as_str()).collect()).collect();
        let answer: Vec<&String> = text.iter().filter(|row| ["one", "five", "ten"].iter().any(|word| row.contains(word))).collect();
        assert!(answer.len() > 1 && answer.iter().all(|row| super::super::render::width(row) <= 24), "{text:#?}");
    }

    #[test]
    fn answers_each_question_in_turn_then_replies() {
        let input = json!({"questions": [
            {"question": "Q1?", "header": "One", "options": [{"label": "a", "description": ""}, {"label": "b", "description": ""}]},
            {"question": "Q2?", "header": "Two", "multiSelect": true, "options": [{"label": "c", "description": ""}, {"label": "d", "description": ""}]},
        ]});
        let (reply, answers) = mpsc::channel();
        let mut panel = AskPanel::new(crate::ask::parse(&input).unwrap(), reply);
        let keys = [KeyCode::Down, KeyCode::Down, KeyCode::Char('x'), KeyCode::Enter, KeyCode::Down, KeyCode::Char(' '), KeyCode::Up, KeyCode::Char(' ')];
        assert!(keys.into_iter().all(|code| !panel.key(code.into())));
        assert!(panel.key(KeyCode::Enter.into()));
        assert_eq!(answers.recv().unwrap(), [Some(Answer::Typed("x".into())), Some(Answer::Multi { picked: vec![0, 1], typed: String::new() })]);
    }
}
