//! 管理の TUI の状態と画面。
//!
//! キーを受けて状態を変え、ゲートウェイの操作が要るときは [`Action`] を返す。操作の結果は
//! [`Outcome`] として受け取る。操作そのものは [`super::perform`] が行う。

use chloe_controller::catalog::SecretInput;
use chloe_controller::runner::PolicyLayer;
use chloe_controller::store::{ApiToken, SshKey};
use chloe_controller::workspace::RootMode;
use chloe_secrets::Owner;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use terminput::{KeyCode, KeyEvent, KeyModifiers};

use super::form::{Field, Form, FormEvent, RowKind};
use super::{policy_form, secret_form};
use crate::ops::{SecretView, WorkspaceInfo, owner_arg};

/// ゲートウェイに頼む操作。シークレットの値を含むので、Debug で書き出さない。
#[derive(Clone)]
pub enum Action {
    LoadWorkspaces,
    CreateWorkspace {
        name: String,
        root: Option<RootMode>,
    },
    SetRoot {
        name: String,
        root: RootMode,
    },
    StopWorkspace(String),
    RemoveWorkspace {
        name: String,
        /// ランナーのホストにあるディスクも消す。
        disks: bool,
    },
    LoadOwners,
    LoadSecrets(Owner),
    ShowSecret {
        owner: Owner,
        name: String,
    },
    SaveSecret {
        owner: Owner,
        name: String,
        input: SecretInput,
        /// 値を書かなかった欄に、今の値を使う。
        keep_values: bool,
    },
    RemoveSecret {
        owner: Owner,
        name: String,
    },
    ShowPolicy(Owner),
    SavePolicy {
        owner: Owner,
        layer: PolicyLayer,
    },
    RemovePolicy(Owner),
    LoadTokens,
    CreateToken {
        name: String,
        expires: Option<String>,
    },
    RevokeToken(i64),
    LoadKeys,
    RemoveKey(String),
    GitHubStatus,
    GitHubLogin,
    GitHubLogout,
}

impl Action {
    /// 監査ログに残す名前。変更する操作だけが名前を持つ (値は含めない)。
    pub fn audit_name(&self) -> Option<&'static str> {
        Some(match self {
            Self::CreateWorkspace { .. } => "ws create",
            Self::SetRoot { .. } => "ws root",
            Self::StopWorkspace(_) => "ws stop",
            Self::RemoveWorkspace { .. } => "ws rm",
            Self::SaveSecret { .. } => "secret set",
            Self::RemoveSecret { .. } => "secret rm",
            Self::SavePolicy { .. } => "policy set",
            Self::RemovePolicy(_) => "policy rm",
            Self::CreateToken { .. } => "token create",
            Self::RevokeToken(_) => "token revoke",
            Self::RemoveKey(_) => "key rm",
            Self::GitHubLogin => "github login",
            Self::GitHubLogout => "github logout",
            _ => return None,
        })
    }
}

/// 操作の結果。作ったトークンを含むので、Debug で書き出さない。
pub enum Outcome {
    Workspaces(Vec<WorkspaceInfo>),
    Owners(Vec<Owner>),
    Secrets {
        owner: Owner,
        names: Vec<String>,
    },
    Secret {
        owner: Owner,
        view: SecretView,
    },
    Policy {
        owner: Owner,
        layer: Option<PolicyLayer>,
        inherited: Vec<(String, PolicyLayer)>,
    },
    Tokens(Vec<ApiToken>),
    TokenCreated {
        id: i64,
        name: String,
        token: String,
    },
    Keys(Vec<SshKey>),
    GitHub(String),
    /// Device Flow のコード。操作はまだ続く。
    DeviceCode {
        uri: String,
        code: String,
    },
    /// 終わった。`reload` で一覧を読み直す。
    Done {
        message: String,
        reload: Vec<Action>,
    },
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Workspaces,
    Secrets,
    Settings,
    Tokens,
    Keys,
    GitHub,
}

const TABS: [Tab; 6] = [
    Tab::Workspaces,
    Tab::Secrets,
    Tab::Settings,
    Tab::Tokens,
    Tab::Keys,
    Tab::GitHub,
];

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Self::Workspaces => "Workspaces",
            Self::Secrets => "Secrets",
            Self::Settings => "Org settings",
            Self::Tokens => "API tokens",
            Self::Keys => "SSH keys",
            Self::GitHub => "GitHub",
        }
    }

    fn hints(self) -> &'static str {
        match self {
            Self::Workspaces => "n new  r switch root  s stop  d delete",
            Self::Secrets => "o owner  n new  Enter edit  d delete",
            Self::Settings => "Enter edit  d delete",
            Self::Tokens => "n new  d revoke",
            Self::Keys => "d remove",
            Self::GitHub => "l sign in again  o sign out",
        }
    }
}

