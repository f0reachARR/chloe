//! 管理の TUI の状態と画面。
//!
//! キーを受けて状態を変え、ゲートウェイの操作が要るときは [`Action`] を返す。操作の結果は
//! [`Outcome`] として受け取る。操作そのものは [`super::perform`] が行う。

use chloe_audit::Record;
use chloe_controller::catalog::SecretInput;
use chloe_controller::runner::{PolicyLayer, RuleOrigin};
use chloe_controller::store::{ApiToken, SshKey};
use chloe_controller::workspace::{DiskSizes, IdleStop, MachineSize, RootMode};
use chloe_policy::{Action as RuleAction, PathRuleConfig};
use chloe_secrets::Owner;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use terminput::{KeyCode, KeyEvent, KeyModifiers};

use super::form::{Field, Form, FormEvent, RowKind};
use super::requests::{self, RequestFilter};
use super::{inbox, policy_form, secret_form};
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
    scopes: Vec<ScopeEntry>,
    rules: Option<RulesView>,
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

    fn current(&self) -> Tab {
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

    /// Rules タブで選んでいるスコープ。
    fn selected_scope(&self) -> Option<&ScopeEntry> {
        self.scopes.get(self.selected[tab_index(Tab::Rules)])
    }

    /// 選んでいるスコープで効く規則を、まだ読んでいなければ読む。
    fn show_selected_rules(&self) -> Vec<Action> {
        match self.selected_scope() {
            Some(entry) if self.rules.as_ref().map(|r| &r.scope) != Some(&entry.scope) => {
                vec![Action::ShowRules(entry.scope.clone())]
            }
            _ => vec![],
        }
    }

    /// 読んである、選んでいるスコープの規則。
    fn selected_rules(&self) -> Option<&RulesView> {
        let entry = self.selected_scope()?;
        self.rules.as_ref().filter(|r| r.scope == entry.scope)
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
                        Field::text("workspace", "Workspace disk", "")
                            .help("in GiB, like 50G. Empty for the default. Holds /workspace and the home directory."),
                        Field::text("docker", "Docker disk", "")
                            .help("in GiB, like 80G. Empty for the default. Holds Docker images and containers."),
                        Field::text("cpus", "vCPUs", "")
                            .help("the number of vCPUs (1-32). Empty for the default."),
                        Field::text("memory", "Memory", "")
                            .help("like 8G or 512M. Empty for the default."),
                        Field::text("idle", "Idle stop", "")
                            .help("stops the VM this long after the last connection closes, like 30m or 2h. never keeps it running. Empty for the default."),
                    ],
                ),
                purpose: Purpose::NewWorkspace,
                saving: false,
            }),
            (Tab::Workspaces, KeyCode::Char('g')) => {
                if let Some(ws) = self.workspaces.get(selected) {
                    let help = "in GiB. Disks only grow; the new size applies from the next start.";
                    self.modals.push(Modal::Form {
                        form: Form::new(
                            format!("Disks of {}", ws.name),
                            vec![
                                Field::text(
                                    "workspace",
                                    "Workspace disk",
                                    &format!("{}G", ws.workspace_disk_gib),
                                )
                                .help(help),
                                Field::text(
                                    "docker",
                                    "Docker disk",
                                    &format!("{}G", ws.docker_disk_gib),
                                )
                                .help(help),
                            ],
                        ),
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
                    let next = "The new size applies from the next start.";
                    self.modals.push(Modal::Form {
                        form: Form::new(
                            format!("Machine of {}", ws.name),
                            vec![
                                Field::text("cpus", "vCPUs", &ws.vcpus.to_string())
                                    .help(format!("the number of vCPUs (1-32). {next}")),
                                Field::text("memory", "Memory", &format_mib(ws.memory_mib))
                                    .help(format!("like 8G or 512M. {next}")),
                                Field::text("idle", "Idle stop", &idle)
                                    .help("stops the VM this long after the last connection closes, like 30m or 2h. never keeps it running; default uses the gateway's."),
                            ],
                        ),
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
            (Tab::Rules, KeyCode::Enter | KeyCode::Char('e')) => {
                let Some(view) = self.selected_rules() else {
                    return self.show_selected_rules();
                };
                if !view.editable {
                    self.status = Some((
                        format!("you cannot change the rules of {}", view.scope.label()),
                        true,
                    ));
                    return vec![];
                }
                self.modals.push(Modal::Form {
                    form: policy_form::rules_form(&view.scope.label(), &view.own),
                    purpose: Purpose::Rules(view.scope.clone()),
                    saving: false,
                });
            }
            (Tab::Rules, KeyCode::Char('d')) => {
                if let Some(view) = self.selected_rules()
                    && view.editable
                    && !view.own.is_empty()
                {
                    self.modals.push(Modal::Confirm {
                        message: format!("Delete the rules of {}?", view.scope.label()),
                        action: Action::ClearRules(view.scope.clone()),
                    });
                }
            }
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
            (Tab::Inbox, _) => return self.handle_inbox_key(key),
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
            Modal::Requests { .. } => return self.handle_requests_key(key),
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
            Outcome::RuleScopes(scopes) => {
                let current = self.selected_scope().map(|e| e.scope.clone());
                let index = current
                    .and_then(|c| scopes.iter().position(|e| e.scope == c))
                    .unwrap_or(0);
                self.scopes = scopes;
                self.selected[tab_index(Tab::Rules)] = index;
                // 一覧を読み直したら、規則も読み直す (変えられるかが変わることがある)。
                self.rules = None;
                actions = self.show_selected_rules();
            }
            Outcome::Rules(view) => {
                if self.selected_scope().map(|e| &e.scope) == Some(&view.scope) {
                    self.rules = Some(view);
                }
            }
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

impl App {
    /// Rules タブ。左にスコープの一覧、右に選んでいるスコープで効く規則を評価する順に出す。
    fn render_rules(&self, frame: &mut Frame, area: Rect) {
        let [list_area, rules_area] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(area);
        let items: Vec<ListItem> = self
            .scopes
            .iter()
            .map(|entry| {
                let (depth, kind) = scope_shape(&entry.scope);
                let access = if entry.editable { "edit" } else { "view" };
                let name = format!("{}{}", "  ".repeat(depth), entry.scope.label());
                ListItem::new(format!("{name:<26} {kind:<8} {access}"))
            })
            .collect();
        render_list(frame, list_area, items, self.selection());
        let block = Block::bordered().title(match self.selected_scope() {
            Some(entry) => format!(" Rules applied to {} ", entry.scope.label()),
            None => " Rules ".to_string(),
        });
        let inner = block.inner(rules_area);
        frame.render_widget(block, rules_area);
        let Some(view) = self.selected_rules() else {
            frame.render_widget(Paragraph::new("…".dark_gray()), inner);
            return;
        };
        let mut lines: Vec<Line> = view
            .rules
            .iter()
            .map(|(origin, rule)| {
                let own = is_own(&view.scope, origin);
                let text = format!(
                    "{} {:<14} {}",
                    if own { "▶" } else { " " },
                    origin_label(origin),
                    describe_rule(rule)
                );
                if own {
                    Line::from(text.bold())
                } else {
                    Line::from(text)
                }
            })
            .collect();
        lines.push(Line::from("  anything else is blocked".dark_gray()));
        if !view.editable {
            lines.push(Line::default());
            lines.push(Line::from("  (view only)".dark_gray()));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}

fn tab_index(tab: Tab) -> usize {
    TABS.iter().position(|t| *t == tab).unwrap_or(0)
}

fn owner_kind(owner: &Owner) -> &'static str {
    match owner {
        Owner::User(_) => "personal",
        Owner::Org(_) => "org",
        Owner::Team { .. } => "team",
    }
}

/// スコープの一覧での字下げと種類。ワークスペースは持ち主の下に、team は Organization の下に置く。
fn scope_shape(scope: &RuleScope) -> (usize, &'static str) {
    let depth = |owner: &Owner| usize::from(matches!(owner, Owner::Team { .. }));
    match scope {
        RuleScope::Gateway => (0, "config"),
        RuleScope::Owner(owner) => (depth(owner), owner_kind(owner)),
        RuleScope::Workspace(ws) => (depth(&ws.owner) + 1, "ws"),
    }
}

/// 規則がスコープ自身のものか。
fn is_own(scope: &RuleScope, origin: &RuleOrigin) -> bool {
    match (scope, origin) {
        (RuleScope::Gateway, RuleOrigin::Gateway) => true,
        (RuleScope::Owner(owner), RuleOrigin::Owner(from)) => owner == from,
        (RuleScope::Workspace(_), RuleOrigin::Workspace) => true,
        _ => false,
    }
}

fn origin_label(origin: &RuleOrigin) -> String {
    match origin {
        RuleOrigin::Gateway => "gateway".into(),
        RuleOrigin::Owner(owner) => owner_arg(owner),
        RuleOrigin::Workspace => "workspace".into(),
        RuleOrigin::Secret => "(secrets)".into(),
    }
}

fn describe_rule(rule: &PathRuleConfig) -> String {
    let action = match rule.action {
        RuleAction::Allow => "allow",
        RuleAction::Deny => "deny",
        RuleAction::Passthrough => "pass",
    };
    let mut text = format!("{action:<5} {}", rule.hosts.join(", "));
    if !rule.methods.is_empty() {
        text.push_str(&format!(" {}", rule.methods.join(",")));
    }
    if !rule.paths.is_empty() {
        text.push_str(&format!(" {}", rule.paths.join(", ")));
    }
    text
}

/// ディスクの大きさの欄。空なら `None`。
fn optional_gib(form: &Form, key: &str) -> Result<Option<u32>, String> {
    let text = form.text(key);
    if text.is_empty() {
        return Ok(None);
    }
    crate::ops::parse_gib(&text)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// vCPU の数の欄。空なら `None`。
fn optional_vcpus(form: &Form) -> Result<Option<u32>, String> {
    let text = form.text("cpus");
    if text.is_empty() {
        return Ok(None);
    }
    text.parse()
        .map(Some)
        .map_err(|_| format!("invalid number of vCPUs: {text}"))
}

/// メモリの欄。空なら `None`。
fn optional_mib(form: &Form) -> Result<Option<u32>, String> {
    let text = form.text("memory");
    if text.is_empty() {
        return Ok(None);
    }
    crate::ops::parse_mib(&text)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// 停止までの時間の欄。空なら `None`。
fn optional_idle(form: &Form) -> Result<Option<IdleStop>, String> {
    let text = form.text("idle");
    if text.is_empty() {
        return Ok(None);
    }
    crate::ops::parse_idle_stop(&text)
        .map(Some)
        .map_err(|e| e.to_string())
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
            let options = NewWorkspace {
                root,
                disks: DiskSizes {
                    workspace_gib: optional_gib(form, "workspace")?,
                    docker_gib: optional_gib(form, "docker")?,
                },
                machine: MachineSize {
                    vcpus: optional_vcpus(form)?,
                    memory_mib: optional_mib(form)?,
                },
                idle_stop: optional_idle(form)?.unwrap_or_default(),
            };
            Ok(Action::CreateWorkspace { name, options })
        }
        Purpose::Machine {
            name,
            vcpus,
            memory_mib,
            idle,
        } => {
            // 変えなかった欄は送らない (既定のままの値を、今の既定の値に固定しない)。
            let machine = MachineSize {
                vcpus: optional_vcpus(form)?.filter(|n| n != vcpus),
                memory_mib: optional_mib(form)?.filter(|n| n != memory_mib),
            };
            let idle_stop = if form.text("idle") == *idle {
                None
            } else {
                optional_idle(form)?
            };
            if machine == MachineSize::default() && idle_stop.is_none() {
                return Err("change a value".into());
            }
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
            // 変えなかった欄は送らない (既定のままのディスクを、今の既定の値に固定しない)。
            let changed =
                |key, now| Ok::<_, String>(optional_gib(form, key)?.filter(|gib| *gib != now));
            let disks = DiskSizes {
                workspace_gib: changed("workspace", *workspace_gib)?,
                docker_gib: changed("docker", *docker_gib)?,
            };
            if disks == DiskSizes::default() {
                return Err("change a size".into());
            }
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
pub(crate) mod tests {
    use super::super::form::tests::{ctrl, key, type_text};
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

    #[test]
    fn grows_disks_with_a_form() {
        let mut app = app();
        assert!(screen(&app).contains("20G/40G"));
        app.handle_key(key(KeyCode::Char('g')));
        // 今の大きさが入っている。変えた欄だけを送る。
        for _ in 0.."20G".len() {
            form_mut(&mut app).handle_key(key(KeyCode::Backspace));
        }
        type_text(form_mut(&mut app), "50G");
        let actions = app.handle_key(ctrl('s'));
        assert!(matches!(
            &actions[..],
            [Action::SetDisks { name, disks }] if name == "default"
                && *disks == DiskSizes { workspace_gib: Some(50), docker_gib: None }
        ));
        // 何も変えなければ送らない。
        app.apply(Outcome::Done {
            message: "ok".into(),
            reload: vec![],
        });
        app.handle_key(key(KeyCode::Char('g')));
        assert!(app.handle_key(ctrl('s')).is_empty());
        assert!(screen(&app).contains("change a size"));
    }

    #[test]
    fn changes_the_machine_with_a_form() {
        let mut app = app();
        assert!(screen(&app).contains("4 vCPU    4G  idle 30m"));
        app.handle_key(key(KeyCode::Char('m')));
        // 今の値が入っている。変えた欄だけを送る。
        form_mut(&mut app).handle_key(key(KeyCode::Backspace));
        type_text(form_mut(&mut app), "8");
        form_mut(&mut app).handle_key(key(KeyCode::Tab));
        form_mut(&mut app).handle_key(key(KeyCode::Tab));
        for _ in 0.."30m".len() {
            form_mut(&mut app).handle_key(key(KeyCode::Backspace));
        }
        type_text(form_mut(&mut app), "never");
        let actions = app.handle_key(ctrl('s'));
        assert!(matches!(
            &actions[..],
            [Action::SetMachine { name, machine, idle_stop: Some(IdleStop::Never) }]
                if name == "default"
                    && *machine == MachineSize { vcpus: Some(8), memory_mib: None }
        ));
        // 何も変えなければ送らない。
        app.apply(Outcome::Done {
            message: "ok".into(),
            reload: vec![],
        });
        app.handle_key(key(KeyCode::Char('m')));
        assert!(app.handle_key(ctrl('s')).is_empty());
        assert!(screen(&app).contains("change a value"));
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
            [Action::CreateWorkspace { name, options }]
                if name == "api" && *options == NewWorkspace {
                    root: Some(RootMode::Ephemeral),
                    ..Default::default()
                }
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

    fn rule(host: &str, action: RuleAction) -> PathRuleConfig {
        PathRuleConfig {
            name: None,
            hosts: vec![host.into()],
            methods: vec![],
            paths: vec![],
            action,
        }
    }

    #[test]
    fn edits_rules_of_the_selected_scope() {
        let mut app = app();
        app.handle_key(key(KeyCode::Char('3')));
        // 選んだスコープで効く規則を読む。
        for _ in 0..4 {
            app.handle_key(key(KeyCode::Down));
        }
        let actions = app.handle_key(key(KeyCode::Up));
        let [Action::ShowRules(scope @ RuleScope::Owner(Owner::Org(_)))] = &actions[..] else {
            panic!("expected the rules of acme");
        };
        let acme = Owner::Org("acme".into());
        app.apply(Outcome::Rules(RulesView {
            scope: scope.clone(),
            editable: false,
            rules: vec![
                (RuleOrigin::Gateway, rule("github.com", RuleAction::Allow)),
                (
                    RuleOrigin::Owner(acme.clone()),
                    rule("example.org", RuleAction::Allow),
                ),
            ],
            own: vec![rule("example.org", RuleAction::Allow)],
        }));
        let text = screen(&app);
        assert!(text.contains("Rules applied to acme"));
        assert!(text.contains("▶ acme"));
        assert!(text.contains("anything else is blocked"));
        // 変えられないスコープでは、フォームを開かない。
        assert!(app.handle_key(key(KeyCode::Enter)).is_empty());
        assert!(app.modals.is_empty());
        assert!(screen(&app).contains("you cannot change the rules of acme"));

        // ワークスペースのスコープで、規則を足して保存する。
        let actions = app.handle_key(key(KeyCode::Down));
        let [Action::ShowRules(scope)] = &actions[..] else {
            panic!("expected the rules of acme/web");
        };
        app.apply(Outcome::Rules(RulesView {
            scope: scope.clone(),
            editable: true,
            rules: vec![],
            own: vec![],
        }));
        app.handle_key(key(KeyCode::Enter));
        let form = form_mut(&mut app);
        while form.fields[form.focus].key != "rules" {
            form.handle_key(key(KeyCode::Down));
        }
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('a')));
        form_mut(&mut app).handle_key(key(KeyCode::Tab));
        type_text(form_mut(&mut app), "pypi.org");
        app.handle_key(ctrl('s'));
        app.handle_key(key(KeyCode::Esc));
        let actions = app.handle_key(ctrl('s'));
        let [Action::SaveRules { scope, rules }] = &actions[..] else {
            panic!("expected a save");
        };
        assert!(matches!(scope, RuleScope::Workspace(ws) if ws.display_name() == "acme/web"));
        assert_eq!(rules, &[rule("pypi.org", RuleAction::Allow)]);
    }

    #[test]
    fn opens_the_rules_of_a_workspace() {
        let mut app = app();
        let actions = app.handle_key(key(KeyCode::Char('u')));
        assert!(matches!(
            &actions[..],
            [Action::ShowRules(RuleScope::Workspace(ws))] if ws.name == "default"
        ));
        assert_eq!(app.current(), Tab::Rules);
        // 古い結果 (ほかのスコープのもの) は使わない。
        app.apply(Outcome::Rules(RulesView {
            scope: RuleScope::Gateway,
            editable: false,
            rules: vec![(RuleOrigin::Gateway, rule("github.com", RuleAction::Allow))],
            own: vec![],
        }));
        assert!(!screen(&app).contains("github.com"));
    }

    fn form_mut(app: &mut App) -> &mut Form {
        match app.modals.last_mut() {
            Some(Modal::Form { form, .. }) => form,
            _ => panic!("no form"),
        }
    }
}
