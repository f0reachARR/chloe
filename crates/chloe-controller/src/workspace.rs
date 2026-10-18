//! ワークスペースとその名前。
//!
//! SSH では、ワークスペースを持ち主に応じて次のように書く。
//!
//! - 個人：`name` (接続した利用者のもの)
//! - Organization：`org/name`
//! - team：`org/team/name`

use std::fmt;
use std::str::FromStr;

use chloe_secrets::Owner;

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RootMode {
    /// 共通イメージに tmpfs を重ね、起動のたびに捨てる。
    Ephemeral,
    /// 共通イメージに重ねる層をワークスペースのディスクに残す。
    Persistent,
}

impl RootMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ephemeral => "ephemeral",
            Self::Persistent => "persistent",
        }
    }
}

impl FromStr for RootMode {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ephemeral" => Ok(Self::Ephemeral),
            "persistent" => Ok(Self::Persistent),
            _ => Err(Error::Invalid(format!("unknown root mode: {s}"))),
        }
    }
}

/// ワークスペースのディスクの大きさ (GiB)。`None` はゲートウェイの設定の既定
/// ([`DiskConfig`])。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskSizes {
    /// ワークスペースのディスク (`/workspace`、ホームディレクトリ、永続のルートの層)。
    pub workspace_gib: Option<u32>,
    /// Docker のディスク (`/var/lib/docker`、`/var/lib/containerd`)。
    pub docker_gib: Option<u32>,
}

/// ディスクの大きさの既定と上限 (ゲートウェイの `[disks]`)。
///
/// ディスクは疎なファイルなので、大きくしても使うまではホストの容量を取らない。そのぶん
/// いくらでも大きくできてしまうので、上限を置く。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DiskConfig {
    pub workspace_gib: u32,
    pub docker_gib: u32,
    pub max_workspace_gib: u32,
    pub max_docker_gib: u32,
}

impl Default for DiskConfig {
    fn default() -> Self {
        Self {
            workspace_gib: 20,
            docker_gib: 40,
            max_workspace_gib: 200,
            max_docker_gib: 400,
        }
    }
}

impl DiskConfig {
    /// 既定が 1 以上で、上限を超えていないかを確かめる。
    pub fn check(&self) -> Result<(), String> {
        for (name, default, max) in [
            ("workspace", self.workspace_gib, self.max_workspace_gib),
            ("docker", self.docker_gib, self.max_docker_gib),
        ] {
            if default == 0 || default > max {
                return Err(format!(
                    "disks.{name}_gib must be between 1 and disks.max_{name}_gib ({max})"
                ));
            }
        }
        Ok(())
    }

    /// 既定を当てはめたワークスペースのディスクの大きさ。
    pub fn workspace_gib(&self, disks: DiskSizes) -> u32 {
        disks.workspace_gib.unwrap_or(self.workspace_gib)
    }

    /// 既定を当てはめた Docker のディスクの大きさ。
    pub fn docker_gib(&self, disks: DiskSizes) -> u32 {
        disks.docker_gib.unwrap_or(self.docker_gib)
    }

    /// 作るワークスペースのディスクの大きさを確かめる。
    pub fn validate_new(&self, requested: DiskSizes) -> Result<(), Error> {
        self.validate(None, requested).map(|_| ())
    }

    /// `current` を `requested` に変えた大きさを返す。書かなかったほうは変えない。
    /// 小さくはできない (ext4 を縮めるのは時間がかかり、中身が収まらなければ失敗する)。
    pub fn validate_change(
        &self,
        current: DiskSizes,
        requested: DiskSizes,
    ) -> Result<DiskSizes, Error> {
        self.validate(Some(current), requested)
    }

