//! エージェントからの依頼を見て答える Inbox タブ。

use chloe_controller::asks::{AskBody, AskStatus};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::widgets::{ListItem, Paragraph};
use terminput::{KeyCode, KeyEvent};

use super::app::{Action, App, render_list};
use super::form::{Field, Form};
use super::modal::{Modal, Purpose};
use crate::ops::{AskInfo, Reply, format_time_secs};

/// 質問の選択肢のほかに、自由に書いて答えるときの選択肢。
const OTHER_ANSWER: &str = "(write an answer)";

impl App {
    /// 依頼が出たか答えられた。依頼の一覧を読み直す。
    pub fn ask_changed(&mut self) -> Vec<Action> {
        self.track(vec![Action::LoadAsks { all: self.all_asks }])
    }

    /// 答えを待つ依頼の数。
    pub(super) fn pending_asks(&self) -> usize {
        self.asks
            .iter()
            .filter(|info| info.ask.status == AskStatus::Pending)
            .count()
    }

    /// Inbox タブでキーを受ける。
    pub(super) fn handle_inbox_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let selected = self.selection();
        match key.code {
            KeyCode::Enter => {
                if let Some(info) = self.asks.get(selected) {
                    self.modals.push(Modal::Message {
                        title: format!("Ask {}", info.ask.id),
                        lines: describe_ask(info),
                    });
                }
            }
            KeyCode::Char('f') => {
                self.all_asks = !self.all_asks;
                self.selected[self.tab] = 0;
                return self.reload();
            }
            KeyCode::Char('a') => {
                let Some(info) = self.asks.get(selected) else {
                    return vec![];
                };
                let ask = &info.ask;
                if ask.status != AskStatus::Pending {
                    self.status = Some((format!("ask {} is already {}", ask.id, ask.status), true));
                    return vec![];
                }
                match &ask.body {
                    AskBody::Network { .. } if !info.can_allow => {
                        self.status = Some((
                            format!(
                                "you cannot change the rules of {}; deny it or ask someone who can",
                                info.workspace
                            ),
                            true,
                        ));
                    }
                    AskBody::Network { .. } => self.modals.push(Modal::Confirm {
                        message: format!(
                            "Allow {} for {}? The rule ask-{} is added to the workspace's rules.",
                            ask.body.summary(),
                            info.workspace,
                            ask.id
                        ),
                        action: Action::AnswerAsk {
                            id: ask.id,
                            reply: Reply::Allow,
                        },
                    }),
                    AskBody::Question { question, options } => {
                        let mut fields = vec![Field::note("Question", question.clone())];
                        if !options.is_empty() {
                            let mut choices: Vec<&str> =
                                options.iter().map(String::as_str).collect();
                            choices.push(OTHER_ANSWER);
                            fields.push(Field::choice("option", "Answer", &choices, choices[0]));
                            fields.push(
                                Field::text("text", "Text", "").shown_when("option", OTHER_ANSWER),
                            );
                        } else {
                            fields.push(Field::text("text", "Answer", ""));
                        }
                        self.modals.push(Modal::Form {
                            form: Form::new(
                                format!("Answer ask {} ({})", ask.id, info.workspace),
                                fields,
                            ),
                            purpose: Purpose::AnswerAsk {
                                id: ask.id,
                                options: !options.is_empty(),
                            },
                            saving: false,
                        });
                    }
                }
            }
            KeyCode::Char('d') => {
                if let Some(info) = self.asks.get(selected)
                    && info.ask.status == AskStatus::Pending
                {
                    self.modals.push(Modal::Form {
                        form: Form::new(
                            format!("Deny ask {} ({})", info.ask.id, info.workspace),
                            vec![
                                Field::note("Ask", info.ask.body.summary()),
                                Field::text("message", "Message", "")
                                    .help("Optional. The agent receives it with the denial."),
                            ],
                        ),
                        purpose: Purpose::DenyAsk(info.ask.id),
                        saving: false,
                    });
                }
            }
            _ => {}
        }
        vec![]
    }

    /// 読んだ依頼の一覧を入れ、新しく来たものを知らせる。
    pub(super) fn apply_asks(&mut self, all: bool, asks: Vec<AskInfo>) {
        // 切り替える前に頼んだ一覧は捨てる。
        if all == self.all_asks {
            let new: Vec<&AskInfo> = asks
                .iter()
                .filter(|info| {
                    info.ask.status == AskStatus::Pending
                        && !self.asks.iter().any(|old| old.ask.id == info.ask.id)
                })
                .collect();
            if self.asks_loaded
                && let Some(info) = new.first()
            {
                self.status = Some((
                    format!(
                        "new ask {} from {}: {} (Inbox)",
                        info.ask.id,
                        info.workspace,
                        info.ask.body.summary()
                    ),
                    false,
                ));
            }
            self.asks = asks;
            self.asks_loaded = true;
        }
    }

    /// Inbox タブ。
    pub(super) fn render_inbox(&self, frame: &mut Frame, area: Rect) {
        let [head_area, list_area] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
        let showing = if self.all_asks {
            "all asks"
        } else {
            "asks waiting for an answer"
        };
        frame.render_widget(
            Paragraph::new(format!(
                "Showing {showing} from the agents in your workspaces  (f: switch)"
            )),
            head_area,
        );
        let items: Vec<ListItem> = self.asks.iter().map(ask_item).collect();
        render_list(frame, list_area, items, self.selection());
    }
}

