//! VM の中のエージェントからの依頼 (通信の許可と質問)。
//!
//! 依頼はランナーのプロキシの問い合わせ用ソケットから届く ([`crate::broker`])。どのワークスペースの
//! 依頼かはソケットで決まる。依頼はデータベースに残し、出たときと答えたときに [`AskEvent`] を流す。
//! 管理の TUI はこれを受けて画面を更新し、ほかの届け先 (Slack など) も同じ出来事を受けて足せる。
//!
//! 答えられる人を確かめるのと、通信の許可を承認したときに規則を足すのは、呼び出し側
//! (ゲートウェイ) が行う。ここでは、答えられるのは期限内に答えを待つ依頼に一度だけ、を守る。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sqlx::Row;
use tokio::sync::broadcast;
use tracing::info;

use crate::Error;
use crate::store::Store;
use crate::workspace::Workspace;

/// 期限の既定。
pub const DEFAULT_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// エージェントが決められるいちばん短い期限。
const MIN_TTL: Duration = Duration::from_secs(60);
/// ワークスペースごとの、答えを待つ依頼の上限。
pub const MAX_PENDING: usize = 20;
/// 質問と理由の長さ (文字数) の上限。
const MAX_TEXT: usize = 4000;
const MAX_OPTIONS: usize = 10;
/// 選択肢、メソッド、パスの一つの長さの上限。
const MAX_ITEM: usize = 200;
const MAX_PATHS: usize = 10;
const MAX_METHODS: usize = 10;
/// 答え (質問への答えと、断るときの言葉) の長さの上限。
const MAX_ANSWER: usize = 4000;

/// 依頼の中身。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AskBody {
    /// 宛先への通信の許可。メソッドとパスは空ならすべて。
    Network {
        host: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        methods: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        paths: Vec<String>,
    },
    /// 人への質問。選択肢が空なら自由に答える。
    Question {
        question: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<String>,
    },
}

