//! GitHub App の設定を実際の GitHub で確かめる。トークンの値は表示しない。
//!
//! cargo run -p chloe-github --example check -- ~/.config/chloe/github-app.toml <login> [org/team]

use chloe_github::{AppConfig, GitHubApp, TokenScope};
use chloe_secrets::{ExposeSecret, Resolver};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let config = AppConfig::load(args.next().ok_or("config path")?.as_ref())?;
    let login = args.next().ok_or("login")?;
    let team = args.next();
    let app = GitHubApp::new(config, &Resolver { connect: None }).await?;
    let orgs = app.organizations().await?;
    println!("installed on: {orgs:?}");
    for org in &orgs {
        let token = app.installation_token(org, &TokenScope::default()).await?;
        println!(
            "{org}: installation token issued ({} chars); {login}: {:?}",
            token.expose_secret().len(),
            app.org_role(org, &login).await?
        );
    }
    if let Some((org, team)) = team.as_deref().and_then(|t| t.split_once('/')) {
        println!(
            "{login} in team {org}/{team}: {:?}",
            app.team_role(org, team, &login).await?
        );
    }
    Ok(())
}
