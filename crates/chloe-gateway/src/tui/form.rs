//! 入力欄を並べたフォーム。
//!
//! 欄は一行の入力、隠す入力、選択、行の一覧 (規則など、行ごとに小さなフォームを持つ)、
//! 読むだけの説明のどれか。Tab と ↑↓ で欄を移り、Ctrl+S で保存し、Esc で閉じる。

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use terminput::{KeyCode, KeyEvent, KeyModifiers};
use tui_input::{Input, InputRequest};

/// 欄の名前の幅。
const LABEL_WIDTH: u16 = 18;

/// 行の一覧の欄で、行を作り、一行に縮めて見せる方法。
#[derive(Clone, Copy)]
pub struct RowKind {
    pub make: fn() -> Form,
    pub summary: fn(&Form) -> String,
}

#[derive(Clone)]
pub enum Value {
    Text(Input),
    /// 値を見せない入力。
    Secret(Input),
    Choice {
        options: Vec<String>,
        index: usize,
    },
    Rows {
        rows: Vec<Form>,
        kind: RowKind,
    },
    /// 読むだけの説明。移れない。
    Note(String),
}

#[derive(Clone)]
pub struct Field {
    pub key: &'static str,
    pub label: String,
    pub help: String,
    pub value: Value,
    /// `(選択の欄, 値)`：その選択の欄がこの値のときだけ見せる。
    pub shown_when: Option<(&'static str, &'static str)>,
}

impl Field {
    fn new(key: &'static str, label: &str, value: Value) -> Self {
        Self {
            key,
            label: label.to_string(),
            help: String::new(),
            value,
            shown_when: None,
        }
    }

    pub fn text(key: &'static str, label: &str, value: &str) -> Self {
        Self::new(key, label, Value::Text(Input::new(value.to_string())))
    }

    pub fn secret(key: &'static str, label: &str) -> Self {
        Self::new(key, label, Value::Secret(Input::default()))
    }

    pub fn choice(key: &'static str, label: &str, options: &[&str], selected: &str) -> Self {
        let options: Vec<String> = options.iter().map(|o| o.to_string()).collect();
        let index = options.iter().position(|o| o == selected).unwrap_or(0);
        Self::new(key, label, Value::Choice { options, index })
    }

    pub fn rows(key: &'static str, label: &str, kind: RowKind, rows: Vec<Form>) -> Self {
        Self::new(key, label, Value::Rows { rows, kind })
    }

    pub fn note(label: &str, text: impl Into<String>) -> Self {
        Self::new("", label, Value::Note(text.into()))
    }

    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = help.into();
        self
    }

    pub fn shown_when(mut self, key: &'static str, value: &'static str) -> Self {
        self.shown_when = Some((key, value));
        self
    }

    fn focusable(&self) -> bool {
        !matches!(self.value, Value::Note(_))
    }
}

/// キーを受けたフォームが、呼び出し側に頼むこと。
#[derive(Debug, PartialEq, Eq)]
pub enum FormEvent {
    None,
    Save,
    Cancel,
    /// 行の一覧の欄 (欄の番号) を開く。
    OpenRows(usize),
}

#[derive(Clone)]
pub struct Form {
    pub title: String,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub error: Option<String>,
}

impl Form {
    pub fn new(title: impl Into<String>, fields: Vec<Field>) -> Self {
        let mut form = Self {
            title: title.into(),
            fields,
            focus: 0,
            error: None,
        };
        if !form.can_focus(0) {
            form.move_focus(1);
        }
        form
    }