    fn validate(
        &self,
        current: Option<DiskSizes>,
        requested: DiskSizes,
    ) -> Result<DiskSizes, Error> {
        let check = |name: &str, requested: Option<u32>, now: Option<u32>, max: u32| {
            let Some(gib) = requested else {
                return Ok(());
            };
            if gib == 0 || gib > max {
                return Err(Error::Invalid(format!(
                    "the {name} disk must be between 1 and {max} GiB"
                )));
            }
            match now {
                Some(now) if gib < now => Err(Error::Invalid(format!(
                    "the {name} disk is {now} GiB and cannot be shrunk (remove the workspace with its disks to start over)"
                ))),
                _ => Ok(()),
            }
        };
        check(
            "workspace",
            requested.workspace_gib,
            current.map(|c| self.workspace_gib(c)),
            self.max_workspace_gib,
        )?;
        check(
            "docker",
            requested.docker_gib,
            current.map(|c| self.docker_gib(c)),
            self.max_docker_gib,
        )?;
        let current = current.unwrap_or_default();
        Ok(DiskSizes {
            workspace_gib: requested.workspace_gib.or(current.workspace_gib),
            docker_gib: requested.docker_gib.or(current.docker_gib),
        })
    }
}

/// Firecracker の VM に割り当てられる vCPU の数の上限。
pub const MAX_VCPUS: u32 = 32;
/// VM のメモリの下限 (MiB)。これより小さいと、systemd と Docker が動くゲストが起動しきらない。
pub const MIN_MEMORY_MIB: u32 = 512;

/// VM の大きさ。`None` はゲートウェイの設定の既定 ([`MachineConfig`])。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MachineSize {
    pub vcpus: Option<u32>,
    /// メモリ (MiB)。
    pub memory_mib: Option<u32>,
}

/// VM の大きさの既定 (ゲートウェイの `[machine]`)。
///
/// 上限は置かない。ホストより大きな VM は、起動するときに失敗するか、ホストのメモリを
/// 取り合う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MachineConfig {
    pub vcpus: u32,
    pub memory_mib: u32,
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            vcpus: 4,
            memory_mib: 4096,
        }
    }
}

impl MachineConfig {
    /// 既定が VM に割り当てられる値かを確かめる。
    pub fn check(&self) -> Result<(), String> {
        self.validate(MachineSize {
            vcpus: Some(self.vcpus),
            memory_mib: Some(self.memory_mib),
        })
        .map_err(|e| format!("machine: {e}"))
    }

    /// 既定を当てはめた vCPU の数。
    pub fn vcpus(&self, machine: MachineSize) -> u32 {
        machine.vcpus.unwrap_or(self.vcpus)
    }

    /// 既定を当てはめたメモリ (MiB)。
    pub fn memory_mib(&self, machine: MachineSize) -> u32 {
        machine.memory_mib.unwrap_or(self.memory_mib)
    }

    /// 書いた値が VM に割り当てられるかを確かめる。
    pub fn validate(&self, requested: MachineSize) -> Result<(), Error> {
        if let Some(vcpus) = requested.vcpus
            && !(1..=MAX_VCPUS).contains(&vcpus)
        {
            return Err(Error::Invalid(format!(
                "the number of vCPUs must be between 1 and {MAX_VCPUS}"
            )));
        }
        if let Some(mib) = requested.memory_mib
            && mib < MIN_MEMORY_MIB
        {
            return Err(Error::Invalid(format!(
                "the memory must be at least {MIN_MEMORY_MIB} MiB"
            )));
        }
        Ok(())
    }

    /// `current` を `requested` に変えた大きさを返す。書かなかったほうは変えない。
    pub fn validate_change(
        &self,
        current: MachineSize,
        requested: MachineSize,
    ) -> Result<MachineSize, Error> {
        self.validate(requested)?;
        Ok(MachineSize {
            vcpus: requested.vcpus.or(current.vcpus),
            memory_mib: requested.memory_mib.or(current.memory_mib),
        })
    }
}

/// 最後の接続が閉じてから、ランナーを止めるまで。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IdleStop {
    /// ゲートウェイの設定の `idle_timeout_secs`。
    #[default]
    Default,
    /// 秒数。
    After(u32),
    /// 接続がなくても止めない (`chloe ws stop` で止める)。
    Never,
}

