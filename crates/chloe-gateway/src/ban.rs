//! 認証を通らずに終わった SSH の接続を接続元ごとに数え、多すぎる接続元をしばらく受け付けない。
//!
//! 認証に失敗した接続も、何もせずに切った接続も、同じ一回に数える。IPv6 は /64 ごとに数える
//! (一つの家やサーバーが /64 を丸ごと持つことが多く、アドレスを変えて試し直せるため)。

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv6Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

/// ban の設定。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BanConfig {
    /// `window_secs` のあいだに認証を通らずに終わった接続がこの数に達したら、接続元を ban する。
    /// 0 なら ban しない。
    pub max_failures: usize,
    pub window_secs: u64,
    /// ban する秒数。
    pub ban_secs: u64,
    /// 数えない接続元 (`127.0.0.1`、`10.0.0.0/8` など)。
    pub ignore: Vec<Cidr>,
}

impl Default for BanConfig {
    fn default() -> Self {
        Self {
            max_failures: 10,
            window_secs: 10 * 60,
            ban_secs: 10 * 60,
            ignore: vec![
                Cidr::parse("127.0.0.0/8").unwrap(),
                Cidr::parse("::1").unwrap(),
            ],
        }
    }
}

/// アドレスの範囲。`/` のあとを省くと、そのアドレスだけ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    pub fn parse(text: &str) -> Result<Self, String> {
        let (addr, prefix) = match text.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (text, None),
        };
        let addr: IpAddr = addr
            .parse()
            .map_err(|_| format!("not an IP address: {text:?}"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(prefix) => prefix
                .parse()
                .ok()
                .filter(|p| *p <= max)
                .ok_or_else(|| format!("invalid prefix length: {text:?}"))?,
            None => max,
        };
        Ok(Self { addr, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                mask(u32::from(net).into(), 32, self.prefix)
                    == mask(u32::from(ip).into(), 32, self.prefix)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                mask(net.into(), 128, self.prefix) == mask(ip.into(), 128, self.prefix)
            }
            _ => false,
        }
    }
}

impl TryFrom<String> for Cidr {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        Self::parse(&text)
    }
}

/// `bits` ビットのアドレスの上位 `prefix` ビットだけを残す。
fn mask(addr: u128, bits: u32, prefix: u8) -> u128 {
    let prefix = u32::from(prefix);
    if prefix == 0 {
        0
    } else {
        addr & (u128::MAX << (bits - prefix))
    }
}

#[derive(Default)]
struct Entry {
    /// window の中の失敗の時刻 (古い順)。
    failures: VecDeque<Instant>,
    banned_until: Option<Instant>,
}

/// 接続元ごとの失敗の数と ban。
pub struct Bans {
    config: BanConfig,
    entries: Mutex<HashMap<IpAddr, Entry>>,
}

impl Bans {
    pub fn new(config: BanConfig) -> Self {
        Self {
            config,
            entries: Mutex::default(),
        }
    }

    /// 数える単位。IPv4 はアドレス、IPv6 は /64。数えない接続元なら `None`。
    fn key(&self, ip: IpAddr) -> Option<IpAddr> {
        if self.config.max_failures == 0 || self.config.ignore.iter().any(|c| c.contains(ip)) {
            return None;
        }
        Some(match ip.to_canonical() {
            IpAddr::V6(ip) => IpAddr::V6(Ipv6Addr::from(mask(ip.into(), 128, 64))),
            ip => ip,
        })
    }

    /// ban している接続元か。
    pub fn is_banned(&self, ip: IpAddr, now: Instant) -> bool {
        let Some(key) = self.key(ip) else {
            return false;
        };
        let entries = self.entries.lock().unwrap();
        entries
            .get(&key)
            .and_then(|e| e.banned_until)
            .is_some_and(|until| now < until)
    }

    /// 認証を通らずに終わった接続を数える。これで ban したら、ban する長さを返す。
    pub fn fail(&self, ip: IpAddr, now: Instant) -> Option<Duration> {
        let key = self.key(ip)?;
        let window = Duration::from_secs(self.config.window_secs);
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.entry(key).or_default();
        while entry
            .failures
            .front()
            .is_some_and(|t| now.duration_since(*t) >= window)
        {
            entry.failures.pop_front();
        }
        entry.failures.push_back(now);
        if entry.failures.len() < self.config.max_failures {
            return None;
        }
        entry.failures.clear();
        let ban = Duration::from_secs(self.config.ban_secs);
        entry.banned_until = Some(now + ban);
        Some(ban)
    }

