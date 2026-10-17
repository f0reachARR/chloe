//! SSH のチャネルを端末として使う ratatui のバックエンドと、入力のバイト列の解釈。
//!
//! ゲートウェイには端末がないので、crossterm の端末の大きさの問い合わせやイベントの読み取りは
//! 使えない。大きさは SSH の pty-req と window-change から、入力はチャネルのデータから得る。

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use terminput::Event;

/// 代替画面に切り替え、カーソルを隠す。
pub const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l";
/// カーソルを戻し、元の画面に戻す。
pub const LEAVE: &[u8] = b"\x1b[?25h\x1b[?1049l";

/// 描画したバイト列を溜めておく。描画のたびに取り出して SSH のチャネルに送る。
#[derive(Clone, Default)]
pub struct Output(Arc<Mutex<Vec<u8>>>);

impl Output {
    pub fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

impl Write for Output {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 端末の大きさを SSH から受け取る、crossterm の制御列を書くバックエンド。
pub struct SshBackend {
    inner: CrosstermBackend<Output>,
    size: Size,
}

impl SshBackend {
    pub fn new(output: Output, size: Size) -> Self {
        Self {
            inner: CrosstermBackend::new(output),
            size,
        }
    }

    pub fn set_size(&mut self, size: Size) {
        self.size = size;
    }
}

impl Backend for SshBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    // 端末に問い合わせると答えを待つことになるので、問い合わせない (固定の画面では使わない)。
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(Position::ORIGIN)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        Ok(self.size)
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.size,
            pixels: Size::default(),
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

/// SSH のチャネルから届いたバイト列を、キーなどのイベントにする。
///
/// エスケープシーケンスや UTF-8 の文字は、データの区切りをまたいで届くことがある。
#[derive(Default)]
pub struct InputParser {
    pending: Vec<u8>,
}

impl InputParser {
    pub fn feed(&mut self, data: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for (i, byte) in data.iter().enumerate() {
            self.pending.push(*byte);
            // ESC だけなら Esc キーとも読めるが、同じデータに続きがあればシーケンスの始まりとして待つ。
            if self.pending == [0x1b] && i + 1 < data.len() {
                continue;
            }
            match Event::parse_from(&self.pending) {
                Ok(Some(event)) => {
                    events.push(event);
                    self.pending.clear();
                }
                Ok(None) => {}
                Err(_) => self.pending.clear(),
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use terminput::{KeyCode, KeyModifiers};

    fn keys(events: &[Event]) -> Vec<(KeyCode, KeyModifiers)> {
        events
            .iter()
            .filter_map(|e| e.as_key())
            .map(|k| (k.code, k.modifiers))
            .collect()
    }

    #[test]
    fn parses_keys_across_chunks() {
        let mut parser = InputParser::default();
        assert_eq!(
            keys(&parser.feed(b"\x1b[Aq\r")),
            [
                (KeyCode::Up, KeyModifiers::NONE),
                (KeyCode::Char('q'), KeyModifiers::NONE),
                (KeyCode::Enter, KeyModifiers::NONE),
            ]
        );
        // ESC だけのデータは Esc キー。
        assert_eq!(
            keys(&parser.feed(b"\x1b")),
            [(KeyCode::Esc, KeyModifiers::NONE)]
        );
        // UTF-8 の文字が二つのデータに分かれて届く。
        let text = "あ".as_bytes();
        assert!(parser.feed(&text[..1]).is_empty());
        assert_eq!(
            keys(&parser.feed(&text[1..])),
            [(KeyCode::Char('あ'), KeyModifiers::NONE)]
        );
        assert_eq!(
            keys(&parser.feed(b"\x03")),
            [(KeyCode::Char('c'), KeyModifiers::CTRL)]
        );
    }

    #[test]
    fn draws_into_the_output() {
        use ratatui::widgets::Paragraph;
        use ratatui::{Terminal, TerminalOptions, Viewport};

        let output = Output::default();
        let backend = SshBackend::new(output.clone(), Size::new(20, 2));
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 20, 2)),
            },
        )
        .unwrap();
        terminal
            .draw(|frame| frame.render_widget(Paragraph::new("hello"), frame.area()))
            .unwrap();
        assert!(String::from_utf8_lossy(&output.take()).contains("hello"));
        assert!(output.take().is_empty());
    }
}