/// 質問への答えのフォームを、答える操作にする。
pub(super) fn answer(form: &Form, id: i64, options: bool) -> Result<Action, String> {
    let answer = match form.choice("option") {
        choice if options && choice != OTHER_ANSWER => choice,
        _ => form.text("text"),
    };
    if answer.trim().is_empty() {
        return Err("enter an answer".into());
    }
    Ok(Action::AnswerAsk {
        id,
        reply: Reply::Answer(answer),
    })
}

/// 断るフォームを、断る操作にする。
pub(super) fn deny(form: &Form, id: i64) -> Action {
    let message = Some(form.text("message")).filter(|m| !m.trim().is_empty());
    Action::AnswerAsk {
        id,
        reply: Reply::Deny(message),
    }
}

/// 依頼を一覧の一行にする。答えを待つものは黄色、断ったものと期限切れは灰色。
fn ask_item(info: &AskInfo) -> ListItem<'static> {
    let ask = &info.ask;
    let time = format_time_secs(ask.created_at)[5..].to_string();
    let line = format!(
        "{:<5} {time} {:<24} {:<8} {:<9} {}",
        ask.id,
        info.workspace,
        ask.body.kind(),
        ask.status.as_str(),
        ask.body.summary()
    );
    match ask.status {
        AskStatus::Pending => ListItem::new(line.yellow()),
        AskStatus::Allowed | AskStatus::Answered => ListItem::new(line),
        _ => ListItem::new(line.dark_gray()),
    }
}