    fn field(&self, key: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.key == key)
    }

    fn field_mut(&mut self, key: &str) -> Option<&mut Field> {
        self.fields.iter_mut().find(|f| f.key == key)
    }

    /// 一行の入力 (隠す入力を含む) の値。前後の空白は除く。
    pub fn text(&self, key: &str) -> String {
        match self.field(key).map(|f| &f.value) {
            Some(Value::Text(input) | Value::Secret(input)) => input.value().trim().to_string(),
            _ => String::new(),
        }
    }

    /// `,` で区切った一覧。空の項目は除く。
    pub fn list(&self, key: &str) -> Vec<String> {
        split_list(&self.text(key))
    }

    pub fn choice(&self, key: &str) -> String {
        match self.field(key).map(|f| &f.value) {
            Some(Value::Choice { options, index }) => options[*index].clone(),
            _ => String::new(),
        }
    }

    pub fn rows(&self, key: &str) -> &[Form] {
        match self.field(key).map(|f| &f.value) {
            Some(Value::Rows { rows, .. }) => rows,
            _ => &[],
        }
    }

    pub fn set_text(&mut self, key: &str, value: &str) {
        if let Some(Value::Text(input) | Value::Secret(input)) =
            self.field_mut(key).map(|f| &mut f.value)
        {
            *input = Input::new(value.to_string());
        }
    }

    pub fn set_choice(&mut self, key: &str, value: &str) {
        if let Some(Value::Choice { options, index }) = self.field_mut(key).map(|f| &mut f.value)
            && let Some(i) = options.iter().position(|o| o == value)
        {
            *index = i;
        }
    }

    /// 行の一覧の欄から行を取り出す (一覧を編集するあいだ)。
    pub fn take_rows(&mut self, index: usize) -> Option<(Vec<Form>, RowKind, String)> {
        let field = self.fields.get_mut(index)?;
        match &mut field.value {
            Value::Rows { rows, kind } => Some((std::mem::take(rows), *kind, field.label.clone())),
            _ => None,
        }
    }

    pub fn put_rows(&mut self, index: usize, new_rows: Vec<Form>) {
        if let Some(Value::Rows { rows, .. }) = self.fields.get_mut(index).map(|f| &mut f.value) {
            *rows = new_rows;
        }
    }

    fn is_shown(&self, index: usize) -> bool {
        match self.fields[index].shown_when {
            Some((key, value)) => self.choice(key) == value,
            None => true,
        }
    }

    fn can_focus(&self, index: usize) -> bool {
        index < self.fields.len() && self.is_shown(index) && self.fields[index].focusable()
    }

    fn move_focus(&mut self, step: isize) {
        let len = self.fields.len() as isize;
        let mut index = self.focus as isize;
        for _ in 0..len {
            index = (index + step).rem_euclid(len);
            if self.can_focus(index as usize) {
                self.focus = index as usize;
                return;
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FormEvent {
        let ctrl = key.modifiers.contains(KeyModifiers::CTRL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('s') if ctrl => return FormEvent::Save,
            KeyCode::Esc => return FormEvent::Cancel,
            KeyCode::Up => {
                self.move_focus(-1);
                return FormEvent::None;
            }
            // Shift+Tab は CSI Z で届き、SHIFT の付いた Tab になる。
            KeyCode::Tab if shift => {
                self.move_focus(-1);
                return FormEvent::None;
            }
            KeyCode::Tab | KeyCode::Down => {
                self.move_focus(1);
                return FormEvent::None;
            }
            _ => {}
        }
        let focus = self.focus;
        let Some(field) = self.fields.get_mut(focus) else {
            return FormEvent::None;
        };
        match &mut field.value {
            Value::Text(input) | Value::Secret(input) => {
                if key.code == KeyCode::Enter {
                    self.move_focus(1);
                } else if let Some(request) = input_request(key) {
                    input.handle(request);
                }
            }
            Value::Choice { options, index } => match key.code {
                KeyCode::Left => *index = (*index + options.len() - 1) % options.len(),
                KeyCode::Right | KeyCode::Char(' ') | KeyCode::Enter => {
                    *index = (*index + 1) % options.len()
                }
                _ => {}
            },
            Value::Rows { .. } => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) {
                    return FormEvent::OpenRows(focus);
                }
            }
            Value::Note(_) => {}
        }
        FormEvent::None
    }

    /// 貼り付けた文字列を、いまの入力欄に入れる。
    pub fn paste(&mut self, text: &str) {
        if let Some(Value::Text(input) | Value::Secret(input)) =
            self.fields.get_mut(self.focus).map(|f| &mut f.value)
        {
            for c in text.chars().filter(|c| !c.is_control()) {
                input.handle(InputRequest::InsertChar(c));
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let block = Block::bordered().title(format!(" {} ", self.title));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [fields_area, help_area, hint_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .areas(inner);

        let shown: Vec<usize> = (0..self.fields.len())
            .filter(|i| self.is_shown(*i))
            .collect();
        // いまの欄が見えるように、上から欄を飛ばす。
        let visible = fields_area.height as usize;
        let position = shown.iter().position(|i| *i == self.focus).unwrap_or(0);
        let skip = (position + 1).saturating_sub(visible);
        let value_width = fields_area.width.saturating_sub(LABEL_WIDTH + 1);
        for (row, index) in shown.iter().skip(skip).take(visible).enumerate() {
            let field = &self.fields[*index];
            let focused = *index == self.focus;
            let y = fields_area.y + row as u16;
            let label_style = if focused {
                Style::new().add_modifier(Modifier::BOLD).fg(Color::Cyan)
            } else {
                Style::new()
            };
            frame.render_widget(
                Paragraph::new(Span::styled(field.label.clone(), label_style)),
                Rect::new(fields_area.x, y, LABEL_WIDTH, 1),
            );
            let value_area = Rect::new(fields_area.x + LABEL_WIDTH + 1, y, value_width, 1);
            let (line, cursor) = value_line(&field.value, value_width as usize, focused);
            frame.render_widget(Paragraph::new(line), value_area);
            if let Some(x) = cursor {
                frame.set_cursor_position((value_area.x + x, y));
            }
        }

        let mut help = Vec::new();
        if let Some(error) = &self.error {
            help.push(Line::from(error.clone()).red());
        }
        if let Some(field) = self.fields.get(self.focus)
            && !field.help.is_empty()
        {
            help.push(Line::from(field.help.clone()).dark_gray());
        }
        frame.render_widget(Paragraph::new(help).wrap(Wrap { trim: true }), help_area);
        frame.render_widget(
            Paragraph::new(
                "Tab/↑↓ move  ←→ choose  Enter open list  Ctrl+S save  Esc cancel".dark_gray(),
            ),
            hint_area,
        );
    }
}

/// 欄の値を一行で見せる。入力欄なら、カーソルの位置 (値の左端から) も返す。
fn value_line(value: &Value, width: usize, focused: bool) -> (Line<'static>, Option<u16>) {
    match value {
        Value::Text(input) => {
            let scroll = input.visual_scroll(width.max(1));
            let text: String = input.value().chars().skip(scroll).collect();
            let cursor = focused.then(|| (input.visual_cursor().saturating_sub(scroll)) as u16);
            (Line::from(text), cursor)
        }
        Value::Secret(input) => {
            let len = input.value().chars().count();
            let line = if len == 0 && !focused {
                Line::from("(unchanged)".dark_gray())
            } else {
                Line::from("•".repeat(len.min(width)))
            };
            (line, focused.then(|| len.min(width) as u16))
        }
        Value::Choice { options, index } => {
            let style = if focused {
                Style::new().reversed()
            } else {
                Style::new()
            };
            (
                Line::from(Span::styled(format!("‹ {} ›", options[*index]), style)),
                None,
            )
        }
        Value::Rows { rows, kind } => {
            let summary = if rows.is_empty() {
                "(none)".to_string()
            } else {
                rows.iter()
                    .map(|r| (kind.summary)(r))
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            let style = if focused {
                Style::new().reversed()
            } else {
                Style::new()
            };
            (
                Line::from(vec![
                    Span::styled(format!("[{} ▸]", rows.len()), style),
                    Span::raw(" "),
                    Span::raw(summary).dark_gray(),
                ]),
                None,
            )
        }
        Value::Note(text) => (Line::from(text.clone()).dark_gray(), None),
    }
}

/// キーを入力欄の操作にする。
fn input_request(key: KeyEvent) -> Option<InputRequest> {
    let ctrl = key.modifiers.contains(KeyModifiers::CTRL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    Some(match key.code {
        KeyCode::Char('a') if ctrl => InputRequest::GoToStart,
        KeyCode::Char('e') if ctrl => InputRequest::GoToEnd,
        KeyCode::Char('u') if ctrl => InputRequest::DeleteLine,
        KeyCode::Char('k') if ctrl => InputRequest::DeleteTillEnd,
        KeyCode::Char('w') if ctrl => InputRequest::DeletePrevWord,
        KeyCode::Char('b') if alt => InputRequest::GoToPrevWord,
        KeyCode::Char('f') if alt => InputRequest::GoToNextWord,
        KeyCode::Char(_) if ctrl || alt => return None,
        KeyCode::Char(c) => InputRequest::InsertChar(c),
        KeyCode::Backspace => InputRequest::DeletePrevChar,
        KeyCode::Delete => InputRequest::DeleteNextChar,
        KeyCode::Left => InputRequest::GoToPrevChar,
        KeyCode::Right => InputRequest::GoToNextChar,
        KeyCode::Home => InputRequest::GoToStart,
        KeyCode::End => InputRequest::GoToEnd,
        _ => return None,
    })
}

/// `a, b,c` を `["a", "b", "c"]` にする。
pub fn split_list(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code)
    }

    pub fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c)).modifiers(KeyModifiers::CTRL)
    }

    pub fn type_text(form: &mut Form, text: &str) {
        for c in text.chars() {
            form.handle_key(key(KeyCode::Char(c)));
        }
    }

    fn sample() -> Form {
        Form::new(
            "sample",
            vec![
                Field::note("about", "read only"),
                Field::text("name", "Name", ""),
                Field::choice("kind", "Kind", &["one", "many"], "one"),
                Field::text("extra", "Extra", "").shown_when("kind", "many"),
                Field::secret("value", "Value"),
            ],
        )
    }

    #[test]
    fn moves_between_shown_fields() {
        let mut form = sample();
        // 説明の欄には止まらない。
        assert_eq!(form.focus, 1);
        type_text(&mut form, "api");
        assert_eq!(form.text("name"), "api");
        form.handle_key(key(KeyCode::Tab));
        assert_eq!(form.focus, 2);
        // 「extra」は kind が many のときだけ見える。
        form.handle_key(key(KeyCode::Down));
        assert_eq!(form.focus, 4);
        form.handle_key(key(KeyCode::Up));
        form.handle_key(key(KeyCode::Right));
        assert_eq!(form.choice("kind"), "many");
        form.handle_key(key(KeyCode::Down));
        assert_eq!(form.focus, 3);
        form.handle_key(key(KeyCode::Down));
        type_text(&mut form, "s3cret");
        assert_eq!(form.text("value"), "s3cret");
        assert_eq!(form.handle_key(ctrl('s')), FormEvent::Save);
        assert_eq!(form.handle_key(key(KeyCode::Esc)), FormEvent::Cancel);
    }

    #[test]
    fn hides_secret_values() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut form = sample();
        form.focus = 4;
        type_text(&mut form, "s3cret");
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal
            .draw(|frame| form.render(frame, frame.area()))
            .unwrap();
        let screen = format!("{:?}", terminal.backend().buffer());
        assert!(!screen.contains("s3cret"));
        assert!(screen.contains("••••••"));
    }

    #[test]
    fn splits_lists() {
        assert_eq!(split_list(" a, b,,c "), ["a", "b", "c"]);
        assert!(split_list("").is_empty());
    }
}