impl AskBody {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Network { .. } => "network",
            Self::Question { .. } => "question",
        }
    }

    /// 一覧に出す短い説明。
    pub fn summary(&self) -> String {
        match self {
            Self::Network {
                host,
                methods,
                paths,
            } => {
                let mut s = host.clone();
                if !methods.is_empty() {
                    s = format!("{} {s}", methods.join(","));
                }
                if !paths.is_empty() {
                    s.push_str(&format!(" {}", paths.join(" ")));
                }
                s
            }
            Self::Question { question, .. } => question.lines().next().unwrap_or("").to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AskStatus {
    Pending,
    Allowed,
    Denied,
    Answered,
    Expired,
    Cancelled,
}

impl AskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Answered => "answered",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(s: &str) -> Result<Self, Error> {
        Ok(match s {
            "pending" => Self::Pending,
            "allowed" => Self::Allowed,
            "denied" => Self::Denied,
            "answered" => Self::Answered,
            "expired" => Self::Expired,
            "cancelled" => Self::Cancelled,
            other => return Err(Error::Invalid(format!("unknown ask status {other:?}"))),
        })
    }
}

impl std::fmt::Display for AskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 依頼。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Ask {
    pub id: i64,
    #[serde(skip)]
    pub workspace_id: i64,
    #[serde(flatten)]
    pub body: AskBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 期限を過ぎた答えを待つ依頼は [`AskStatus::Expired`] になる。
    pub status: AskStatus,
    /// 質問への答えか、断ったときの言葉。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<String>,
    /// UNIX 時刻 (秒)。
    pub created_at: i64,
    pub expires_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<i64>,
}

/// VM から届いた依頼。[`NewAsk::sanitize`] で確かめてから使う。
#[derive(Debug, Clone, Deserialize)]
pub struct NewAsk {
    #[serde(flatten)]
    pub body: AskBody,
    #[serde(default)]
    pub reason: Option<String>,
    /// 期限 (秒)。書かなければ [`DEFAULT_TTL`]。
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

impl NewAsk {
    /// 長さを限り、改行とタブ以外の制御文字を除き、宛先とメソッドとパスの形を確かめる。
    pub fn sanitize(self) -> Result<(AskBody, Option<String>, Duration), Error> {
        let invalid = |m: String| Error::Invalid(m);
        let body = match self.body {
            AskBody::Network {
                host,
                methods,
                paths,
            } => {
                let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
                check_host(&host).map_err(invalid)?;
                if methods.len() > MAX_METHODS || paths.len() > MAX_PATHS {
                    return Err(invalid("too many methods or paths".into()));
                }
                let mut methods: Vec<String> = methods
                    .iter()
                    .map(|m| m.trim().to_ascii_uppercase())
                    .collect();
                if let Some(bad) = methods.iter().find(|m| {
                    m.is_empty() || m.len() > 16 || !m.chars().all(|c| c.is_ascii_uppercase())
                }) {
                    return Err(invalid(format!("invalid method {bad:?}")));
                }
                methods.sort();
                methods.dedup();
                if let Some(bad) = paths.iter().find(|p| {
                    !p.starts_with('/')
                        || p.len() > MAX_ITEM
                        || p.chars().any(|c| c.is_control() || c.is_whitespace())
                }) {
                    return Err(invalid(format!("invalid path {bad:?}")));
                }
                AskBody::Network {
                    host,
                    methods,
                    paths,
                }
            }
            AskBody::Question { question, options } => {
                let question = clean_text(&question, MAX_TEXT);
                if question.trim().is_empty() {
                    return Err(invalid("the question is empty".into()));
                }
                if options.len() > MAX_OPTIONS {
                    return Err(invalid(format!("at most {MAX_OPTIONS} options")));
                }
                let options: Vec<String> = options
                    .iter()
                    .map(|o| clean_line(o, MAX_ITEM))
                    .filter(|o| !o.is_empty())
                    .collect();
                AskBody::Question { question, options }
            }
        };
        let reason = self
            .reason
            .map(|r| clean_text(&r, MAX_TEXT))
            .filter(|r| !r.trim().is_empty());
        let ttl = match self.timeout_secs {
            None => DEFAULT_TTL,
            Some(secs) => Duration::from_secs(secs).clamp(MIN_TTL, DEFAULT_TTL),
        };
        Ok((body, reason, ttl))
    }
}

/// 規則の `hosts` に書ける形 (先頭の `*.` のほかは英数字、`-`、`.`) か。chloe の中の名前と
/// IP アドレスは断る。
fn check_host(host: &str) -> Result<(), String> {
    let name = host.strip_prefix("*.").unwrap_or(host);
    let valid = !name.is_empty()
        && host.len() <= 253
        && name.contains('.')
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if !valid {
        return Err(format!("invalid host {host:?}"));
    }
    // IP アドレスの宛先は頼ませない (プライベートなアドレスを許させないため)。
    if name
        .split('.')
        .all(|label| label.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(format!("{host} is an IP address; ask for a host name"));
    }
    if name == "chloe.internal" || name.ends_with(".chloe.internal") {
        return Err(format!("{host} is internal to chloe"));
    }
    Ok(())
}

/// 改行とタブのほかの制御文字を除き、`max` 文字までにする。
fn clean_text(s: &str, max: usize) -> String {
    s.replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(max)
        .collect()
}

/// 一行にする (制御文字を除く)。
fn clean_line(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// 依頼の出来事。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskEvent {
    Created {
        workspace_id: i64,
        id: i64,
    },
    /// 答えたか、取り下げた。
    Resolved {
        workspace_id: i64,
        id: i64,
    },
}

/// 答え。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Allow,
    Deny(Option<String>),
    Answer(String),
    Cancel,
}

impl Resolution {
    fn status(&self) -> AskStatus {
        match self {
            Self::Allow => AskStatus::Allowed,
            Self::Deny(_) => AskStatus::Denied,
            Self::Answer(_) => AskStatus::Answered,
            Self::Cancel => AskStatus::Cancelled,
        }
    }
}

/// 依頼を扱う。複製しても同じ表と出来事の流れを指す。
#[derive(Clone)]
pub struct Asks {
    store: Store,
    events: broadcast::Sender<AskEvent>,
}

/// 表の列。`sqlx` は組み立てた文字列を受け付けないので、マクロで文に埋める。
macro_rules! select_asks {
    ($rest:literal) => {
        concat!(
            "SELECT id, workspace_id, kind, body, reason, status, answer, answered_by, created_at, expires_at, answered_at FROM asks ",
            $rest
        )
    };
}

impl Asks {
    pub fn new(store: Store) -> Self {
        let (events, _) = broadcast::channel(64);
        Self { store, events }
    }

    /// 依頼の出来事を受け取る。受け取りが遅れて取りこぼしたら、一覧を読み直せばよい。
    pub fn subscribe(&self) -> broadcast::Receiver<AskEvent> {
        self.events.subscribe()
    }

    /// 依頼を出す。同じ宛先、メソッド、パスの通信の許可を待っていれば、それを返す。
    pub async fn create(&self, workspace: &Workspace, new: NewAsk) -> Result<Ask, Error> {
        let (body, reason, ttl) = new.sanitize()?;
        let now = now();
        let pending = self.list(&[workspace.id], true).await?;
        if matches!(body, AskBody::Network { .. })
            && let Some(same) = pending.iter().find(|ask| ask.body == body)
        {
            return Ok(same.clone());
        }
        if pending.len() >= MAX_PENDING {
            return Err(Error::Invalid(format!(
                "too many pending asks (at most {MAX_PENDING})"
            )));
        }
        let json = serde_json::to_string(&body).map_err(|e| Error::Invalid(e.to_string()))?;
        let expires_at = now + ttl.as_secs() as i64;
        let id = sqlx::query(
            "INSERT INTO asks (workspace_id, kind, body, reason, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(workspace.id)
        .bind(body.kind())
        .bind(json)
        .bind(&reason)
        .bind(now)
        .bind(expires_at)
        .execute(self.store.pool())
        .await?
        .last_insert_rowid();
        info!(
            target: "audit",
            workspace = %workspace.display_name(),
            ask = id,
            kind = body.kind(),
            summary = %body.summary(),
            "an agent asked"
        );
        let _ = self.events.send(AskEvent::Created {
            workspace_id: workspace.id,
            id,
        });
        Ok(Ask {
            id,
            workspace_id: workspace.id,
            body,
            reason,
            status: AskStatus::Pending,
            answer: None,
            answered_by: None,
            created_at: now,
            expires_at,
            answered_at: None,
        })
    }

    pub async fn get(&self, id: i64) -> Result<Option<Ask>, Error> {
        let row = sqlx::query(select_asks!("WHERE id = ?1"))
            .bind(id)
            .fetch_optional(self.store.pool())
            .await?;
        row.map(|row| ask_from_row(&row, now())).transpose()
    }

    /// ワークスペースの依頼を新しい順に。`pending` なら答えを待つもの (期限内) だけ。
    /// 答えたものは 200 件までにする。
    pub async fn list(&self, workspace_ids: &[i64], pending: bool) -> Result<Vec<Ask>, Error> {
        if workspace_ids.is_empty() {
            return Ok(vec![]);
        }
        let now = now();
        let ids =
            serde_json::to_string(workspace_ids).map_err(|e| Error::Invalid(e.to_string()))?;
        let rows = sqlx::query(select_asks!(
            "WHERE workspace_id IN (SELECT value FROM json_each(?1))
             AND (?2 = 0 OR (status = 'pending' AND expires_at > ?3))
             ORDER BY id DESC LIMIT 200"
        ))
        .bind(ids)
        .bind(pending)
        .bind(now)
        .fetch_all(self.store.pool())
        .await?;
        rows.iter().map(|row| ask_from_row(row, now)).collect()
    }

    /// 答えを待つ依頼があるか (止める前に確かめる)。
    pub async fn has_pending(&self, workspace_id: i64) -> Result<bool, Error> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM asks WHERE workspace_id = ?1 AND status = 'pending' AND expires_at > ?2",
        )
        .bind(workspace_id)
        .bind(now())
        .fetch_one(self.store.pool())
        .await?;
        Ok(row.try_get::<i64, _>("n")? > 0)
    }

    /// 答えを待つ依頼に答える。期限を過ぎていたり、もう答えていたりすれば断る。
    pub async fn resolve(&self, id: i64, resolution: Resolution, by: &str) -> Result<Ask, Error> {
        let answer = match &resolution {
            Resolution::Deny(message) => message.as_deref().map(|m| clean_text(m, MAX_ANSWER)),
            Resolution::Answer(answer) => Some(clean_text(answer, MAX_ANSWER)),
            Resolution::Allow | Resolution::Cancel => None,
        };
        let status = resolution.status();
        let now = now();
        let updated = sqlx::query(
            "UPDATE asks SET status = ?1, answer = ?2, answered_by = ?3, answered_at = ?4
             WHERE id = ?5 AND status = 'pending' AND expires_at > ?4",
        )
        .bind(status.as_str())
        .bind(&answer)
        .bind(by)
        .bind(now)
        .bind(id)
        .execute(self.store.pool())
        .await?
        .rows_affected();
        let ask = self
            .get(id)
            .await?
            .ok_or_else(|| Error::Invalid(format!("no such ask {id}")))?;
        if updated == 0 {
            return Err(Error::Invalid(format!(
                "ask {id} is already {}",
                ask.status
            )));
        }
        info!(target: "audit", ask = id, status = %status, by, "an ask was resolved");
        let _ = self.events.send(AskEvent::Resolved {
            workspace_id: ask.workspace_id,
            id,
        });
        Ok(ask)
    }
}

