//! VM の中で公開した、ブラウザから開ける HTTP(S) のサーバーのポート。
//!
//! 公開は VM の中から問い合わせ用ソケットで届く ([`crate::broker`])。どのワークスペースの公開かは
//! ソケットで決まる。公開はデータベースに置き、取り消すまで残す。ゲートウェイは公開したポートだけを
//! プロキシのホスト ([`ProxyBase`]) で中継する。

use reqwest::Url;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use tracing::info;

use crate::Error;
use crate::store::{Store, now};
use crate::workspace::Workspace;

/// ワークスペースごとの、公開できるポートの数の上限。
pub const MAX_PORTS: usize = 20;

/// VM の中のサーバーとの話し方。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    #[default]
    Http,
    /// TLS でつなぐ。証明書は確かめない。
    Https,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    fn parse(s: &str) -> Result<Self, Error> {
        match s {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            other => Err(Error::Invalid(format!("unknown scheme {other:?}"))),
        }
    }
}

/// 公開したポート。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct PublishedPort {
    #[serde(skip)]
    pub workspace_id: i64,
    pub port: u16,
    pub scheme: Scheme,
    /// プロキシのホスト名に使うランダムな名前。
    #[serde(skip)]
    pub name: String,
    /// ブラウザで開く URL。ゲートウェイに `[api] proxy_url` がなければ出さない。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// UNIX 時刻 (秒)。
    pub created_at: i64,
}

/// VM から届いた公開。
#[derive(Debug, Clone, Deserialize)]
pub struct NewPort {
    pub port: u16,
    #[serde(default)]
    pub scheme: Scheme,
}

/// プロキシのホストの名前の長さ。英小文字と 2 から 7 の数字 (base32) で、60 ビットになる。
const NAME_LEN: usize = 12;
const NAME_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// 公開したポートのホストのランダムな名前。ホスト名からワークスペースやポートを推し量れず、
/// 試して当てることもできないようにする。
fn random_name() -> String {
    let mut bytes = [0u8; NAME_LEN];
    rand::fill(&mut bytes);
    bytes
        .iter()
        .map(|b| NAME_ALPHABET[usize::from(b & 31)] as char)
        .collect()
}

fn is_name(label: &str) -> bool {
    label.len() == NAME_LEN && label.bytes().all(|b| NAME_ALPHABET.contains(&b))
}

/// プロキシのホストの元 (`[api] proxy_url`)。公開したポートごとに、ランダムな名前の
/// `<名前>.<ドメイン>` のホストを割り当てる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyBase {
    scheme: String,
    domain: String,
    /// URL に書いたポート (既定のポートなら `None`)。
    port: Option<u16>,
}

impl ProxyBase {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let invalid = |m: &str| Error::Invalid(format!("proxy_url {m}: {text}"));
        let url = Url::parse(text).map_err(|_| invalid("is not a URL"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(invalid("must be an http(s) URL"));
        }
        if url.path() != "/" || url.query().is_some() || !url.username().is_empty() {
            return Err(invalid("must not have a path, a query or a user"));
        }
        let Some(domain) = url.domain() else {
            return Err(invalid("must have a domain name (not an IP address)"));
        };
        Ok(Self {
            scheme: url.scheme().to_string(),
            domain: domain.to_ascii_lowercase(),
            port: url.port(),
        })
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// 公開したポートのホスト名 (ポートを付けない)。
    pub fn host(&self, name: &str) -> String {
        format!("{name}.{}", self.domain)
    }

    /// 公開したポートのオリジン (`https://k3x7q2m5vbtd.preview.example.com`)。
    pub fn origin(&self, name: &str) -> String {
        let host = self.host(name);
        match self.port {
            Some(p) => format!("{}://{host}:{p}", self.scheme),
            None => format!("{}://{host}", self.scheme),
        }
    }

    /// 要求の `Host` (ポートが付いていてもよい) がプロキシのホストなら、その名前を返す。
    pub fn parse_host(&self, host: &str) -> Option<String> {
        let host = host.to_ascii_lowercase();
        let name = match host.rsplit_once(':') {
            Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
            _ => host.as_str(),
        };
        let label = name.strip_suffix(&self.domain)?.strip_suffix('.')?;
        is_name(label).then(|| label.to_string())
    }
}

#[derive(Clone)]
pub struct Ports {
    store: Store,
    base: Option<ProxyBase>,
}

impl Ports {
    pub fn new(store: Store, base: Option<ProxyBase>) -> Self {
        Self { store, base }
    }

