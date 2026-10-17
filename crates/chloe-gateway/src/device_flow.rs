//! GitHub の Device Flow の承認を待って、ユーザートークンを得る。
//!
//! SSH の認証 (keyboard-interactive)、`chloe github login`、管理の TUI が使う。

use std::time::Duration;

use anyhow::{Result, bail};
use chloe_github::{DeviceCode, GitHubApp, Poll, UserToken};

use crate::access::personal;
use crate::session::Shared;

/// 承認を `deadline` まで待つ。承認されたら、トークンの持ち主のログイン名とトークンを返す
/// (保存はしない)。まだ承認されていなければ `None` を返す。
pub async fn wait_for_token(
    github: &GitHubApp,
    device: &DeviceCode,
    deadline: tokio::time::Instant,
) -> Result<Option<(String, UserToken)>> {
    loop {
        match github.poll_device_flow(device).await? {
            Poll::Token(token) => {
                let owner = github.user_login(&token.access_token).await?;
                return Ok(Some((owner, token)));
            }
            Poll::Pending { interval } => {
                let next = tokio::time::Instant::now() + Duration::from_secs(interval);
                if next > deadline {
                    return Ok(None);
                }
                tokio::time::sleep_until(next).await;
            }
            Poll::Failed(message) => bail!("GitHub sign-in failed: {message}"),
        }
    }
}

/// ログインした利用者 `login` のトークンを取り直す。コードが失効するまで承認を待ち、トークンの
/// 持ち主が `login` なら保存して、動いている個人のワークスペースに渡し直す。渡し直した
/// ワークスペースの数を返す。失効したら `None` を返す。
pub async fn renew(
    shared: &Shared,
    github: &GitHubApp,
    login: &str,
    device: &DeviceCode,
) -> Result<Option<usize>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(device.expires_in);
    let Some((owner, token)) = wait_for_token(github, device, deadline).await? else {
        return Ok(None);
    };
    if !owner.eq_ignore_ascii_case(login) {
        bail!("signed in to GitHub as {owner}, not {login}");
    }
    shared.store_user_token(login, &token).await?;
    Ok(Some(shared.refresh_running(Some(&personal(login))).await))
}