fn ask_from_row(row: &sqlx::sqlite::SqliteRow, now: i64) -> Result<Ask, Error> {
    let body: String = row.try_get("body")?;
    let body: AskBody =
        serde_json::from_str(&body).map_err(|e| Error::Invalid(format!("ask body: {e}")))?;
    let expires_at: i64 = row.try_get("expires_at")?;
    let mut status = AskStatus::parse(&row.try_get::<String, _>("status")?)?;
    if status == AskStatus::Pending && expires_at <= now {
        status = AskStatus::Expired;
    }
    Ok(Ask {
        id: row.try_get("id")?,
        workspace_id: row.try_get("workspace_id")?,
        body,
        reason: row.try_get("reason")?,
        status,
        answer: row.try_get("answer")?,
        answered_by: row.try_get("answered_by")?,
        created_at: row.try_get("created_at")?,
        expires_at,
        answered_at: row.try_get("answered_at")?,
    })
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::pool;
    use crate::workspace::{DiskSizes, IdleStop, MachineSize, RootMode, WorkspaceRef};

    async fn setup() -> (Asks, Workspace) {
        let store = Store::new(pool().await);
        let ws = store
            .create(
                &WorkspaceRef::parse("api", "alice").unwrap(),
                RootMode::Ephemeral,
                DiskSizes::default(),
                MachineSize::default(),
                IdleStop::Default,
                "alice",
            )
            .await
            .unwrap();
        (Asks::new(store), ws)
    }

    fn network(host: &str) -> NewAsk {
        serde_json::from_value(serde_json::json!({ "kind": "network", "host": host })).unwrap()
    }

    fn question(text: &str) -> NewAsk {
        serde_json::from_value(serde_json::json!({
            "kind": "question", "question": text, "options": ["yes", "no"],
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn create_dedupe_and_resolve_once() {
        let (asks, ws) = setup().await;
        let mut events = asks.subscribe();
        let a = asks.create(&ws, network("Example.COM.")).await.unwrap();
        assert_eq!(
            a.body,
            AskBody::Network {
                host: "example.com".into(),
                methods: vec![],
                paths: vec![]
            }
        );
        assert_eq!(
            events.recv().await.unwrap(),
            AskEvent::Created {
                workspace_id: ws.id,
                id: a.id
            }
        );
        // 同じ宛先を待っていれば、同じ依頼を返す。質問は重ねて出せる。
        assert_eq!(
            asks.create(&ws, network("example.com")).await.unwrap().id,
            a.id
        );
        let q1 = asks.create(&ws, question("ok?")).await.unwrap();
        let q2 = asks.create(&ws, question("ok?")).await.unwrap();
        assert_ne!(q1.id, q2.id);
        assert!(asks.has_pending(ws.id).await.unwrap());

        let done = asks
            .resolve(a.id, Resolution::Allow, "alice")
            .await
            .unwrap();
        assert_eq!(done.status, AskStatus::Allowed);
        assert_eq!(done.answered_by.as_deref(), Some("alice"));
        // 答えは一度だけ。
        assert!(
            asks.resolve(a.id, Resolution::Deny(None), "bob")
                .await
                .is_err()
        );
        assert_eq!(
            asks.get(a.id).await.unwrap().unwrap().status,
            AskStatus::Allowed
        );

        let answered = asks
            .resolve(q1.id, Resolution::Answer("yes\u{1b}[2J".into()), "bob")
            .await
            .unwrap();
        assert_eq!(answered.answer.as_deref(), Some("yes[2J"));
        asks.resolve(q2.id, Resolution::Cancel, "agent")
            .await
            .unwrap();
        assert!(!asks.has_pending(ws.id).await.unwrap());
        assert_eq!(asks.list(&[ws.id], false).await.unwrap().len(), 3);
        assert!(asks.list(&[ws.id], true).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn expired_asks_cannot_be_answered() {
        let (asks, ws) = setup().await;
        let a = asks.create(&ws, question("ok?")).await.unwrap();
        sqlx::query("UPDATE asks SET expires_at = 0 WHERE id = ?1")
            .bind(a.id)
            .execute(asks.store.pool())
            .await
            .unwrap();
        assert_eq!(
            asks.get(a.id).await.unwrap().unwrap().status,
            AskStatus::Expired
        );
        assert!(!asks.has_pending(ws.id).await.unwrap());
        assert!(
            asks.resolve(a.id, Resolution::Answer("y".into()), "alice")
                .await
                .is_err()
        );
        // 期限切れは数えないので、同じ宛先の依頼を新しく出せる。
        let n = asks.create(&ws, network("example.com")).await.unwrap();
        assert_eq!(asks.list(&[ws.id], true).await.unwrap(), [n]);
    }

    #[tokio::test]
    async fn limits_pending_asks() {
        let (asks, ws) = setup().await;
        for i in 0..MAX_PENDING {
            asks.create(&ws, question(&format!("q{i}"))).await.unwrap();
        }
        assert!(matches!(
            asks.create(&ws, question("one more")).await,
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn sanitizes_what_the_vm_sends() {
        for bad in [
            "",
            "localhost",
            "*",
            "*.com.",
            "a..b",
            "exa mple.com",
            "example.com:443",
            "ask.chloe.internal",
            "chloe.internal",
            "10.0.0.5",
            "172.29.0.1",
        ] {
            assert!(network(bad).sanitize().is_err(), "{bad:?}");
        }
        let new: NewAsk = serde_json::from_value(serde_json::json!({
            "kind": "network", "host": "*.example.com", "methods": ["get", "POST", "GET"],
            "paths": ["/v1/*"], "timeout_secs": 1,
        }))
        .unwrap();
        let (body, _, ttl) = new.sanitize().unwrap();
        assert_eq!(
            body,
            AskBody::Network {
                host: "*.example.com".into(),
                methods: vec!["GET".into(), "POST".into()],
                paths: vec!["/v1/*".into()],
            }
        );
        assert_eq!(ttl, MIN_TTL);
        let bad_path: NewAsk = serde_json::from_value(serde_json::json!({
            "kind": "network", "host": "example.com", "paths": ["v1"],
        }))
        .unwrap();
        assert!(bad_path.sanitize().is_err());

        let new: NewAsk = serde_json::from_value(serde_json::json!({
            "kind": "question", "question": "a\u{7}\nb", "options": ["x\ny", " "],
            "reason": "  ", "timeout_secs": 999_999_999u64,
        }))
        .unwrap();
        let (body, reason, ttl) = new.sanitize().unwrap();
        assert_eq!(
            body,
            AskBody::Question {
                question: "a\nb".into(),
                options: vec!["xy".into()],
            }
        );
        assert_eq!(reason, None);
        assert_eq!(ttl, DEFAULT_TTL);
    }
}
