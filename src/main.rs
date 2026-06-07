mod app;
mod clipboard;
mod config;
mod keys;
mod slack;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mnml-msg-slack",
    version,
    about = "Slack browse + post terminal client for the mnml family"
)]
struct Cli {
    /// Print resolved config + auth state (mask token, hit auth.test) and exit.
    #[arg(long)]
    check: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.check {
        let cfg = config::load();
        let auth = slack::Auth::from_env();

        println!("config: {}", config::config_path().display());
        match &cfg {
            Ok(cfg) => {
                println!(
                    "tabs ({}, refresh={}s, post_multiline={}):",
                    cfg.tabs.len(),
                    cfg.refresh_interval_secs,
                    cfg.post_multiline
                );
                for (i, t) in cfg.tabs.iter().enumerate() {
                    println!("  {} ({}): kind={}", i + 1, t.name, t.kind);
                }
            }
            Err(e) => println!("config: ERROR — {e}"),
        }

        println!();
        println!("env: SLACK_USER_TOKEN={}", mask_env("SLACK_USER_TOKEN"));
        println!("env: SLACK_BOT_TOKEN={}", mask_env("SLACK_BOT_TOKEN"));

        match &auth {
            Ok(a) => {
                println!();
                println!("api base: {}", a.api_base());
                println!("token kind: {} ({})", a.kind, slack::mask_token(&a.token));
                match slack::auth_test(a) {
                    Ok(test) => {
                        println!();
                        println!("auth.test: ok");
                        println!("  team:    {} ({})", test.team, test.team_id);
                        println!("  user:    {} ({})", test.user, test.user_id);
                        if !test.url.is_empty() {
                            println!("  url:     {}", test.url);
                        }
                    }
                    Err(e) => {
                        println!();
                        println!("auth.test: ERROR — {e}");
                        std::process::exit(2);
                    }
                }
            }
            Err(e) => {
                println!();
                println!("auth: ERROR — {e}");
                std::process::exit(2);
            }
        }

        if cfg.is_err() {
            std::process::exit(2);
        }
        return Ok(());
    }

    let cfg = config::load()?;
    let auth = match slack::Auth::from_env() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            eprintln!("setup:");
            eprintln!("  1. visit https://api.slack.com/apps and create an app");
            eprintln!("  2. add the User-token scopes (see README)");
            eprintln!("  3. install the app to your workspace");
            eprintln!("  4. copy the User OAuth Token (xoxp-…)");
            eprintln!("  5. export SLACK_USER_TOKEN=xoxp-...");
            eprintln!();
            eprintln!("then re-run, or `mnml-msg-slack --check` to confirm.");
            std::process::exit(2);
        }
    };

    let mut app = app::App::new(cfg, auth)?;
    ui::run(&mut app)
}

fn mask_env(name: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => slack::mask_token(&v),
        _ => "(unset)".into(),
    }
}