/// フォームを保存したときにすること。
#[derive(Clone, Debug)]
enum Purpose {
    NewWorkspace,
    Secret {
        owner: Owner,
        /// 登録したものを編集するなら、その名前。
        existing: Option<String>,
        builtin: bool,
    },
    Policy(Owner),
    NewToken,
    /// 下の行の一覧の、この番号の行。
    Row(usize),
}

enum Modal {
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
}

pub struct App {
    login: String,
    tab: usize,
    pub quit: bool,
    status: Option<(String, bool)>,
    /// 結果を待っている操作の数。
    busy: usize,
    workspaces: Vec<WorkspaceInfo>,
    owners: Vec<Owner>,
    secret_owner: usize,
    secrets: Vec<String>,
    tokens: Vec<ApiToken>,
    keys: Vec<SshKey>,
    github: String,
    /// タブごとの一覧で選んでいる行。
    selected: [usize; TABS.len()],
    modals: Vec<Modal>,
}

impl App {
    pub fn new(login: &str) -> Self {
        Self {
            login: login.to_string(),
            tab: 0,
            quit: false,
            status: None,
            busy: 0,
            workspaces: Vec::new(),
            owners: Vec::new(),
            secret_owner: 0,
            secrets: Vec::new(),
            tokens: Vec::new(),
            keys: Vec::new(),
            github: String::new(),
            selected: [0; TABS.len()],
            modals: Vec::new(),
        }
    }

    /// 始めに読むもの。
    pub fn start(&mut self) -> Vec<Action> {
        self.track(vec![
            Action::LoadWorkspaces,
            Action::LoadOwners,
            Action::LoadTokens,
            Action::LoadKeys,
            Action::GitHubStatus,
        ])
    }

    fn track(&mut self, actions: Vec<Action>) -> Vec<Action> {
        self.busy += actions.len();
        actions
    }

    fn current(&self) -> Tab {
        TABS[self.tab]
    }

    fn selection(&self) -> usize {
        self.selected[self.tab]
    }

    fn list_len(&self) -> usize {
        self.len_of(self.current())
    }

    fn len_of(&self, tab: Tab) -> usize {
        match tab {
            Tab::Workspaces => self.workspaces.len(),
            Tab::Secrets => self.secrets.len(),
            Tab::Settings => self.setting_owners().len(),
            Tab::Tokens => self.tokens.len(),
            Tab::Keys => self.keys.len(),
            Tab::GitHub => 0,
        }
    }

    /// 設定を持てる持ち主 (Organization と team)。
    fn setting_owners(&self) -> Vec<&Owner> {
        self.owners
            .iter()
            .filter(|o| !matches!(o, Owner::User(_)))
            .collect()
    }

    fn secret_owner(&self) -> Option<&Owner> {
        self.owners.get(self.secret_owner)
    }

    fn reload(&self) -> Vec<Action> {
        match self.current() {
            Tab::Workspaces => vec![Action::LoadWorkspaces],
            Tab::Secrets => match self.secret_owner() {
                Some(owner) => vec![Action::LoadSecrets(owner.clone())],
                None => vec![Action::LoadOwners],
            },
            Tab::Settings => vec![Action::LoadOwners],
            Tab::Tokens => vec![Action::LoadTokens],
            Tab::Keys => vec![Action::LoadKeys],
            Tab::GitHub => vec![Action::GitHubStatus],
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Action> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CTRL) {
            self.quit = true;
            return vec![];
        }
        let actions = if self.modals.is_empty() {
            self.handle_main_key(key)
        } else {
            self.handle_modal_key(key)
        };
        self.track(actions)
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(Modal::Form { form, .. }) = self.modals.last_mut() {
            form.paste(text);
        }
    }