    pub fn base(&self) -> Option<&ProxyBase> {
        self.base.as_ref()
    }

    fn port(&self, row: &sqlx::sqlite::SqliteRow) -> Result<PublishedPort, Error> {
        let port: i64 = row.try_get("port")?;
        let port =
            u16::try_from(port).map_err(|_| Error::Invalid(format!("invalid port {port}")))?;
        let name: String = row.try_get("name")?;
        Ok(PublishedPort {
            workspace_id: row.try_get("workspace_id")?,
            port,
            scheme: Scheme::parse(&row.try_get::<String, _>("scheme")?)?,
            url: self.base.as_ref().map(|b| format!("{}/", b.origin(&name))),
            name,
            created_at: row.try_get("created_at")?,
        })
    }

    /// ポートを公開する。公開してあれば、話し方だけを変える (ホストの名前は変えない)。
    pub async fn publish(
        &self,
        workspace: &Workspace,
        new: NewPort,
    ) -> Result<PublishedPort, Error> {
        if new.port == 0 {
            return Err(Error::Invalid("the port must be 1-65535".into()));
        }
        let published = self.list(workspace.id).await?;
        if published.len() >= MAX_PORTS && !published.iter().any(|p| p.port == new.port) {
            return Err(Error::Invalid(format!(
                "at most {MAX_PORTS} ports can be published"
            )));
        }
        // 名前がほかの公開とぶつかることはまずないが、ぶつかれば作り直す。
        let mut attempts = 0;
        let row = loop {
            let result = sqlx::query(
                "INSERT INTO published_ports (workspace_id, port, scheme, name, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (workspace_id, port) DO UPDATE SET scheme = excluded.scheme
                 RETURNING workspace_id, port, scheme, name, created_at",
            )
            .bind(workspace.id)
            .bind(i64::from(new.port))
            .bind(new.scheme.as_str())
            .bind(random_name())
            .bind(now())
            .fetch_one(self.store.pool())
            .await;
            match result {
                Ok(row) => break row,
                Err(sqlx::Error::Database(e)) if e.is_unique_violation() && attempts < 3 => {
                    attempts += 1;
                }
                Err(e) => return Err(e.into()),
            }
        };
        info!(target: "audit", workspace = %workspace.display_name(), port = new.port, scheme = new.scheme.as_str(), "port published");
        self.port(&row)
    }

    /// 公開したポートの一覧 (ポートの順)。
    pub async fn list(&self, workspace_id: i64) -> Result<Vec<PublishedPort>, Error> {
        let rows = sqlx::query("SELECT workspace_id, port, scheme, name, created_at FROM published_ports WHERE workspace_id = ?1 ORDER BY port")
        .bind(workspace_id)
        .fetch_all(self.store.pool())
        .await?;
        rows.iter().map(|row| self.port(row)).collect()
    }

    /// プロキシのホストの名前で、公開したポートを探す。
    pub async fn find(&self, name: &str) -> Result<Option<PublishedPort>, Error> {
        let row = sqlx::query("SELECT workspace_id, port, scheme, name, created_at FROM published_ports WHERE name = ?1")
        .bind(name)
        .fetch_optional(self.store.pool())
        .await?;
        row.map(|row| self.port(&row)).transpose()
    }

