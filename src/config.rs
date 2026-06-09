//! Config file at `~/.config/mnml-msg-slack/config.toml`. First
//! run writes the scaffold + exits with instructions.
//!
//! Auth lives entirely in env (`SLACK_USER_TOKEN`, optional
//! `SLACK_BOT_TOKEN`) — never in the TOML.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    #[serde(default)]
    pub post_multiline: bool,
    #[serde(default)]
    pub tabs: Vec<Tab>,
}

fn default_refresh() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub name: String,
    /// Tab kind:
    ///   - `channels` — public + private channels, sorted by membership / unread
    ///   - `dms` — direct messages + multi-person DMs
    ///   - `search` — interactive query input (search.messages)
    ///   - `threads` — v0.1 stub
    pub kind: String,
    /// Reserved for v0.2 (per-tab filters / query presets).
    #[serde(default)]
    pub query: Option<String>,
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-msg-slack config. Edit and re-run.
#
# Auth lives in env vars (NOT here):
#   export SLACK_USER_TOKEN=xoxp-...   (required — user token)
#   export SLACK_BOT_TOKEN=xoxb-...    (optional — falls back to user)
#
# Create a Slack app at https://api.slack.com/apps, install it to
# your workspace, request the User-token scopes listed in the
# README, then copy the User OAuth Token.

refresh_interval_secs = 60
post_multiline = false

# ── Tabs ─────────────────────────────────────────────────────────
# Kinds:
#   "channels" — public + private channels
#   "dms"      — direct messages + group DMs
#   "search"   — interactive search.messages query
#   "threads"  — v0.1 stub

[[tabs]]
name = "channels"
kind = "channels"

[[tabs]]
name = "dms"
kind = "dms"

[[tabs]]
name = "search"
kind = "search"

[[tabs]]
name = "threads"
kind = "threads"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "channels" | "dms" | "search" | "threads" => {}
                other => {
                    return Err(anyhow!(
                        "tab #{i} ({}): unknown kind {other:?} (expected \"channels\", \"dms\", \"search\", or \"threads\")",
                        t.name
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-msg-slack")
        .join("config.toml")
}

pub fn load() -> Result<Config> {
    let path = config_path();
    let first_run = !path.exists();
    if first_run {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, Config::EXAMPLE)?;
        eprintln!(
            "first run: wrote config template to {} — edit it to customize",
            path.display()
        );
    }
    let text = std::fs::read_to_string(&path)?;
    let cfg: Config = toml::from_str(&text)?;
    cfg.validate()?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_validates() {
        let cfg: Config = toml::from_str(Config::EXAMPLE).expect("example parses");
        cfg.validate().expect("example validates");
        assert!(!cfg.tabs.is_empty());
    }

    #[test]
    fn rejects_no_tabs() {
        let cfg = Config {
            refresh_interval_secs: 60,
            post_multiline: false,
            tabs: vec![],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_unknown_kind() {
        let cfg = Config {
            refresh_interval_secs: 60,
            post_multiline: false,
            tabs: vec![Tab {
                name: "bad".into(),
                kind: "bogus".into(),
                query: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn accepts_all_known_kinds() {
        for kind in &["channels", "dms", "search", "threads"] {
            let cfg = Config {
                refresh_interval_secs: 60,
                post_multiline: false,
                tabs: vec![Tab {
                    name: "x".into(),
                    kind: kind.to_string(),
                    query: None,
                }],
            };
            assert!(cfg.validate().is_ok(), "expected `{kind}` to validate");
        }
    }
}