    fn handle_main_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            // Esc はフォームなどを閉じるのに使うので、続けて押しても終わらないようにする。
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Tab if shift => self.tab = (self.tab + TABS.len() - 1) % TABS.len(),
            KeyCode::Left => self.tab = (self.tab + TABS.len() - 1) % TABS.len(),
            KeyCode::Tab | KeyCode::Right => self.tab = (self.tab + 1) % TABS.len(),
            KeyCode::Char(c @ '1'..='6') => self.tab = c as usize - '1' as usize,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected[self.tab] = self.selection().saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selection() + 1 < self.list_len() {
                    self.selected[self.tab] += 1;
                }
            }
            KeyCode::Char('R') => return self.reload(),
            _ => return self.handle_tab_key(key),
        }
        vec![]
    }

    fn handle_tab_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let selected = self.selection();
        match (self.current(), key.code) {
            (Tab::Workspaces, KeyCode::Char('n')) => self.modals.push(Modal::Form {
                form: Form::new(
                    "New workspace",
                    vec![
                        Field::text("name", "Name", "").help(
                            "name for a personal workspace, org/name or org/team/name for a shared one.",
                        ),
                        Field::choice(
                            "root",
                            "Root",
                            &["(settings default)", "ephemeral", "persistent"],
                            "(settings default)",
                        )
                        .help("persistent keeps changes to the root file system (apt install) across restarts."),
                    ],
                ),
                purpose: Purpose::NewWorkspace,
                saving: false,
            }),
            (Tab::Workspaces, KeyCode::Char('d')) => {
                if let Some(ws) = self.workspaces.get(selected) {
                    self.modals.push(Modal::RemoveWorkspace {
                        name: ws.name.clone(),
                    });
                }
            }
            (Tab::Workspaces, KeyCode::Char(c @ ('r' | 's'))) => {
                let Some(ws) = self.workspaces.get(selected) else {
                    return vec![];
                };
                let name = ws.name.clone();
                let (message, action) = match c {
                    'r' => {
                        let root = match ws.root {
                            RootMode::Ephemeral => RootMode::Persistent,
                            RootMode::Persistent => RootMode::Ephemeral,
                        };
                        (
                            format!("Use the {} root for {name} from the next start?", root.as_str()),
                            Action::SetRoot { name, root },
                        )
                    }
                    _ => (
                        format!("Stop {name}? Processes in the VM end; files are kept."),
                        Action::StopWorkspace(name),
                    ),
                };
                self.modals.push(Modal::Confirm { message, action });
            }
            (Tab::Secrets, KeyCode::Char('o')) if !self.owners.is_empty() => {
                self.secret_owner = (self.secret_owner + 1) % self.owners.len();
                self.selected[self.tab] = 0;
                self.secrets.clear();
                return self.reload();
            }
            (Tab::Secrets, KeyCode::Char('n')) => {
                if let Some(owner) = self.secret_owner().cloned() {
                    self.modals.push(Modal::PickTemplate { owner, selected: 0 });
                }
            }
            (Tab::Secrets, KeyCode::Enter | KeyCode::Char('e')) => {
                if let (Some(owner), Some(name)) =
                    (self.secret_owner().cloned(), self.secrets.get(selected))
                {
                    return vec![Action::ShowSecret {
                        owner,
                        name: name.clone(),
                    }];
                }
            }
            (Tab::Secrets, KeyCode::Char('d')) => {
                if let (Some(owner), Some(name)) =
                    (self.secret_owner().cloned(), self.secrets.get(selected))
                {
                    self.modals.push(Modal::Confirm {
                        message: format!("Delete the secret {name} of {}?", owner_arg(&owner)),
                        action: Action::RemoveSecret {
                            owner,
                            name: name.clone(),
                        },
                    });
                }
            }
            (Tab::Settings, KeyCode::Enter | KeyCode::Char('e')) => {
                if let Some(owner) = self.setting_owners().get(selected) {
                    return vec![Action::ShowPolicy((*owner).clone())];
                }
            }
            (Tab::Settings, KeyCode::Char('d')) => {
                if let Some(owner) = self.setting_owners().get(selected).map(|o| (*o).clone()) {
                    self.modals.push(Modal::Confirm {
                        message: format!("Delete the settings of {}?", owner_arg(&owner)),
                        action: Action::RemovePolicy(owner),
                    });
                }
            }
            (Tab::Tokens, KeyCode::Char('n')) => self.modals.push(Modal::Form {
                form: Form::new(
                    "New API token",
                    vec![
                        Field::text("name", "Name", "").help("What the token is for (1-64 characters)."),
                        Field::text("expires", "Expires in", "30d")
                            .help("e.g. 30d or 12h. Empty means no expiry."),
                    ],
                ),
                purpose: Purpose::NewToken,
                saving: false,
            }),
            (Tab::Tokens, KeyCode::Char('d')) => {
                if let Some(token) = self.tokens.get(selected) {
                    self.modals.push(Modal::Confirm {
                        message: format!("Revoke the token {} ({})?", token.id, token.name),
                        action: Action::RevokeToken(token.id),
                    });
                }
            }
            (Tab::Keys, KeyCode::Char('d')) => {
                if let Some(key) = self.keys.get(selected) {
                    self.modals.push(Modal::Confirm {
                        message: format!(
                            "Remove {}? The key can no longer sign in with the shared user name.",
                            key.fingerprint
                        ),
                        action: Action::RemoveKey(key.fingerprint.clone()),
                    });
                }
            }
            (Tab::GitHub, KeyCode::Char('l')) => return vec![Action::GitHubLogin],
            (Tab::GitHub, KeyCode::Char('o')) => self.modals.push(Modal::Confirm {
                message: "Sign out of GitHub? You need the Device Flow again on the next connection."
                    .into(),
                action: Action::GitHubLogout,
            }),
            _ => {}
        }
        vec![]
    }

    fn handle_modal_key(&mut self, key: KeyEvent) -> Vec<Action> {
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
        }
        vec![]
    }

    /// 結果を受ける。続けて読むものがあれば返す。
    pub fn apply(&mut self, outcome: Outcome) -> Vec<Action> {
        if !matches!(outcome, Outcome::DeviceCode { .. }) {
            self.busy = self.busy.saturating_sub(1);
        }
        let mut actions = Vec::new();
        match outcome {
            Outcome::Workspaces(workspaces) => self.workspaces = workspaces,
            Outcome::Owners(owners) => {
                let current = self.secret_owner().cloned();
                self.owners = owners;
                self.secret_owner = current
                    .and_then(|c| self.owners.iter().position(|o| *o == c))
                    .unwrap_or(0);
                if let Some(owner) = self.secret_owner() {
                    actions.push(Action::LoadSecrets(owner.clone()));
                }
            }
            Outcome::Secrets { owner, names } => {
                if self.secret_owner() == Some(&owner) {
                    self.secrets = names;
                }
            }
            Outcome::Secret { owner, view } => {
                let owner_name = owner_arg(&owner);
                let form = if view.builtin {
                    secret_form::value_form(&owner_name, &view.name)
                } else {
                    secret_form::edit_form(&owner_name, &view.name, &view.spec)
                };
                self.modals.push(Modal::Form {
                    form,
                    purpose: Purpose::Secret {
                        owner,
                        existing: Some(view.name),
                        builtin: view.builtin,
                    },
                    saving: false,
                });
            }
            Outcome::Policy {
                owner,
                layer,
                inherited,
            } => self.modals.push(Modal::Form {
                form: policy_form::form(&owner_arg(&owner), layer.as_ref(), &inherited),
                purpose: Purpose::Policy(owner),
                saving: false,
            }),
            Outcome::Tokens(tokens) => self.tokens = tokens,
            Outcome::TokenCreated { id, name, token } => {
                self.close_saving_form();
                self.modals.push(Modal::Message {
                    title: format!("Token {id} ({name})"),
                    lines: vec![
                        token,
                        String::new(),
                        "It is shown only now. Use it as `Authorization: Bearer <token>`.".into(),
                    ],
                });
                actions.push(Action::LoadTokens);
            }
            Outcome::Keys(keys) => self.keys = keys,
            Outcome::GitHub(status) => self.github = status,
            Outcome::DeviceCode { uri, code } => self.modals.push(Modal::Message {
                title: "Sign in to GitHub".into(),
                lines: vec![
                    format!("Sign in to GitHub as {}:", self.login),
                    format!("  1. Open {uri}"),
                    format!("  2. Enter the code {code}"),
                    String::new(),
                    "Waiting for approval…".into(),
                ],
            }),
            Outcome::Done { message, reload } => {
                self.close_saving_form();
                if message.starts_with("Signed in")
                    && matches!(self.modals.last(), Some(Modal::Message { .. }))
                {
                    self.modals.pop();
                }
                self.status = Some((message, false));
                actions = reload;
            }
            Outcome::Failed(message) => match self.modals.last_mut() {
                Some(Modal::Form {
                    form,
                    saving: saving @ true,
                    ..
                }) => {
                    *saving = false;
                    form.error = Some(message);
                }
                _ => self.status = Some((message, true)),
            },
        }
        for (list, tab) in TABS.iter().enumerate() {
            let last = self.len_of(*tab).saturating_sub(1);
            self.selected[list] = self.selected[list].min(last);
        }
        self.track(actions)
    }

    fn close_saving_form(&mut self) {
        if matches!(self.modals.last(), Some(Modal::Form { saving: true, .. })) {
            self.modals.pop();
        }
    }

    pub fn render(&self, frame: &mut Frame) {
        let [tabs_area, body, status_area, hints_area] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let titles: Vec<String> = TABS
            .iter()
            .enumerate()
            .map(|(i, t)| format!("{} {}", i + 1, t.title()))
            .collect();
        frame.render_widget(
            Tabs::new(titles)
                .select(self.tab)
                .highlight_style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                .block(Block::new().title(format!(" chloe — {} ", self.login).bold())),
            tabs_area,
        );
        self.render_tab(frame, body);
        let mut status = match &self.status {
            Some((message, true)) => Line::from(message.clone().red()),
            Some((message, false)) => Line::from(message.clone().green()),
            None => Line::default(),
        };
        if self.busy > 0 {
            status.push_span("  working…".yellow());
        }
        frame.render_widget(Paragraph::new(status), status_area);
        frame.render_widget(
            Paragraph::new(
                format!(
                    "{}  ↑↓ select  ←→ tab  R reload  q quit",
                    self.current().hints()
                )
                .dark_gray(),
            ),
            hints_area,
        );
        for modal in &self.modals {
            render_modal(frame, modal);
        }
    }

    fn render_tab(&self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(format!(" {} ", self.current().title()));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let items: Vec<ListItem> = match self.current() {
            Tab::Workspaces => self
                .workspaces
                .iter()
                .map(|ws| {
                    let state = if ws.running { "running" } else { "stopped" };
                    ListItem::new(format!("{:<32} {:<11} {state}", ws.name, ws.root.as_str()))
                })
                .collect(),
            Tab::Secrets => {
                let [owner_area, list_area] =
                    Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
                let owner = self
                    .secret_owner()
                    .map(owner_arg)
                    .unwrap_or_else(|| "…".into());
                frame.render_widget(
                    Paragraph::new(format!("Owner: {owner}  (o: next owner)")),
                    owner_area,
                );
                let items: Vec<ListItem> = self
                    .secrets
                    .iter()
                    .map(|n| ListItem::new(n.clone()))
                    .collect();
                render_list(frame, list_area, items, self.selection());
                return;
            }
            Tab::Settings => self
                .setting_owners()
                .iter()
                .map(|o| ListItem::new(owner_arg(o)))
                .collect(),
            Tab::Tokens => self
                .tokens
                .iter()
                .map(|t| {
                    let expires = t
                        .expires_at
                        .map(|at| format!("expires {}", format_time(at)))
                        .unwrap_or_else(|| "no expiry".into());
                    let used = t
                        .last_used_at
                        .map(|at| format!("used {}", format_time(at)))
                        .unwrap_or_else(|| "never used".into());
                    ListItem::new(format!("{:<5} {:<24} {expires:<26} {used}", t.id, t.name))
                })
                .collect(),
            Tab::Keys => self
                .keys
                .iter()
                .map(|k| {
                    let kind = k.public_key.split_whitespace().next().unwrap_or("");
                    let used = k
                        .last_used_at
                        .map(|at| format!("used {}", format_time(at)))
                        .unwrap_or_else(|| "never used".into());
                    ListItem::new(format!(
                        "{} {kind}  added {}  {used}",
                        k.fingerprint,
                        format_time(k.created_at)
                    ))
                })
                .collect(),
            Tab::GitHub => {
                frame.render_widget(
                    Paragraph::new(self.github.clone()).wrap(Wrap { trim: true }),
                    inner,
                );
                return;
            }
        };
        render_list(frame, inner, items, self.selection());
    }
}

