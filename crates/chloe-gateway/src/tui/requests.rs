//! ワークスペースの HTTP の要求のログを見る画面。

use chloe_audit::{Event, Record};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::widgets::{Block, Clear, ListItem, Paragraph};
use terminput::{KeyCode, KeyEvent};

use super::app::{Action, App, render_list};
use super::modal::Modal;
use crate::ops::{format_bytes, format_time_secs};

/// HTTP の要求のログの絞り込み。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum RequestFilter {
    #[default]
    All,
    Blocked,
    Injected,
    Passthrough,
}

impl RequestFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Blocked,
            Self::Blocked => Self::Injected,
            Self::Injected => Self::Passthrough,
            Self::Passthrough => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Blocked => "blocked",
            Self::Injected => "injected",
            Self::Passthrough => "passthrough",
        }
    }

    fn matches(self, record: &Record) -> bool {
        match (self, &record.event) {
            (Self::All, _) => true,
            (Self::Blocked, Event::Request(req)) => req.blocked(),
            (Self::Injected, Event::Request(req)) => !req.injected.is_empty(),
            (Self::Passthrough, Event::Passthrough { .. }) => true,
            _ => false,
        }
    }

    fn apply(self, records: &[Record]) -> Vec<&Record> {
        records.iter().filter(|r| self.matches(r)).collect()
    }
}

impl App {
    /// 開いている HTTP の要求のログでキーを受ける。
    pub(super) fn handle_requests_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Modal::Requests {
            name,
            records,
            filter,
            selected,
        }) = self.modals.last_mut()
        else {
            return vec![];
        };
        let shown = records.as_deref().map_or(0, |r| filter.apply(r).len());
        let last = shown.saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => *selected = (*selected + 1).min(last),
            KeyCode::PageUp => *selected = selected.saturating_sub(10),
            KeyCode::PageDown => *selected = (*selected + 10).min(last),
            KeyCode::Home | KeyCode::Char('g') => *selected = 0,
            KeyCode::End | KeyCode::Char('G') => *selected = last,
            KeyCode::Char('f') => {
                *filter = filter.next();
                // 新しいものを下に並べるので、いちばん下を選ぶ。
                let shown = records.as_deref().map_or(0, |r| filter.apply(r).len());
                *selected = shown.saturating_sub(1);
            }
            KeyCode::Char('r') => return vec![Action::LoadRequests(name.clone())],
            KeyCode::Enter => {
                let record = records
                    .as_deref()
                    .and_then(|r| filter.apply(r).get(*selected).map(|r| (*r).clone()));
                if let Some(record) = record {
                    self.modals.push(Modal::Message {
                        title: "Request".into(),
                        lines: describe_request(&record),
                    });
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modals.pop();
            }
            _ => {}
        }
        vec![]
    }

    /// 読んだログを、そのワークスペースのログの画面に入れる。
    pub(super) fn apply_requests(&mut self, loaded: String, new: Vec<Record>) {
        for modal in &mut self.modals {
            if let Modal::Requests {
                name,
                records,
                filter,
                selected,
            } = modal
                && *name == loaded
            {
                // 新しいものを下に並べるので、いちばん下を選ぶ。
                *selected = filter.apply(&new).len().saturating_sub(1);
                *records = Some(new);
                break;
            }
        }
    }
}

