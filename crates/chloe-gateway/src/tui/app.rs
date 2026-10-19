//! 管理の TUI の状態と画面。
//!
//! キーを受けて状態を変え、ゲートウェイの操作が要るときは [`Action`] を返す。操作の結果は
//! [`Outcome`] として受け取る。操作そのものは [`super::perform`] が行う。

use chloe_audit::Record;
use chloe_controller::catalog::SecretInput;
use chloe_controller::runner::PolicyLayer;
use chloe_controller::store::{ApiToken, SshKey};
use chloe_controller::workspace::{DiskSizes, IdleStop, MachineSize, RootMode};
use chloe_policy::PathRuleConfig;
use chloe_secrets::Owner;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use terminput::{KeyCode, KeyEvent, KeyModifiers};

use super::form::{Field, Form};
use super::modal::{Modal, Purpose, render_modal};
use super::requests::RequestFilter;
use super::{policy_form, secret_form, workspace_form};
use crate::ops::{
    AskInfo, NewWorkspace, Reply, RuleScope, RulesView, ScopeEntry, SecretView, WorkspaceInfo,
    format_idle, format_mib, format_time, owner_arg,
};

/// ゲートウェイに頼む操作。シークレットの値を含むので、Debug で書き出さない。
#[derive(Clone)]
pub enum Action {
    LoadWorkspaces,
    CreateWorkspace {
        name: String,
        options: NewWorkspace,
    },
    SetDisks {
        name: String,
        disks: DiskSizes,
    },
    /// VM の大きさと停止までの時間。変えたものだけを持つ。
    SetMachine {
        name: String,
        machine: MachineSize,
        idle_stop: Option<IdleStop>,
    },
    SetRoot {
        name: String,
        root: RootMode,
    },
    StopWorkspace(String),
    /// ワークスペースの HTTP の要求のログの末尾を読む。
    LoadRequests(String),
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
    /// 持ち主の設定のうち、規則のほかを置き換える (規則は残す)。
    SaveSettings {
        owner: Owner,
        settings: PolicyLayer,
    },
    /// 持ち主の設定のうち、規則のほかを消す (規則は残す)。
    ResetSettings(Owner),
    LoadRuleScopes,
    ShowRules(RuleScope),
    SaveRules {
        scope: RuleScope,
        rules: Vec<PathRuleConfig>,
    },
    ClearRules(RuleScope),
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
    /// エージェントからの依頼。`all` なら答えたものも。
    LoadAsks {
        all: bool,
    },
    AnswerAsk {
        id: i64,
        reply: Reply,
    },
}

impl Action {
    /// 監査ログに残す名前。変更する操作だけが名前を持つ (値は含めない)。
    pub fn audit_name(&self) -> Option<&'static str> {
        Some(match self {
            Self::CreateWorkspace { .. } => "ws create",
            Self::SetRoot { .. } => "ws root",
            Self::SetDisks { .. } => "ws disk",
            Self::SetMachine { .. } => "ws machine",
            Self::StopWorkspace(_) => "ws stop",
            Self::RemoveWorkspace { .. } => "ws rm",
            Self::SaveSecret { .. } => "secret set",
            Self::RemoveSecret { .. } => "secret rm",
            Self::SaveSettings { .. } | Self::ResetSettings(_) => "policy set",
            Self::SaveRules { .. } => "rules set",
            Self::ClearRules(_) => "rules rm",
            Self::CreateToken { .. } => "token create",
            Self::RevokeToken(_) => "token revoke",
            Self::RemoveKey(_) => "key rm",
            Self::GitHubLogin => "github login",
            Self::GitHubLogout => "github logout",
            Self::AnswerAsk { .. } => "inbox answer",
            _ => return None,
        })
    }
}

/// 操作の結果。作ったトークンを含むので、Debug で書き出さない。
pub enum Outcome {
    Workspaces(Vec<WorkspaceInfo>),
    Requests {
        name: String,
        records: Vec<Record>,
    },
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
    RuleScopes(Vec<ScopeEntry>),
    Rules(RulesView),
    Tokens(Vec<ApiToken>),
    TokenCreated {
        id: i64,
        name: String,
        token: String,
    },
    Keys(Vec<SshKey>),
    GitHub(String),
    Asks {
        all: bool,
        asks: Vec<AskInfo>,
    },
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
    Rules,
    Settings,
    Tokens,
    Keys,
    GitHub,
    Inbox,
}