/// フォームを、保存する操作にする。
fn submit(form: &Form, purpose: &Purpose) -> Result<Action, String> {
    match purpose {
        Purpose::NewWorkspace => {
            let name = form.text("name");
            if name.is_empty() {
                return Err("enter a name".into());
            }
            let root = match form.choice("root").as_str() {
                "ephemeral" => Some(RootMode::Ephemeral),
                "persistent" => Some(RootMode::Persistent),
                _ => None,
            };
            Ok(Action::CreateWorkspace { name, root })
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
        Purpose::Policy(owner) => Ok(Action::SavePolicy {
            owner: owner.clone(),
            layer: policy_form::to_layer(form)?,
        }),
        Purpose::NewToken => {
            let name = form.text("name");
            if name.is_empty() {
                return Err("enter a name".into());
            }
            let expires = Some(form.text("expires")).filter(|e| !e.is_empty());
            Ok(Action::CreateToken { name, expires })
        }
        Purpose::Row(_) => Err("not a top-level form".into()),
    }
}

fn render_list(frame: &mut Frame, area: Rect, items: Vec<ListItem>, selected: usize) {
    let empty = items.is_empty();
    let list = List::new(items)
        .highlight_style(Style::new().reversed())
        .highlight_symbol("› ");
    let mut state = ListState::default().with_selected((!empty).then_some(selected));
    frame.render_stateful_widget(list, area, &mut state);
    if empty {
        frame.render_widget(Paragraph::new("(none)".dark_gray()), area);
    }
}

fn render_modal(frame: &mut Frame, modal: &Modal) {
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

/// UNIX 時刻の秒を `YYYY-MM-DD HH:MM` (UTC) にする。
pub fn format_time(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    // 1970-01-01 からの日数を、グレゴリオ暦の日付にする。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::super::form::tests::{ctrl, key, type_text};
    use super::*;

    fn screen(app: &App) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
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

    fn app() -> App {
        let mut app = App::new("alice");
        let actions = app.start();
        assert_eq!(actions.len(), 5);
        app.apply(Outcome::Workspaces(vec![WorkspaceInfo {
            name: "default".into(),
            root: RootMode::Ephemeral,
            running: true,
        }]));
        let more = app.apply(Outcome::Owners(vec![
            Owner::User("alice".into()),
            Owner::Org("acme".into()),
        ]));
        assert!(matches!(&more[..], [Action::LoadSecrets(Owner::User(_))]));
        app.apply(Outcome::Secrets {
            owner: Owner::User("alice".into()),
            names: vec!["openai_api_key".into()],
        });
        app.apply(Outcome::Tokens(vec![]));
        app.apply(Outcome::Keys(vec![]));
        app.apply(Outcome::GitHub("signed in".into()));
        app
    }

    #[test]
    fn lists_workspaces_and_switches_tabs() {
        let mut app = app();
        assert!(screen(&app).contains("default"));
        app.handle_key(key(KeyCode::Right));
        assert!(screen(&app).contains("openai_api_key"));
        // 持ち主を切り替えると、その持ち主のシークレットを読む。
        let actions = app.handle_key(key(KeyCode::Char('o')));
        assert!(matches!(&actions[..], [Action::LoadSecrets(Owner::Org(o))] if o == "acme"));
        app.handle_key(key(KeyCode::Char('6')));
        assert!(screen(&app).contains("signed in"));
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.quit);
    }

    #[test]
    fn creates_a_workspace_with_a_form() {
        let mut app = app();
        app.handle_key(key(KeyCode::Char('n')));
        type_text(form_mut(&mut app), "api");
        form_mut(&mut app).handle_key(key(KeyCode::Tab));
        form_mut(&mut app).handle_key(key(KeyCode::Right));
        let actions = app.handle_key(ctrl('s'));
        assert!(matches!(
            &actions[..],
            [Action::CreateWorkspace { name, root: Some(RootMode::Ephemeral) }] if name == "api"
        ));
        // 失敗すると、フォームに理由を出して開いたままにする。
        app.apply(Outcome::Failed("already exists: api".into()));
        assert!(screen(&app).contains("already exists: api"));
        app.handle_key(ctrl('s'));
        app.apply(Outcome::Done {
            message: "created api".into(),
            reload: vec![Action::LoadWorkspaces],
        });
        assert!(app.modals.is_empty());
        assert!(screen(&app).contains("created api"));
    }

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

    #[test]
    fn formats_times() {
        assert_eq!(format_time(0), "1970-01-01 00:00");
        assert_eq!(format_time(1_792_152_000), "2026-10-16 12:00");
    }

    fn form_mut(app: &mut App) -> &mut Form {
        match app.modals.last_mut() {
            Some(Modal::Form { form, .. }) => form,
            _ => panic!("no form"),
        }
    }
}