/// HTTP の要求のログの画面。
pub(super) fn render(
    frame: &mut Frame,
    name: &str,
    records: Option<&[Record]>,
    filter: RequestFilter,
    selected: usize,
) {
    let area = frame.area();
    // 下の状態と操作の行は隠さない。
    let area = Rect {
        height: area.height.saturating_sub(2),
        ..area
    };
    frame.render_widget(Clear, area);
    let block = Block::bordered().title(format!(
        " HTTP requests of {name} (showing {}) ",
        filter.label()
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [list_area, hint_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    match records {
        None => frame.render_widget(Paragraph::new("…".dark_gray()), list_area),
        Some(records) => {
            let items = filter
                .apply(records)
                .into_iter()
                .map(request_item)
                .collect();
            render_list(frame, list_area, items, selected);
        }
    }
    frame.render_widget(
        Paragraph::new(
            "Enter details  f filter  r reload  Esc close   (times in UTC, newest at the bottom)"
                .dark_gray(),
        ),
        hint_area,
    );
}

/// ログの一件を一覧の一行にする。遮断したものは赤、中継のみは灰色、差し替えたものは `+` を付ける。
fn request_item(record: &Record) -> ListItem<'static> {
    // 日付の年を落とす (`MM-DD HH:MM:SS`)。
    let time = format_time_secs(record.ts as i64)[5..].to_string();
    let req = match &record.event {
        Event::Passthrough { host } => {
            let text = format!("{time}    - {:<6} {:>6} {:>7} {host}", "TLS", "-", "-");
            return ListItem::new(text.dark_gray());
        }
        Event::Request(req) => req,
    };
    let status = req.status.map_or("-".into(), |s| s.to_string());
    let size = req.response_bytes.map_or("-".into(), format_bytes);
    let duration = req.duration_ms.map_or("-".into(), |ms| format!("{ms}ms"));
    let mark = if req.injected.is_empty() { ' ' } else { '+' };
    let mut text = format!(
        "{time} {mark}{status:>3} {:<6} {size:>6} {duration:>7} {} {}",
        req.method, req.host, req.path
    );
    if req.decision != "allowed" {
        text.push_str(&format!("  [{}]", req.decision));
    }
    if req.blocked() {
        ListItem::new(text.red())
    } else if req.error.is_some() {
        ListItem::new(text.yellow())
    } else {
        ListItem::new(text)
    }
}

/// ログの一件の、すべての欄。
fn describe_request(record: &Record) -> Vec<String> {
    let time = format!("time        {} UTC", format_time_secs(record.ts as i64));
    let req = match &record.event {
        Event::Passthrough { host } => {
            return vec![
                time,
                format!("host        {host}"),
                "            passed through without decrypting TLS".into(),
            ];
        }
        Event::Request(req) => req,
    };
    let or_none = |value: Option<String>| value.unwrap_or_else(|| "-".into());
    let list = |items: &[String]| {
        if items.is_empty() {
            "-".to_string()
        } else {
            items.join(", ")
        }
    };
    vec![
        time,
        format!("host        {}", req.host),
        format!("sni         {}", or_none(req.sni.clone())),
        format!(
            "request     {} {} {}",
            req.method, req.path, req.http_version
        ),
        format!("query keys  {}", list(&req.query_keys)),
        format!("decision    {}", req.decision),
        format!("rule        {}", or_none(req.rule.clone())),
        format!("injected    {}", list(&req.injected)),
        format!("status      {}", or_none(req.status.map(|s| s.to_string()))),
        format!(
            "sent        {}{}",
            or_none(req.request_bytes.map(format_bytes)),
            if req.body_scanned {
                ""
            } else {
                " (body not scanned)"
            }
        ),
        format!(
            "received    {}",
            or_none(req.response_bytes.map(format_bytes))
        ),
        format!(
            "duration    {}",
            or_none(req.duration_ms.map(|ms| format!("{ms}ms")))
        ),
        format!("error       {}", or_none(req.error.clone())),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::app::Outcome;
    use super::super::app::tests::{app, screen};
    use super::super::form::tests::key;
    use super::*;

    fn request(host: &str, decision: &str, injected: &[&str]) -> Record {
        Record {
            ts: 1_792_152_059.0,
            event: Event::Request(Box::new(chloe_audit::Request {
                host: host.into(),
                method: "GET".into(),
                path: "/v1".into(),
                status: Some(if decision == "allowed" { 200 } else { 403 }),
                decision: decision.into(),
                injected: injected.iter().map(|s| s.to_string()).collect(),
                response_bytes: Some(2048),
                duration_ms: Some(42),
                ..Default::default()
            })),
        }
    }

    #[test]
    fn shows_the_request_log_of_a_workspace() {
        let mut app = app();
        let actions = app.handle_key(key(KeyCode::Char('l')));
        assert!(matches!(&actions[..], [Action::LoadRequests(name)] if name == "default"));
        assert!(screen(&app).contains("HTTP requests of default"));
        app.apply(Outcome::Requests {
            name: "default".into(),
            records: vec![
                Record {
                    ts: 1_792_152_000.0,
                    event: Event::Passthrough {
                        host: "pass.example".into(),
                    },
                },
                request("api.example.com", "allowed", &["anthropic_api_key-0"]),
                request("evil.example", "blocked:host", &[]),
            ],
        });
        let shown = screen(&app);
        assert!(shown.contains("10-16 12:00:59 +200 GET      2.0K    42ms api.example.com /v1"));
        assert!(shown.contains("evil.example /v1  [blocked:host]"));
        assert!(shown.contains("pass.example"));

        // 遮断したものだけにすると、いちばん下 (遮断したもの) を選ぶ。
        app.handle_key(key(KeyCode::Char('f')));
        let shown = screen(&app);
        assert!(shown.contains("showing blocked"));
        assert!(!shown.contains("api.example.com"));
        app.handle_key(key(KeyCode::Enter));
        let shown = screen(&app);
        assert!(shown.contains("decision    blocked:host"));
        app.handle_key(key(KeyCode::Esc));

        // 差し替えたものだけ。
        app.handle_key(key(KeyCode::Char('f')));
        let shown = screen(&app);
        assert!(shown.contains("api.example.com"));
        assert!(!shown.contains("evil.example"));

        let actions = app.handle_key(key(KeyCode::Char('r')));
        assert!(matches!(&actions[..], [Action::LoadRequests(_)]));
        app.handle_key(key(KeyCode::Esc));
        assert!(!screen(&app).contains("HTTP requests of"));
    }
}
