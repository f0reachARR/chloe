//! タブの上に重ねる窓 (フォーム、行の一覧、確かめる窓、知らせる窓など) のキーと描き方。

use chloe_audit::Record;
use chloe_secrets::Owner;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, ListItem, Paragraph, Wrap};
use terminput::{KeyCode, KeyEvent};

use super::app::{Action, App, render_list};
use super::form::{Form, FormEvent, RowKind};
use super::requests::{self, RequestFilter};
use super::{inbox, policy_form, secret_form, workspace_form};
use crate::ops::{RuleScope, owner_arg};

/// フォームを保存したときにすること。
#[derive(Clone, Debug)]
pub(super) enum Purpose {
    NewWorkspace,
    /// ワークスペースのディスクの大きさ。開いたときの大きさを持ち、変えた欄だけを送る。
    Disks {
        name: String,
        workspace_gib: u32,
        docker_gib: u32,
    },
    /// VM の大きさと停止までの時間。開いたときの値を持ち、変えた欄だけを送る。
    Machine {
        name: String,
        vcpus: u32,
        memory_mib: u32,
        idle: String,
    },
    Secret {
        owner: Owner,
        /// 登録したものを編集するなら、その名前。
        existing: Option<String>,
        builtin: bool,
    },
    Policy(Owner),
    Rules(RuleScope),
    NewToken,
    /// 質問への答え。選択肢があれば、フォームに選択肢の欄がある。
    AnswerAsk {
        id: i64,
        options: bool,
    },
    /// 依頼を断る。
    DenyAsk(i64),
    /// 下の行の一覧の、この番号の行。
    Row(usize),
}

pub(super) enum Modal {
    Form {
        form: Form,
        purpose: Purpose,
        /// 保存を頼んで、結果を待っている。
        saving: bool,
    },
    Rows {
        title: String,
        rows: Vec<Form>,
        kind: RowKind,
        /// 下のフォームの、この欄の行。
        field: usize,
        selected: usize,
    },
    PickTemplate {
        owner: Owner,
        selected: usize,
    },
    Confirm {
        message: String,
        action: Action,
    },
    /// ワークスペースを消すときに、ディスクも消すかを選ぶ。
    RemoveWorkspace {
        name: String,
    },
    Message {
        title: String,
        lines: Vec<String>,
    },
    /// ワークスペースの HTTP の要求のログ。読み終わるまで `records` は `None`。
    Requests {
        name: String,
        records: Option<Vec<Record>>,
        filter: RequestFilter,
        /// 絞り込んだ一覧で選んでいる行。
        selected: usize,
    },
}

impl App {
    /// いちばん上の窓でキーを受ける。
    pub(super) fn handle_modal_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(modal) = self.modals.last_mut() else {
            return vec![];
        };
        match modal {
            Modal::Form {
                form,
                purpose,
                saving,
            } => {
                if *saving {
                    return vec![];
                }
                match form.handle_key(key) {
                    FormEvent::None => {}
                    FormEvent::Cancel => {
                        self.modals.pop();
                    }
                    FormEvent::OpenRows(field) => {
                        if let Some((rows, kind, title)) = form.take_rows(field) {
                            self.modals.push(Modal::Rows {
                                title,
                                rows,
                                kind,
                                field,
                                selected: 0,
                            });
                        }
                    }
                    FormEvent::Save => {
                        if let Purpose::Row(index) = purpose {
                            let index = *index;
                            let Some(Modal::Form { form, .. }) = self.modals.pop() else {
                                return vec![];
                            };
                            if let Some(Modal::Rows { rows, selected, .. }) = self.modals.last_mut()
                            {
                                if index < rows.len() {
                                    rows[index] = form;
                                } else {
                                    rows.push(form);
                                }
                                *selected = index;
                            }
                            return vec![];
                        }
                        match submit(form, purpose) {
                            Ok(action) => {
                                form.error = None;
                                *saving = true;
                                return vec![action];
                            }
                            Err(message) => form.error = Some(message),
                        }
                    }
                }
            }
            Modal::Rows {
                rows,
                kind,
                selected,
                ..
            } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    if *selected + 1 < rows.len() {
                        *selected += 1;
                    }
                }
                KeyCode::Char('a' | 'n') => {
                    let index = rows.len();
                    let form = (kind.make)();
                    self.modals.push(Modal::Form {
                        form,
                        purpose: Purpose::Row(index),
                        saving: false,
                    });
                }
                KeyCode::Enter | KeyCode::Char('e') => {
                    if let Some(row) = rows.get(*selected) {
                        let form = row.clone();
                        let index = *selected;
                        self.modals.push(Modal::Form {
                            form,
                            purpose: Purpose::Row(index),
                            saving: false,
                        });
                    }
                }
                KeyCode::Char('d') | KeyCode::Delete => {
                    if *selected < rows.len() {
                        rows.remove(*selected);
                        *selected = (*selected).min(rows.len().saturating_sub(1));
                    }
                }
                KeyCode::Esc => {
                    let Some(Modal::Rows { rows, field, .. }) = self.modals.pop() else {
                        return vec![];
                    };
                    if let Some(Modal::Form { form, .. }) = self.modals.last_mut() {
                        form.put_rows(field, rows);
                    }
                }
                _ => {}
            },
            Modal::PickTemplate { owner, selected } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1).min(secret_form::TEMPLATES.len())
                }
                KeyCode::Enter => {
                    let owner = owner.clone();
                    let template = selected.checked_sub(1).map(|i| secret_form::TEMPLATES[i].0);
                    self.modals.pop();
                    self.modals.push(Modal::Form {
                        form: secret_form::new_form(&owner_arg(&owner), template),
                        purpose: Purpose::Secret {
                            owner,
                            existing: None,
                            builtin: false,
                        },
                        saving: false,
                    });
                }
                KeyCode::Esc => {
                    self.modals.pop();
                }
                _ => {}
            },
            Modal::Confirm { action, .. } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let action = action.clone();
                    self.modals.pop();
                    return vec![action];
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.modals.pop();
                }
                _ => {}
            },
            Modal::RemoveWorkspace { name } => match key.code {
                KeyCode::Char(c @ ('k' | 'd')) => {
                    let name = name.clone();
                    self.modals.pop();
                    return vec![Action::RemoveWorkspace {
                        name,
                        disks: c == 'd',
                    }];
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.modals.pop();
                }
                _ => {}
            },
            Modal::Message { .. } => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
                    self.modals.pop();
                }
            }
            Modal::Requests { .. } => return self.handle_requests_key(key),
        }
        vec![]
    }

    pub(super) fn close_saving_form(&mut self) {
        if matches!(self.modals.last(), Some(Modal::Form { saving: true, .. })) {
            self.modals.pop();
        }
    }
}