const TABS: [Tab; 8] = [
    Tab::Workspaces,
    Tab::Secrets,
    Tab::Rules,
    Tab::Settings,
    Tab::Tokens,
    Tab::Keys,
    Tab::GitHub,
    Tab::Inbox,
];

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Self::Workspaces => "Workspaces",
            Self::Secrets => "Secrets",
            Self::Rules => "Rules",
            Self::Settings => "Settings",
            Self::Tokens => "API tokens",
            Self::Keys => "SSH keys",
            Self::GitHub => "GitHub",
            Self::Inbox => "Inbox",
        }
    }

    fn hints(self) -> &'static str {
        match self {
            Self::Workspaces => {
                "n new  r switch root  g disks  m machine  u rules  l log  s stop  d delete"
            }
            Self::Secrets => "o owner  n new  Enter edit  d delete",
            Self::Rules => "Enter edit rules  d delete rules",
            Self::Settings => "Enter edit  d reset",
            Self::Tokens => "n new  d revoke",
            Self::Keys => "d remove",
            Self::GitHub => "l sign in again  o sign out",
            Self::Inbox => "Enter details  a allow/answer  d deny  f pending/all",
        }
    }
}

pub struct App {
    login: String,
    pub(super) tab: usize,
    pub quit: bool,
    pub(super) status: Option<(String, bool)>,
    /// 結果を待っている操作の数。
    busy: usize,
    workspaces: Vec<WorkspaceInfo>,
    owners: Vec<Owner>,
    secret_owner: usize,
    secrets: Vec<String>,
    /// Rules タブのスコープの一覧と、選んでいるスコープで効く規則。
    pub(super) scopes: Vec<ScopeEntry>,
    pub(super) rules: Option<RulesView>,
    tokens: Vec<ApiToken>,
    keys: Vec<SshKey>,
    github: String,
    /// エージェントからの依頼と、答えたものも出しているか。
    pub(super) asks: Vec<AskInfo>,
    pub(super) all_asks: bool,
    /// 依頼を一度でも読んだか (始めに読んだ依頼は、新しく来たとは知らせない)。
    pub(super) asks_loaded: bool,
    /// タブごとの一覧で選んでいる行。
    pub(super) selected: [usize; TABS.len()],
    pub(super) modals: Vec<Modal>,
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
            scopes: Vec::new(),
            rules: None,
            tokens: Vec::new(),
            keys: Vec::new(),
            github: String::new(),
            asks: Vec::new(),
            all_asks: false,
            asks_loaded: false,
            selected: [0; TABS.len()],
            modals: Vec::new(),
        }
    }

    /// 始めに読むもの。
    pub fn start(&mut self) -> Vec<Action> {
        self.track(vec![
            Action::LoadWorkspaces,
            Action::LoadOwners,
            Action::LoadRuleScopes,
            Action::LoadTokens,
            Action::LoadKeys,
            Action::GitHubStatus,
            Action::LoadAsks { all: false },
        ])
    }

    pub(super) fn track(&mut self, actions: Vec<Action>) -> Vec<Action> {
        self.busy += actions.len();
        actions
    }

    pub(super) fn current(&self) -> Tab {
        TABS[self.tab]
    }

    pub(super) fn selection(&self) -> usize {
        self.selected[self.tab]
    }

    fn list_len(&self) -> usize {
        self.len_of(self.current())
    }

    fn len_of(&self, tab: Tab) -> usize {
        match tab {
            Tab::Workspaces => self.workspaces.len(),
            Tab::Secrets => self.secrets.len(),
            Tab::Rules => self.scopes.len(),
            Tab::Settings => self.owners.len(),
            Tab::Tokens => self.tokens.len(),
            Tab::Keys => self.keys.len(),
            Tab::GitHub => 0,
            Tab::Inbox => self.asks.len(),
        }
    }

    fn secret_owner(&self) -> Option<&Owner> {
        self.owners.get(self.secret_owner)
    }

    pub(super) fn reload(&self) -> Vec<Action> {
        match self.current() {
            Tab::Workspaces => vec![Action::LoadWorkspaces],
            Tab::Secrets => match self.secret_owner() {
                Some(owner) => vec![Action::LoadSecrets(owner.clone())],
                None => vec![Action::LoadOwners],
            },
            Tab::Rules => vec![Action::LoadRuleScopes],
            Tab::Settings => vec![Action::LoadOwners],
            Tab::Tokens => vec![Action::LoadTokens],
            Tab::Keys => vec![Action::LoadKeys],
            Tab::GitHub => vec![Action::GitHubStatus],
            Tab::Inbox => vec![Action::LoadAsks { all: self.all_asks }],
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
        let before = (self.tab, self.selection());
        let actions = self.handle_move_key(key);
        // Rules タブでスコープを選んだら、そのスコープで効く規則を読む。
        if self.current() == Tab::Rules && (self.tab, self.selection()) != before {
            return self.show_selected_rules();
        }
        actions
    }

    fn handle_move_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            // Esc はフォームなどを閉じるのに使うので、続けて押しても終わらないようにする。
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Tab if shift => self.tab = (self.tab + TABS.len() - 1) % TABS.len(),
            KeyCode::Left => self.tab = (self.tab + TABS.len() - 1) % TABS.len(),
            KeyCode::Tab | KeyCode::Right => self.tab = (self.tab + 1) % TABS.len(),
            KeyCode::Char(c @ '1'..='8') => self.tab = c as usize - '1' as usize,
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
                form: workspace_form::new_form(),
                purpose: Purpose::NewWorkspace,
                saving: false,
            }),
            (Tab::Workspaces, KeyCode::Char('g')) => {
                if let Some(ws) = self.workspaces.get(selected) {
                    self.modals.push(Modal::Form {
                        form: workspace_form::disks_form(ws),
                        purpose: Purpose::Disks {
                            name: ws.name.clone(),
                            workspace_gib: ws.workspace_disk_gib,
                            docker_gib: ws.docker_disk_gib,
                        },
                        saving: false,
                    });
                }
            }
            (Tab::Workspaces, KeyCode::Char('m')) => {
                if let Some(ws) = self.workspaces.get(selected) {
                    let idle = format_idle(ws.idle_timeout_secs);
                    self.modals.push(Modal::Form {
                        form: workspace_form::machine_form(ws, &idle),
                        purpose: Purpose::Machine {
                            name: ws.name.clone(),
                            vcpus: ws.vcpus,
                            memory_mib: ws.memory_mib,
                            idle,
                        },
                        saving: false,
                    });
                }
            }
            (Tab::Workspaces, KeyCode::Char('u')) => {
                let Some(ws) = self.workspaces.get(selected) else {
                    return vec![];
                };
                let found = self.scopes.iter().position(
                    |e| matches!(&e.scope, RuleScope::Workspace(w) if w.display_name() == ws.name),
                );
                match found {
                    Some(index) => {
                        self.tab = tab_index(Tab::Rules);
                        self.selected[self.tab] = index;
                        return self.show_selected_rules();
                    }
                    None => self.status = Some((format!("no rules found for {}", ws.name), true)),
                }
            }
            (Tab::Workspaces, KeyCode::Char('l')) => {
                if let Some(ws) = self.workspaces.get(selected) {
                    self.modals.push(Modal::Requests {
                        name: ws.name.clone(),
                        records: None,
                        filter: RequestFilter::All,
                        selected: 0,
                    });
                    return vec![Action::LoadRequests(ws.name.clone())];
                }
            }
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
                            format!(
                                "Use the {} root for {name} from the next start?",
                                root.as_str()
                            ),
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
            (Tab::Rules, _) => return self.handle_rules_key(key),
            (Tab::Settings, KeyCode::Enter | KeyCode::Char('e')) => {
                if let Some(owner) = self.owners.get(selected) {
                    return vec![Action::ShowPolicy(owner.clone())];
                }
            }
            (Tab::Settings, KeyCode::Char('d')) => {
                if let Some(owner) = self.owners.get(selected).cloned() {
                    self.modals.push(Modal::Confirm {
                        message: format!(
                            "Reset the settings of {}? Its rules are kept.",
                            owner_arg(&owner)
                        ),
                        action: Action::ResetSettings(owner),
                    });
                }
            }
            (Tab::Tokens, KeyCode::Char('n')) => self.modals.push(Modal::Form {
                form: Form::new(
                    "New API token",
                    vec![
                        Field::text("name", "Name", "")
                            .help("What the token is for (1-64 characters)."),
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
            (Tab::Inbox, _) => return self.handle_inbox_key(key),
            (Tab::GitHub, KeyCode::Char('l')) => return vec![Action::GitHubLogin],
            (Tab::GitHub, KeyCode::Char('o')) => self.modals.push(Modal::Confirm {
                message:
                    "Sign out of GitHub? You need the Device Flow again on the next connection."
                        .into(),
                action: Action::GitHubLogout,
            }),
            _ => {}
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
            Outcome::Requests { name, records } => self.apply_requests(name, records),
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
                form: policy_form::form(
                    &owner_arg(&owner),
                    matches!(owner, Owner::User(_)),
                    layer.as_ref(),
                    &inherited,
                ),
                purpose: Purpose::Policy(owner),
                saving: false,
            }),
            Outcome::RuleScopes(scopes) => actions = self.apply_rule_scopes(scopes),
            Outcome::Rules(view) => self.apply_rules(view),
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
            Outcome::Asks { all, asks } => self.apply_asks(all, asks),
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
                Some(Modal::Requests { records, .. }) => {
                    records.get_or_insert_with(Vec::new);
                    self.status = Some((message, true));
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
            .map(|(i, t)| match (t, self.pending_asks()) {
                (Tab::Inbox, n) if n > 0 => format!("{} {} ({n})", i + 1, t.title()),
                _ => format!("{} {}", i + 1, t.title()),
            })
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
                    ListItem::new(format!(
                        "{:<32} {:<11} {state:<8} {:<10} {:>2} vCPU {:>5}  idle {}",
                        ws.name,
                        ws.root.as_str(),
                        format!("{}G/{}G", ws.workspace_disk_gib, ws.docker_disk_gib),
                        ws.vcpus,
                        format_mib(ws.memory_mib),
                        format_idle(ws.idle_timeout_secs)
                    ))
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
            Tab::Rules => {
                self.render_rules(frame, inner);
                return;
            }
            Tab::Settings => self
                .owners
                .iter()
                .map(|o| ListItem::new(format!("{:<32} {}", owner_arg(o), owner_kind(o))))
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
            Tab::Inbox => {
                self.render_inbox(frame, inner);
                return;
            }
        };
        render_list(frame, inner, items, self.selection());
    }
}