impl IdleStop {
    /// 止めるまでの時間。止めないなら `None`。
    pub fn timeout(self, default: std::time::Duration) -> Option<std::time::Duration> {
        match self {
            Self::Default => Some(default),
            Self::After(secs) => Some(std::time::Duration::from_secs(secs.into())),
            Self::Never => None,
        }
    }

    /// データベースに入れる値。NULL は既定、0 は止めない。
    pub fn to_column(self) -> Option<u32> {
        match self {
            Self::Default => None,
            Self::After(secs) => Some(secs),
            Self::Never => Some(0),
        }
    }

    pub fn from_column(value: Option<u32>) -> Self {
        match value {
            None => Self::Default,
            Some(0) => Self::Never,
            Some(secs) => Self::After(secs),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub id: i64,
    pub owner: Owner,
    pub name: String,
    pub root_mode: RootMode,
    pub disks: DiskSizes,
    pub machine: MachineSize,
    pub idle_stop: IdleStop,
}

impl Workspace {
    /// SSH で書く形の名前。
    pub fn display_name(&self) -> String {
        WorkspaceRef {
            owner: self.owner.clone(),
            name: self.name.clone(),
        }
        .to_string()
    }

    /// VM のホスト名。プロンプトに出るので、SSH で書く名前に近づける (`acme/dev/api` は
    /// `acme-dev-api`)。ホスト名に使えない文字は `-` にし、63 文字に収める。
    pub fn hostname(&self) -> String {
        let name: String = self
            .display_name()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .take(63)
            .collect();
        let name = name.trim_matches('-');
        if name.is_empty() {
            "chloe".into()
        } else {
            name.to_ascii_lowercase()
        }
    }

    /// ランナーやディレクトリの名前に使う、ワークスペースごとに一意な識別子。
    pub fn key(&self) -> String {
        format!("ws-{}", self.id)
    }
}

/// 持ち主と名前の組。データベースにまだないワークスペースも指せる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRef {
    pub owner: Owner,
    pub name: String,
}

impl WorkspaceRef {
    /// SSH で書かれた名前を、接続した利用者 `login` から見て解釈する。
    pub fn parse(spec: &str, login: &str) -> Result<Self, Error> {
        let parts: Vec<&str> = spec.split('/').collect();
        let invalid = || Error::Invalid(format!("invalid workspace: {spec}"));
        let (owner, name) = match parts.as_slice() {
            [name] => (format!("user:{login}"), *name),
            [org, name] => (format!("org:{org}"), *name),
            [org, team, name] => (format!("team:{org}/{team}"), *name),
            _ => return Err(invalid()),
        };
        validate_name(name)?;
        Ok(Self {
            owner: owner.parse().map_err(|_| invalid())?,
            name: name.to_ascii_lowercase(),
        })
    }
}

impl fmt::Display for WorkspaceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.owner {
            Owner::User(_) => write!(f, "{}", self.name),
            Owner::Org(org) => write!(f, "{org}/{}", self.name),
            Owner::Team { org, team } => write!(f, "{org}/{team}/{}", self.name),
        }
    }
}

