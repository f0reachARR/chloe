//! ワークスペースを書かずに端末で入ったとき、入るワークスペースを選ばせる画面。

use chloe_agent_proto::{ClientMessage, WindowSize};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Size};
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use russh::ChannelId;
use russh::server::Handle;
use terminput::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;
use tracing::warn;

use super::term::{ENTER, InputParser, LEAVE, Output};
use crate::ops::WorkspaceInfo;

/// 選んだ結果。
#[derive(Debug, PartialEq, Eq)]
pub enum Picked {
    /// このワークスペースに入る (SSH で書く名前)。
    Workspace(String),
    /// 利用者が選ばずに閉じた。
    Cancelled,
    /// チャネルが閉じた。
    Closed,
}

/// 選ぶ画面の状態。
pub struct Picker {
    login: String,
    /// 先頭は個人の `default` (まだなければ `None`)。
    rows: Vec<(String, Option<WorkspaceInfo>)>,
    selected: usize,
    picked: Option<Picked>,
}

impl Picker {
    /// `default` は、まだなくても先頭に置き、最初に選んでおく。
    pub fn new(login: &str, default: &str, workspaces: Vec<WorkspaceInfo>) -> Self {
        let mut rows = vec![(default.to_string(), None)];
        for ws in workspaces {
            if ws.name == default {
                rows[0].1 = Some(ws);
            } else {
                rows.push((ws.name.clone(), Some(ws)));
            }
        }
        Self {
            login: login.to_string(),
            rows,
            selected: 0,
            picked: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CTRL);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if !ctrl => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if !ctrl => {
                self.selected = (self.selected + 1).min(self.rows.len() - 1);
            }
            KeyCode::Enter => {
                self.picked = Some(Picked::Workspace(self.rows[self.selected].0.clone()));
            }
            KeyCode::Esc | KeyCode::Char('q') if !ctrl => self.picked = Some(Picked::Cancelled),
            KeyCode::Char('c' | 'd') if ctrl => self.picked = Some(Picked::Cancelled),
            _ => {}
        }
    }

    pub fn render(&self, frame: &mut Frame) {
        let [body, hints_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
        let block =
            Block::bordered().title(format!(" chloe — {}: choose a workspace ", self.login).bold());
        let inner = block.inner(body);
        frame.render_widget(block, body);
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|(name, ws)| {
                let (root, state) = match ws {
                    Some(ws) => (
                        ws.root.as_str(),
                        if ws.running { "running" } else { "stopped" },
                    ),
                    None => ("", "new"),
                };
                ListItem::new(format!("{name:<32} {root:<11} {state}"))
            })
            .collect();
        let list = List::new(items)
            .highlight_style(Style::new().reversed())
            .highlight_symbol("› ");
        let mut state = ListState::default().with_selected(Some(self.selected));
        frame.render_stateful_widget(list, inner, &mut state);
        frame.render_widget(
            Paragraph::new(Line::from(
                "↑↓ select  Enter connect  q quit  (connect as chloe+<name>@ to skip this)"
                    .dark_gray(),
            )),
            hints_area,
        );
    }
}

/// 選ぶ画面を出し、選び終わるかチャネルが閉じたら元の画面に戻す。画面の大きさが変わったら
/// `size` に書き戻す (このあと開くシェルに渡すため)。
pub async fn pick(
    mut picker: Picker,
    handle: &Handle,
    channel: ChannelId,
    size: &mut WindowSize,
    input: &mut mpsc::Receiver<ClientMessage>,
) -> Picked {
    let send = |data: Vec<u8>| async move { handle.data(channel, data).await.is_ok() };
    let output = Output::default();
    let mut terminal = match super::terminal(output.clone(), Size::new(size.cols, size.rows)) {
        Ok(terminal) => terminal,
        Err(error) => {
            warn!("failed to start the workspace picker: {error}");
            return Picked::Closed;
        }
    };
    if !send(ENTER.to_vec()).await {
        return Picked::Closed;
    }
    let mut parser = InputParser::default();
    let picked = loop {
        if let Some(picked) = picker.picked.take() {
            break picked;
        }
        if let Err(error) = terminal.draw(|frame| picker.render(frame)) {
            warn!("failed to draw the workspace picker: {error}");
            break Picked::Closed;
        }
        if !send(output.take()).await {
            break Picked::Closed;
        }
        match input.recv().await {
            Some(ClientMessage::Stdin(data)) => {
                for event in parser.feed(&data) {
                    if let Event::Key(key) = event
                        && key.kind != KeyEventKind::Release
                        && picker.picked.is_none()
                    {
                        picker.handle_key(key);
                    }
                }
            }
            Some(ClientMessage::Resize(new_size)) => {
                *size = new_size;
                if super::resize(&mut terminal, new_size).is_err() {
                    break Picked::Closed;
                }
            }
            Some(ClientMessage::CloseStdin) | None => break Picked::Closed,
            Some(_) => {}
        }
    };
    let _ = send(output.take()).await;
    let _ = send(LEAVE.to_vec()).await;
    picked
}

#[cfg(test)]
mod tests {
    use super::super::form::tests::{ctrl, key};
    use super::*;
    use chloe_controller::workspace::RootMode;

    fn ws(name: &str, running: bool) -> WorkspaceInfo {
        WorkspaceInfo {
            name: name.into(),
            root: RootMode::Ephemeral,
            running,
            workspace_disk_gib: 20,
            docker_disk_gib: 40,
        }
    }

    fn screen(picker: &Picker) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(90, 8)).unwrap();
        terminal.draw(|frame| picker.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn default_comes_first_even_if_missing() {
        let picker = Picker::new("alice", "default", vec![ws("acme/web", true)]);
        let screen = screen(&picker);
        let default = screen.find("default").unwrap();
        let web = screen.find("acme/web").unwrap();
        assert!(default < web);
        assert!(screen.contains("new"));
        assert!(screen.contains("running"));

        let picker = Picker::new(
            "alice",
            "default",
            vec![ws("api", false), ws("default", true)],
        );
        assert_eq!(picker.rows[0].0, "default");
        assert!(picker.rows[0].1.as_ref().unwrap().running);
        assert_eq!(picker.rows.len(), 2);
    }

    #[test]
    fn picks_with_keys() {
        let mut picker = Picker::new(
            "alice",
            "default",
            vec![ws("api", false), ws("acme/web", true)],
        );
        picker.handle_key(key(KeyCode::Up));
        picker.handle_key(key(KeyCode::Down));
        picker.handle_key(key(KeyCode::Down));
        picker.handle_key(key(KeyCode::Down));
        picker.handle_key(key(KeyCode::Enter));
        assert_eq!(picker.picked, Some(Picked::Workspace("acme/web".into())));

        let mut picker = Picker::new("alice", "default", vec![ws("api", false)]);
        picker.handle_key(key(KeyCode::Enter));
        assert_eq!(picker.picked, Some(Picked::Workspace("default".into())));

        let mut picker = Picker::new("alice", "default", vec![ws("api", false)]);
        picker.handle_key(ctrl('c'));
        assert_eq!(picker.picked, Some(Picked::Cancelled));
    }
}