/// フォームを、保存する操作にする。
fn submit(form: &Form, purpose: &Purpose) -> Result<Action, String> {
    match purpose {
        Purpose::NewWorkspace => {
            let (name, options) = workspace_form::to_new(form)?;
            Ok(Action::CreateWorkspace { name, options })
        }
        Purpose::Machine {
            name,
            vcpus,
            memory_mib,
            idle,
        } => {
            let (machine, idle_stop) = workspace_form::to_machine(form, *vcpus, *memory_mib, idle)?;
            Ok(Action::SetMachine {
                name: name.clone(),
                machine,
                idle_stop,
            })
        }
        Purpose::Disks {
            name,
            workspace_gib,
            docker_gib,
        } => {
            let disks = workspace_form::to_disks(form, *workspace_gib, *docker_gib)?;
            Ok(Action::SetDisks {
                name: name.clone(),
                disks,
            })
        }
        Purpose::Secret {
            owner,
            existing,
            builtin,
        } => {
            let (name, input) = match (existing, builtin) {
                (Some(name), true) => (name.clone(), secret_form::value_input(form)?),
                (Some(name), false) => (name.clone(), secret_form::to_input(form)?),
                (None, _) => (secret_form::name(form), secret_form::to_input(form)?),
            };
            if name.is_empty() {
                return Err("enter a name".into());
            }
            Ok(Action::SaveSecret {
                owner: owner.clone(),
                name,
                input,
                keep_values: existing.is_some(),
            })
        }
        Purpose::Policy(owner) => Ok(Action::SaveSettings {
            owner: owner.clone(),
            settings: policy_form::to_layer(form)?,
        }),
        Purpose::Rules(scope) => Ok(Action::SaveRules {
            scope: scope.clone(),
            rules: policy_form::to_rules(form)?,
        }),
        Purpose::NewToken => {
            let name = form.text("name");
            if name.is_empty() {
                return Err("enter a name".into());
            }
            let expires = Some(form.text("expires")).filter(|e| !e.is_empty());
            Ok(Action::CreateToken { name, expires })
        }
        Purpose::AnswerAsk { id, options } => inbox::answer(form, *id, *options),
        Purpose::DenyAsk(id) => Ok(inbox::deny(form, *id)),
        Purpose::Row(_) => Err("not a top-level form".into()),
    }
}