    /// 公開を取り消す。取り消したら `true`。`by` は監査ログに残す (VM の中からなら `agent`)。
    pub async fn unpublish(
        &self,
        workspace: &Workspace,
        port: u16,
        by: &str,
    ) -> Result<bool, Error> {
        let result =
            sqlx::query("DELETE FROM published_ports WHERE workspace_id = ?1 AND port = ?2")
                .bind(workspace.id)
                .bind(i64::from(port))
                .execute(self.store.pool())
                .await?;
        let removed = result.rows_affected() > 0;
        if removed {
            info!(target: "audit", workspace = %workspace.display_name(), port, by, "port unpublished");
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::pool;
    use crate::workspace::{DiskSizes, IdleStop, MachineSize, RootMode, WorkspaceRef};

    fn base() -> ProxyBase {
        ProxyBase::parse("https://Preview.Example.com").unwrap()
    }

    #[test]
    fn builds_and_parses_hosts() {
        let base = base();
        assert_eq!(
            base.host("k3x7q2m5vbtd"),
            "k3x7q2m5vbtd.preview.example.com"
        );
        assert_eq!(
            base.origin("k3x7q2m5vbtd"),
            "https://k3x7q2m5vbtd.preview.example.com"
        );
        assert_eq!(
            base.parse_host("k3x7q2m5vbtd.preview.example.com")
                .as_deref(),
            Some("k3x7q2m5vbtd")
        );
        assert_eq!(
            base.parse_host("K3X7Q2M5VBTD.Preview.Example.COM:443")
                .as_deref(),
            Some("k3x7q2m5vbtd")
        );
        for host in [
            "preview.example.com",
            "k3x7q2m5vbt.preview.example.com",
            "k3x7q2m5vbtdd.preview.example.com",
            "k3x7q2m5vbt1.preview.example.com",
            "k3x7q2m5vbt-.preview.example.com",
            "x.k3x7q2m5vbtd.preview.example.com",
            "k3x7q2m5vbtd.evilpreview.example.com",
            "k3x7q2m5vbtd.preview.example.com.evil.test",
            "chloe.example.com",
        ] {
            assert_eq!(base.parse_host(host), None, "{host}");
        }

        let local = ProxyBase::parse("http://preview.localhost:8080").unwrap();
        assert_eq!(
            local.origin("abcdefghijkl"),
            "http://abcdefghijkl.preview.localhost:8080"
        );
        assert_eq!(
            local
                .parse_host("abcdefghijkl.preview.localhost:8080")
                .as_deref(),
            Some("abcdefghijkl")
        );

        for bad in [
            "ftp://x.example.com",
            "https://x.example.com/p",
            "https://10.0.0.1",
            "nope",
        ] {
            assert!(ProxyBase::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_are_random() {
        let a = random_name();
        assert!(is_name(&a), "{a}");
        assert_ne!(a, random_name());
    }

    #[tokio::test]
    async fn publish_list_and_unpublish() {
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
        let ports = Ports::new(store, Some(base()));
        let p = ports
            .publish(
                &ws,
                NewPort {
                    port: 5173,
                    scheme: Scheme::Http,
                },
            )
            .await
            .unwrap();
        assert!(is_name(&p.name), "{}", p.name);
        assert_eq!(
            p.url.as_deref(),
            Some(format!("https://{}.preview.example.com/", p.name).as_str())
        );
        // 公開し直すと、話し方だけが変わる (ホストの名前は変わらない)。
        let again = ports
            .publish(
                &ws,
                NewPort {
                    port: 5173,
                    scheme: Scheme::Https,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            (again.name.as_str(), again.scheme),
            (p.name.as_str(), Scheme::Https)
        );
        assert_eq!(
            ports
                .find(&p.name)
                .await
                .unwrap()
                .map(|f| (f.workspace_id, f.port)),
            Some((ws.id, 5173))
        );
        ports
            .publish(
                &ws,
                NewPort {
                    port: 3000,
                    scheme: Scheme::Http,
                },
            )
            .await
            .unwrap();
        let listed = ports.list(ws.id).await.unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|p| (p.port, p.scheme))
                .collect::<Vec<_>>(),
            vec![(3000, Scheme::Http), (5173, Scheme::Https)]
        );
        assert!(
            ports
                .publish(
                    &ws,
                    NewPort {
                        port: 0,
                        scheme: Scheme::Http
                    }
                )
                .await
                .is_err()
        );
        assert!(ports.unpublish(&ws, 5173, "alice").await.unwrap());
        assert!(!ports.unpublish(&ws, 5173, "alice").await.unwrap());
        assert_eq!(ports.find(&p.name).await.unwrap(), None);
        // 取り消してから公開すると、新しい名前になる。
        let renewed = ports
            .publish(
                &ws,
                NewPort {
                    port: 5173,
                    scheme: Scheme::Http,
                },
            )
            .await
            .unwrap();
        assert_ne!(renewed.name, p.name);
        assert!(ports.unpublish(&ws, 5173, "alice").await.unwrap());

        for port in 1..MAX_PORTS as u16 {
            ports
                .publish(
                    &ws,
                    NewPort {
                        port: 10000 + port,
                        scheme: Scheme::Http,
                    },
                )
                .await
                .unwrap();
        }
        assert!(
            ports
                .publish(
                    &ws,
                    NewPort {
                        port: 9999,
                        scheme: Scheme::Http
                    }
                )
                .await
                .is_err()
        );
        // 公開してあるポートは、上限でも公開し直せる。
        ports
            .publish(
                &ws,
                NewPort {
                    port: 3000,
                    scheme: Scheme::Https,
                },
            )
            .await
            .unwrap();
    }
}