pub fn validate_name(name: &str) -> Result<(), Error> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !name.starts_with('-');
    if valid {
        Ok(())
    } else {
        Err(Error::Invalid(format!("invalid workspace name: {name}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_names() {
        let personal = WorkspaceRef::parse("api", "alice").unwrap();
        assert_eq!(personal.owner, Owner::User("alice".into()));
        assert_eq!(personal.to_string(), "api");

        let org = WorkspaceRef::parse("ForteFibre/api", "alice").unwrap();
        assert_eq!(org.owner, Owner::Org("fortefibre".into()));
        assert_eq!(org.to_string(), "fortefibre/api");

        let team = WorkspaceRef::parse("fortefibre/dev/api", "alice").unwrap();
        assert_eq!(team.to_string(), "fortefibre/dev/api");

        for bad in ["", "a/b/c/d", "../x", "-x", "a b", "org/"] {
            assert!(WorkspaceRef::parse(bad, "alice").is_err(), "{bad}");
        }
    }

    #[test]
    fn hostnames_follow_ssh_names() {
        let ws = |spec: &str| {
            let r = WorkspaceRef::parse(spec, "alice").unwrap();
            Workspace {
                id: 1,
                owner: r.owner,
                name: r.name,
                root_mode: RootMode::Ephemeral,
                disks: DiskSizes::default(),
                machine: MachineSize::default(),
                idle_stop: IdleStop::Default,
            }
        };
        assert_eq!(ws("default").hostname(), "default");
        assert_eq!(ws("Acme/dev/my_api").hostname(), "acme-dev-my-api");
        assert_eq!(ws("_x_").hostname(), "x");
        assert_eq!(ws(&"a".repeat(64)).hostname().len(), 63);
    }

    #[test]
    fn disks_only_grow_within_limits() {
        let config = DiskConfig::default();
        config.check().unwrap();
        let sizes = |w, d| DiskSizes {
            workspace_gib: w,
            docker_gib: d,
        };
        // 既定 (20 GiB) から広げる。書かなかった Docker のディスクは既定のまま。
        let grown = config
            .validate_change(DiskSizes::default(), sizes(Some(50), None))
            .unwrap();
        assert_eq!(grown, sizes(Some(50), None));
        assert_eq!(config.docker_gib(grown), 40);
        // 同じ大きさは受け付ける。
        config
            .validate_change(grown, sizes(Some(50), None))
            .unwrap();
        // 小さくはできない。既定より小さくもできない。
        assert!(
            config
                .validate_change(grown, sizes(Some(49), None))
                .is_err()
        );
        assert!(
            config
                .validate_change(DiskSizes::default(), sizes(None, Some(39)))
                .is_err()
        );
        // 上限を超えられない。
        assert!(
            config
                .validate_change(grown, sizes(None, Some(401)))
                .is_err()
        );
        // 作るときは既定より小さくてよい。
        config.validate_new(sizes(Some(10), Some(10))).unwrap();
        assert!(config.validate_new(sizes(Some(0), None)).is_err());
        assert!(
            DiskConfig {
                workspace_gib: 300,
                ..config
            }
            .check()
            .is_err()
        );
    }

    #[test]
    fn machines_fit_firecracker() {
        let config = MachineConfig::default();
        config.check().unwrap();
        let size = |vcpus, memory_mib| MachineSize { vcpus, memory_mib };
        // 書かなかったほうは変えない。
        let changed = config
            .validate_change(size(Some(2), None), size(None, Some(16384)))
            .unwrap();
        assert_eq!(changed, size(Some(2), Some(16384)));
        assert_eq!(config.vcpus(MachineSize::default()), 4);
        assert_eq!(config.memory_mib(MachineSize::default()), 4096);
        // 小さくもできる。
        config.validate(size(Some(1), Some(512))).unwrap();
        config.validate(size(Some(32), None)).unwrap();
        assert!(config.validate(size(Some(0), None)).is_err());
        assert!(config.validate(size(Some(33), None)).is_err());
        assert!(config.validate(size(None, Some(511))).is_err());
        assert!(MachineConfig { vcpus: 0, ..config }.check().is_err());
    }

    #[test]
    fn idle_stop_round_trips_through_the_column() {
        for idle in [IdleStop::Default, IdleStop::After(7200), IdleStop::Never] {
            assert_eq!(IdleStop::from_column(idle.to_column()), idle);
        }
        let default = std::time::Duration::from_secs(1800);
        assert_eq!(IdleStop::Default.timeout(default), Some(default));
        assert_eq!(
            IdleStop::After(60).timeout(default),
            Some(std::time::Duration::from_secs(60))
        );
        assert_eq!(IdleStop::Never.timeout(default), None);
    }
}