pub(super) fn render_modal(frame: &mut Frame, modal: &Modal) {
    let area = frame.area();
    match modal {
        Modal::Form { form, saving, .. } => {
            let area = centered(area, 100, 30);
            form.render(frame, area);
            if *saving {
                frame.render_widget(
                    Paragraph::new(" Saving… ".yellow()),
                    Rect::new(area.x + 2, area.bottom().saturating_sub(1), 10, 1),
                );
            }
        }
        Modal::Rows {
            title,
            rows,
            kind,
            selected,
            ..
        } => {
            let area = centered(area, 90, 20);
            frame.render_widget(Clear, area);
            let block = Block::bordered().title(format!(" {title} "));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let [list_area, hint_area] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
            let items = rows
                .iter()
                .map(|r| ListItem::new((kind.summary)(r)))
                .collect();
            render_list(frame, list_area, items, *selected);
            frame.render_widget(
                Paragraph::new("a add  Enter edit  d delete  Esc done".dark_gray()),
                hint_area,
            );
        }
        Modal::PickTemplate { selected, .. } => {
            let area = centered(area, 50, secret_form::TEMPLATES.len() as u16 + 5);
            frame.render_widget(Clear, area);
            let block = Block::bordered().title(" New secret: start from ");
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let items = std::iter::once("(empty)")
                .chain(secret_form::TEMPLATES.iter().map(|(n, _)| *n))
                .map(ListItem::new)
                .collect();
            render_list(frame, inner, items, *selected);
        }
        Modal::Confirm { message, .. } => {
            let area = centered(area, 70, 5);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(message.clone()),
                    Line::default(),
                    Line::from("y yes  n no".dark_gray()),
                ])
                .wrap(Wrap { trim: true })
                .block(Block::bordered().title(" Confirm ")),
                area,
            );
        }
        Modal::RemoveWorkspace { name } => {
            let area = centered(area, 70, 6);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(format!("Delete {name}?")),
                    Line::from("Its disks hold /workspace, the home directory and Docker data."),
                    Line::default(),
                    Line::from("k delete, keep disks  d delete disks too  n cancel".dark_gray()),
                ])
                .wrap(Wrap { trim: true })
                .block(Block::bordered().title(" Delete workspace ")),
                area,
            );
        }
        Modal::Requests {
            name,
            records,
            filter,
            selected,
        } => requests::render(frame, name, records.as_deref(), *filter, *selected),
        Modal::Message { title, lines } => {
            let area = centered(area, 90, lines.len() as u16 + 4);
            frame.render_widget(Clear, area);
            let mut text: Vec<Line> = lines.iter().map(|l| Line::from(l.clone())).collect();
            text.push(Line::from("Enter close".dark_gray()));
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: false })
                    .block(Block::bordered().title(format!(" {title} "))),
                area,
            );
        }
    }
}

/// `area` の中央に、幅と高さが最大で `width` と `height` の領域をとる。
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(1);
    let height = height.min(area.height.saturating_sub(2)).max(1);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::super::app::tests::{app, form_mut, screen};
    use super::super::form::tests::{ctrl, key, type_text};
    use super::*;

    #[test]
    fn edits_rows_of_a_secret_from_a_template() {
        let mut app = app();
        app.handle_key(key(KeyCode::Char('2')));
        app.handle_key(key(KeyCode::Char('n')));
        // 雛形の一覧から anthropic_api_key を選ぶ。
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        assert!(screen(&app).contains("ANTHROPIC_API_KEY"));
        // 差し替えの規則の欄を開いて、規則を足す。
        let form = form_mut(&mut app);
        while form.fields[form.focus].key != "inject" {
            form.handle_key(key(KeyCode::Down));
        }
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('a')));
        type_text(form_mut(&mut app), "api.example.com");
        form_mut(&mut app).handle_key(key(KeyCode::Tab));
        type_text(form_mut(&mut app), "authorization");
        app.handle_key(ctrl('s'));
        app.handle_key(key(KeyCode::Esc));
        let form = form_mut(&mut app);
        assert_eq!(form.rows("inject").len(), 2);
        while form.fields[form.focus].key != "value" {
            form.handle_key(key(KeyCode::Down));
        }
        type_text(form, "sk-ant-real");
        let actions = app.handle_key(ctrl('s'));
        let [Action::SaveSecret { name, input, .. }] = &actions[..] else {
            panic!("expected a save");
        };
        assert_eq!(name, "anthropic_api_key");
        assert_eq!(input.inject.len(), 2);
        // 値は画面に出さない。
        assert!(!screen(&app).contains("sk-ant-real"));
    }

    #[test]
    fn confirms_before_deleting() {
        let mut app = app();
        assert!(app.handle_key(key(KeyCode::Char('d'))).is_empty());
        assert!(screen(&app).contains("Delete default?"));
        // y と Enter では消さない (ディスクを残すかを選ばせる)。
        assert!(app.handle_key(key(KeyCode::Enter)).is_empty());
        assert!(app.handle_key(key(KeyCode::Char('y'))).is_empty());
        let actions = app.handle_key(key(KeyCode::Char('k')));
        assert!(matches!(
            &actions[..],
            [Action::RemoveWorkspace { name, disks: false }] if name == "default"
        ));

        app.handle_key(key(KeyCode::Char('d')));
        let actions = app.handle_key(key(KeyCode::Char('d')));
        assert!(matches!(
            &actions[..],
            [Action::RemoveWorkspace { name, disks: true }] if name == "default"
        ));
    }
}