/// 依頼の欄をすべて出す。
fn describe_ask(info: &AskInfo) -> Vec<String> {
    let ask = &info.ask;
    let mut lines = vec![
        format!("workspace   {}", info.workspace),
        format!("kind        {}", ask.body.kind()),
        format!("status      {}", ask.status),
        format!("asked       {} UTC", format_time_secs(ask.created_at)),
        format!("expires     {} UTC", format_time_secs(ask.expires_at)),
    ];
    match &ask.body {
        AskBody::Network {
            host,
            methods,
            paths,
        } => {
            let all = |items: &[String]| {
                if items.is_empty() {
                    "(all)".to_string()
                } else {
                    items.join(" ")
                }
            };
            lines.push(format!("host        {host}"));
            lines.push(format!("methods     {}", all(methods)));
            lines.push(format!("paths       {}", all(paths)));
        }
        AskBody::Question { question, options } => {
            lines.push("question".into());
            lines.extend(question.lines().map(|l| format!("  {l}")));
            for (i, option) in options.iter().enumerate() {
                lines.push(format!("option {}    {option}", i + 1));
            }
        }
    }
    if let Some(reason) = &ask.reason {
        lines.push("reason".into());
        lines.extend(reason.lines().map(|l| format!("  {l}")));
    }
    if let Some(answer) = &ask.answer {
        lines.push("answer".into());
        lines.extend(answer.lines().map(|l| format!("  {l}")));
    }
    if let Some(by) = &ask.answered_by {
        lines.push(format!("by          {by}"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::super::app::Outcome;
    use super::super::app::tests::{app, screen};
    use super::super::form::tests::{ctrl, key, type_text};
    use super::*;

    fn ask_info(id: i64, body: AskBody, can_allow: bool) -> AskInfo {
        AskInfo {
            workspace: "alice/default".into(),
            ask: chloe_controller::asks::Ask {
                id,
                workspace_id: 1,
                body,
                reason: Some("to read the docs".into()),
                status: AskStatus::Pending,
                answer: None,
                answered_by: None,
                created_at: 1_792_152_000,
                expires_at: 1_792_238_400,
                answered_at: None,
            },
            can_allow,
        }
    }

    fn network_ask(id: i64, can_allow: bool) -> AskInfo {
        ask_info(
            id,
            AskBody::Network {
                host: "docs.example.com".into(),
                methods: vec![],
                paths: vec![],
            },
            can_allow,
        )
    }

    fn question_ask(id: i64) -> AskInfo {
        ask_info(
            id,
            AskBody::Question {
                question: "Which branch?".into(),
                options: vec!["main".into(), "dev".into()],
            },
            false,
        )
    }

    #[test]
    fn answers_asks_in_the_inbox() {
        let mut app = app();
        app.apply(Outcome::Asks {
            all: false,
            asks: vec![network_ask(1, true)],
        });
        // 始めに読んだ依頼は、新しく来たとは知らせない。
        assert!(!screen(&app).contains("new ask"));
        assert!(screen(&app).contains("8 Inbox (1)"));

        // 依頼が出たら読み直し、新しく来たものを知らせる。
        let actions = app.ask_changed();
        assert!(matches!(&actions[..], [Action::LoadAsks { all: false }]));
        app.apply(Outcome::Asks {
            all: false,
            asks: vec![question_ask(2), network_ask(1, true)],
        });
        assert!(screen(&app).contains("new ask 2 from alice/default: Which branch?"));
        assert!(screen(&app).contains("8 Inbox (2)"));

        app.handle_key(key(KeyCode::Char('8')));
        let shown = screen(&app);
        assert!(
            shown.contains("question pending   Which branch?"),
            "{shown}"
        );
        assert!(shown.contains("docs.example.com"));

        // 質問は選択肢で答える。
        app.handle_key(key(KeyCode::Char('a')));
        assert!(screen(&app).contains("Answer ask 2"));
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Right));
        let actions = app.handle_key(ctrl('s'));
        assert!(
            matches!(&actions[..], [Action::AnswerAsk { id: 2, reply: Reply::Answer(a) }] if a == "dev"),
            "{}",
            screen(&app)
        );
        app.apply(Outcome::Done {
            message: "ask 2 is answered".into(),
            reload: vec![],
        });

        // 通信の許可は確かめてから承認する。詳細も見られる。
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        assert!(screen(&app).contains("reason"));
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Char('a')));
        assert!(screen(&app).contains("Allow docs.example.com for alice/default?"));
        let actions = app.handle_key(key(KeyCode::Char('y')));
        assert!(matches!(
            &actions[..],
            [Action::AnswerAsk {
                id: 1,
                reply: Reply::Allow
            }]
        ));

        // 規則を変えられない人は承認できず、断ることはできる。
        app.apply(Outcome::Asks {
            all: false,
            asks: vec![network_ask(3, false)],
        });
        app.handle_key(key(KeyCode::Char('a')));
        assert!(screen(&app).contains("you cannot change the rules"));
        app.handle_key(key(KeyCode::Char('d')));
        type_text_into_message(&mut app, "not now");
        let actions = app.handle_key(ctrl('s'));
        assert!(
            matches!(&actions[..], [Action::AnswerAsk { id: 3, reply: Reply::Deny(Some(m)) }] if m == "not now")
        );

        // 答えたものも出すと、一覧を読み直す。
        app.apply(Outcome::Done {
            message: "ask 3 is denied".into(),
            reload: vec![],
        });
        let actions = app.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(&actions[..], [Action::LoadAsks { all: true }]));
    }

    /// 断るフォームの言葉の欄に書く。
    fn type_text_into_message(app: &mut App, text: &str) {
        if let Some(Modal::Form { form, .. }) = app.modals.last_mut() {
            type_text(form, text);
        }
    }
}