    /// 認証を通った。それまでの失敗を忘れる。
    pub fn succeed(&self, ip: IpAddr) {
        if let Some(key) = self.key(ip) {
            self.entries.lock().unwrap().remove(&key);
        }
    }

    /// window を過ぎた失敗しかなく、ban もしていない接続元を忘れる。
    pub fn prune(&self, now: Instant) {
        let window = Duration::from_secs(self.config.window_secs);
        self.entries.lock().unwrap().retain(|_, entry| {
            entry.banned_until.is_some_and(|until| now < until)
                || entry
                    .failures
                    .back()
                    .is_some_and(|t| now.duration_since(*t) < window)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bans(max_failures: usize) -> Bans {
        Bans::new(BanConfig {
            max_failures,
            window_secs: 60,
            ban_secs: 300,
            ..BanConfig::default()
        })
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn bans_after_max_failures_within_window() {
        let bans = bans(3);
        let t = Instant::now();
        let peer = ip("203.0.113.1");
        assert_eq!(bans.fail(peer, t), None);
        assert_eq!(bans.fail(peer, t + Duration::from_secs(10)), None);
        assert!(!bans.is_banned(peer, t + Duration::from_secs(10)));
        assert_eq!(
            bans.fail(peer, t + Duration::from_secs(20)),
            Some(Duration::from_secs(300))
        );
        assert!(bans.is_banned(peer, t + Duration::from_secs(20)));
        assert!(bans.is_banned(peer, t + Duration::from_secs(319)));
        assert!(!bans.is_banned(peer, t + Duration::from_secs(320)));
        assert!(!bans.is_banned(ip("203.0.113.2"), t + Duration::from_secs(20)));
    }

    #[test]
    fn forgets_failures_outside_window() {
        let bans = bans(3);
        let t = Instant::now();
        let peer = ip("203.0.113.1");
        bans.fail(peer, t);
        bans.fail(peer, t + Duration::from_secs(30));
        assert_eq!(bans.fail(peer, t + Duration::from_secs(60)), None);
        assert!(!bans.is_banned(peer, t + Duration::from_secs(60)));
    }

    #[test]
    fn success_clears_failures() {
        let bans = bans(2);
        let t = Instant::now();
        let peer = ip("203.0.113.1");
        bans.fail(peer, t);
        bans.succeed(peer);
        assert_eq!(bans.fail(peer, t), None);
    }

    #[test]
    fn counts_ipv6_by_64() {
        let bans = bans(2);
        let t = Instant::now();
        bans.fail(ip("2001:db8:1:2::1"), t);
        assert!(bans.fail(ip("2001:db8:1:2::2"), t).is_some());
        assert!(bans.is_banned(ip("2001:db8:1:2:ffff::"), t));
        assert!(!bans.is_banned(ip("2001:db8:1:3::1"), t));
    }

    #[test]
    fn ignores_listed_peers_and_zero() {
        let t = Instant::now();
        let one = bans(1);
        assert_eq!(one.fail(ip("127.0.0.1"), t), None);
        assert_eq!(one.fail(ip("::ffff:127.0.0.1"), t), None);
        assert_eq!(one.fail(ip("::1"), t), None);
        assert_eq!(bans(0).fail(ip("203.0.113.1"), t), None);
    }

    #[test]
    fn prunes_old_entries() {
        let bans = bans(2);
        let t = Instant::now();
        bans.fail(ip("203.0.113.1"), t);
        bans.fail(ip("203.0.113.2"), t);
        bans.fail(ip("203.0.113.2"), t);
        bans.prune(t + Duration::from_secs(61));
        assert_eq!(bans.entries.lock().unwrap().len(), 1);
        bans.prune(t + Duration::from_secs(301));
        assert!(bans.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn parses_cidr() {
        let net = Cidr::parse("10.0.0.0/8").unwrap();
        assert!(net.contains(ip("10.1.2.3")));
        assert!(!net.contains(ip("11.0.0.0")));
        assert!(Cidr::parse("0.0.0.0/0").unwrap().contains(ip("1.2.3.4")));
        assert!(Cidr::parse("fd00::/8").unwrap().contains(ip("fd12::1")));
        assert!(Cidr::parse("10.0.0.0/33").is_err());
        assert!(Cidr::parse("example.com").is_err());
    }
}