pub(super) fn tab_index(tab: Tab) -> usize {
    TABS.iter().position(|t| *t == tab).unwrap_or(0)
}

pub(super) fn owner_kind(owner: &Owner) -> &'static str {
    match owner {
        Owner::User(_) => "personal",
        Owner::Org(_) => "org",
        Owner::Team { .. } => "team",
    }
}

pub(super) fn render_list(frame: &mut Frame, area: Rect, items: Vec<ListItem>, selected: usize) {
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

#[cfg(test)]
pub(crate) mod tests {
    use super::super::form::tests::key;
    use super::*;

    pub fn screen(app: &App) -> String {
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

    pub fn app() -> App {
        let mut app = App::new("alice");
        let actions = app.start();
        assert_eq!(actions.len(), 7);
        app.apply(Outcome::Workspaces(vec![WorkspaceInfo {
            name: "default".into(),
            root: RootMode::Ephemeral,
            running: true,
            workspace_disk_gib: 20,
            docker_disk_gib: 40,
            vcpus: 4,
            memory_mib: 4096,
            idle_timeout_secs: Some(1800),
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
        let more = app.apply(Outcome::RuleScopes(scopes()));
        // 一覧を読んだら、先頭のスコープ (gateway) の規則を読む。
        assert!(matches!(&more[..], [Action::ShowRules(RuleScope::Gateway)]));
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
        app.handle_key(key(KeyCode::Char('7')));
        assert!(screen(&app).contains("signed in"));
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.quit);
    }

    fn workspace(id: i64, owner: Owner, name: &str) -> chloe_controller::workspace::Workspace {
        chloe_controller::workspace::Workspace {
            id,
            owner,
            name: name.into(),
            root_mode: RootMode::Ephemeral,
            disks: DiskSizes::default(),
            machine: MachineSize::default(),
            idle_stop: IdleStop::Default,
        }
    }

    fn scopes() -> Vec<ScopeEntry> {
        let acme = Owner::Org("acme".into());
        [
            (RuleScope::Gateway, false),
            (RuleScope::Owner(Owner::User("alice".into())), true),
            (
                RuleScope::Workspace(workspace(1, Owner::User("alice".into()), "default")),
                true,
            ),
            (RuleScope::Owner(acme.clone()), false),
            (RuleScope::Workspace(workspace(2, acme, "web")), true),
        ]
        .into_iter()
        .map(|(scope, editable)| ScopeEntry { scope, editable })
        .collect()
    }

    pub fn form_mut(app: &mut App) -> &mut Form {
        match app.modals.last_mut() {
            Some(Modal::Form { form, .. }) => form,
            _ => panic!("no form"),
        }
    }
}
