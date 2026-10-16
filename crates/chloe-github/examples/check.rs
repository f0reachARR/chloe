//! GitHub App の設定を実際の GitHub で確かめる。トークンの値は表示しない。
//!
//! cargo run -p chloe-github --example check -- ~/.config/chloe/github-app.toml <login> [team]

use chloe_github::{AppConfig, GitHubApp};
use chloe_secrets::{ExposeSecret, Resolver};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let config = AppConfig::load(args.next().ok_or("config path")?.as_ref())?;
    let login = args.next().ok_or("login")?;
    let team = args.next();
    let app = GitHubApp::new(config, &Resolver { connect: None }).await?;
    println!("installed on: {:?}", app.installed_accounts().await?);
    let token = app.installation_token().await?;
    println!(
        "installation token: issued ({} chars)",
        token.expose_secret().len()
    );
    println!(
        "{login} is a member of {}: {}",
        app.organization(),
        app.is_org_member(&login).await?
    );
    if let Some(team) = team {
        println!(
            "{login} is a member of team {team}: {}",
            app.is_team_member(&team, &login).await?
        );
    }
    Ok(())
}
