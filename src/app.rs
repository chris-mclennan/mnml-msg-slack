//! App state — per-tab item lists, focused-channel history cache,
//! user-name cache, post / search / react input modes.

use crate::config::{Config, Tab};
use crate::slack::{self, Auth, Channel, Message, QUICK_REACTIONS, SearchMatch};
use anyhow::Result;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 5-min channel-list cache.
const CHANNEL_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: String,
    #[allow(dead_code)]
    pub query: Option<String>,
}

impl TabSpec {
    pub fn resolve(t: &Tab) -> Result<Self> {
        match t.kind.as_str() {
            "channels" | "dms" | "search" | "threads" => Ok(Self {
                kind: t.kind.clone(),
                query: t.query.clone(),
            }),
            other => anyhow::bail!("tab `{}`: unknown kind {other:?}", t.name),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Item {
    Channel(Channel),
    SearchHit(SearchMatch),
    /// `threads` tab is a stub in v0.1.
    ThreadPlaceholder,
}

pub struct TabState {
    pub name: String,
    pub spec: TabSpec,
    pub items: Vec<Item>,
    pub selected: usize,
    pub last_loaded: Option<Instant>,
    pub last_error: Option<String>,
    pub loading: bool,
    /// `search` tab: the most-recently-submitted query.
    pub search_query: String,
}

impl TabState {
    fn empty(name: String, spec: TabSpec) -> Self {
        Self {
            name,
            spec,
            items: Vec::new(),
            selected: 0,
            last_loaded: None,
            last_error: None,
            loading: false,
            search_query: String::new(),
        }
    }
}

/// Right-pane state for a focused channel / DM.
pub struct ChannelDetail {
    pub channel_id: String,
    pub messages: Vec<Message>,
    pub last_loaded: Instant,
}

/// Interactive bottom-bar mode. None = passive; otherwise a one-line
/// text input that captures keystrokes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    Search,
    Post,
    ThreadReply,
}

#[derive(Debug, Clone)]
pub struct InputBar {
    pub mode: InputMode,
    pub buffer: String,
    /// For `ThreadReply` — the `(channel_id, parent_ts)` we're replying to.
    pub thread_target: Option<(String, String)>,
}

/// Reaction picker overlay state — selected index into [`QUICK_REACTIONS`].
#[derive(Debug, Clone)]
pub struct ReactionPicker {
    pub selected: usize,
    pub channel_id: String,
    pub message_ts: String,
}

pub struct App {
    pub cfg: Config,
    pub auth: Auth,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
    pub detail: Option<ChannelDetail>,
    /// User-id → resolved display name. Lazy-filled from `users.info`.
    pub user_names: HashMap<String, String>,
    /// Channel-list cache (`types` → (fetched-at, channels)).
    pub channel_cache: HashMap<String, (Instant, Vec<Channel>)>,
    pub input: Option<InputBar>,
    pub reaction_picker: Option<ReactionPicker>,
    /// Last `auth.test` payload — used for the title bar.
    pub team_name: String,
    pub self_user_id: String,
}

impl App {
    pub fn new(cfg: Config, auth: Auth) -> Result<Self> {
        let mut tabs = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let spec = TabSpec::resolve(t)?;
            tabs.push(TabState::empty(t.name.clone(), spec));
        }
        let mut app = App {
            cfg,
            auth,
            tabs,
            active_tab: 0,
            status: String::new(),
            detail: None,
            user_names: HashMap::new(),
            channel_cache: HashMap::new(),
            input: None,
            reaction_picker: None,
            team_name: String::new(),
            self_user_id: String::new(),
        };
        // Best-effort auth.test on startup — surfaces a bad token
        // immediately and primes the title bar. Don't hard-fail.
        match slack::auth_test(&app.auth) {
            Ok(t) => {
                app.team_name = t.team;
                app.self_user_id = t.user_id;
            }
            Err(e) => {
                app.status = format!("error: {e}");
            }
        }
        app.refresh_active(false);
        Ok(app)
    }

    pub fn active(&self) -> &TabState {
        &self.tabs[self.active_tab]
    }
    pub fn active_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active_tab]
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
            self.detail = None;
            if self.tabs[idx].items.is_empty() && self.tabs[idx].last_error.is_none() {
                self.refresh_active(false);
            } else {
                self.maybe_load_detail();
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let tab = self.active_mut();
        if tab.items.is_empty() {
            return;
        }
        let n = tab.items.len() as isize;
        let cur = tab.selected as isize;
        let next = (cur + delta).clamp(0, n - 1);
        tab.selected = next as usize;
        self.maybe_load_detail();
    }

    /// On channel/dm tabs only: lazy-load `conversations.history` for
    /// the focused channel into the detail pane.
    fn maybe_load_detail(&mut self) {
        let idx = self.active_tab;
        let kind = self.tabs[idx].spec.kind.clone();
        if kind != "channels" && kind != "dms" {
            self.detail = None;
            return;
        }
        let Some(Item::Channel(c)) = self.tabs[idx].items.get(self.tabs[idx].selected).cloned()
        else {
            self.detail = None;
            return;
        };
        // Skip refetch if this is already the focused detail and
        // we've loaded within the last few seconds.
        if let Some(d) = &self.detail
            && d.channel_id == c.id
            && d.last_loaded.elapsed() < Duration::from_secs(15)
        {
            return;
        }
        match slack::conversations_history(&self.auth, &c.id, 30) {
            Ok(msgs) => {
                // Prefetch unknown user names (best-effort — bounded).
                let unknown: Vec<String> = msgs
                    .iter()
                    .filter_map(|m| m.user.clone())
                    .filter(|u| !self.user_names.contains_key(u))
                    .take(10)
                    .collect();
                for uid in unknown {
                    if let Ok(u) = slack::users_info(&self.auth, &uid) {
                        self.user_names.insert(uid, u.best_name());
                    }
                }
                self.detail = Some(ChannelDetail {
                    channel_id: c.id.clone(),
                    messages: msgs,
                    last_loaded: Instant::now(),
                });
            }
            Err(e) => {
                self.status = format!("error: {e}");
            }
        }
    }

    /// `r` — force a fresh channel-list pull (bypass cache).
    pub fn refresh_force(&mut self) {
        self.refresh_active(true);
    }

    pub fn refresh_active(&mut self, force: bool) {
        let idx = self.active_tab;
        let kind = self.tabs[idx].spec.kind.clone();
        let name = self.tabs[idx].name.clone();
        self.tabs[idx].loading = true;

        let res: Result<Vec<Item>> = match kind.as_str() {
            "channels" => {
                let types = "public_channel,private_channel";
                self.list_channels_cached(types, force).map(|chans| {
                    sort_channels(chans)
                        .into_iter()
                        .map(Item::Channel)
                        .collect()
                })
            }
            "dms" => {
                let types = "im,mpim";
                self.list_channels_cached(types, force)
                    .map(|chans| chans.into_iter().map(Item::Channel).collect())
            }
            "search" => {
                // Search-tab refresh re-runs the last-submitted query.
                let q = self.tabs[idx].search_query.clone();
                if q.trim().is_empty() {
                    self.status = "(search): press / to enter a query".into();
                    self.tabs[idx].loading = false;
                    return;
                }
                slack::search_messages(&self.auth, &q)
                    .map(|hits| hits.into_iter().map(Item::SearchHit).collect())
            }
            "threads" => {
                self.tabs[idx].loading = false;
                self.tabs[idx].items = vec![Item::ThreadPlaceholder];
                self.tabs[idx].selected = 0;
                self.tabs[idx].last_loaded = Some(Instant::now());
                self.status = "threads: (v0.2 — needs scan across recent channels)".into();
                return;
            }
            _ => unreachable!("validated in TabSpec::resolve"),
        };

        let t = &mut self.tabs[idx];
        t.loading = false;
        match res {
            Ok(items) => {
                let n = items.len();
                t.items = items;
                t.selected = t.selected.min(n.saturating_sub(1));
                t.last_loaded = Some(Instant::now());
                t.last_error = None;
                self.status = format!("{name}: {n} item{}", if n == 1 { "" } else { "s" });
                self.maybe_load_detail();
            }
            Err(e) => {
                t.last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    fn list_channels_cached(&mut self, types: &str, force: bool) -> Result<Vec<Channel>> {
        if !force
            && let Some((fetched, chans)) = self.channel_cache.get(types)
            && fetched.elapsed() < CHANNEL_CACHE_TTL
        {
            return Ok(chans.clone());
        }
        let chans = slack::conversations_list(&self.auth, types)?;
        self.channel_cache
            .insert(types.to_string(), (Instant::now(), chans.clone()));
        Ok(chans)
    }

    pub fn focused_item(&self) -> Option<&Item> {
        let t = self.active();
        t.items.get(t.selected)
    }

    /// `Enter` — open a thread view. v0.1: bring the focused channel's
    /// detail pane to front + flash a hint. Real threaded view is v0.2.
    pub fn open_thread(&mut self) {
        match self.focused_item() {
            Some(Item::Channel(_)) => {
                self.maybe_load_detail();
                self.status = "loaded history (thread-view v0.2)".into();
            }
            Some(Item::SearchHit(hit)) => {
                self.status = format!("search hit ts={} (thread-view v0.2)", hit.ts);
            }
            Some(Item::ThreadPlaceholder) | None => {
                self.status = "nothing to open".into();
            }
        }
    }

    /// `/` — open the search input bar (only meaningful on the search tab).
    pub fn begin_search(&mut self) {
        if self.active().spec.kind != "search" {
            // Allow it from any tab — switch to the first search tab.
            if let Some(i) = self.tabs.iter().position(|t| t.spec.kind == "search") {
                self.switch_tab(i);
            } else {
                self.status = "no search tab configured".into();
                return;
            }
        }
        let initial = self.active().search_query.clone();
        self.input = Some(InputBar {
            mode: InputMode::Search,
            buffer: initial,
            thread_target: None,
        });
    }

    /// `p` — open the post input bar. Requires a focused channel.
    pub fn begin_post(&mut self) {
        let Some(channel) = self.focused_channel() else {
            self.status = "no channel under cursor".into();
            return;
        };
        let _ = channel;
        self.input = Some(InputBar {
            mode: InputMode::Post,
            buffer: String::new(),
            thread_target: None,
        });
    }

    /// `T` — open the thread-reply input bar. Requires a focused
    /// channel and a focused message in the detail pane.
    pub fn begin_thread_reply(&mut self) {
        let Some(channel) = self.focused_channel() else {
            self.status = "no channel under cursor".into();
            return;
        };
        let channel_id = channel.id.clone();
        let Some(detail) = &self.detail else {
            self.status = "no detail panel loaded".into();
            return;
        };
        // Pick the most-recent message in the detail pane as the
        // parent. v0.2 will add a cursor inside the detail pane.
        let Some(msg) = detail.messages.last() else {
            self.status = "no messages in channel".into();
            return;
        };
        let parent_ts = msg.thread_ts.clone().unwrap_or_else(|| msg.ts.clone());
        self.input = Some(InputBar {
            mode: InputMode::ThreadReply,
            buffer: String::new(),
            thread_target: Some((channel_id, parent_ts)),
        });
    }

    /// `R` — open the reaction picker overlay.
    pub fn begin_reaction(&mut self) {
        let Some(channel) = self.focused_channel() else {
            self.status = "no channel under cursor".into();
            return;
        };
        let channel_id = channel.id.clone();
        let Some(detail) = &self.detail else {
            self.status = "no detail panel loaded".into();
            return;
        };
        let Some(msg) = detail.messages.last() else {
            self.status = "no messages in channel".into();
            return;
        };
        self.reaction_picker = Some(ReactionPicker {
            selected: 0,
            channel_id,
            message_ts: msg.ts.clone(),
        });
    }

    pub fn cancel_input(&mut self) {
        self.input = None;
        self.status = "cancelled".into();
    }

    pub fn cancel_reaction(&mut self) {
        self.reaction_picker = None;
        self.status = "cancelled".into();
    }

    /// Commit the current input bar (`Enter`).
    pub fn submit_input(&mut self) {
        let Some(bar) = self.input.take() else {
            return;
        };
        match bar.mode {
            InputMode::Search => {
                let q = bar.buffer.trim().to_string();
                if q.is_empty() {
                    self.status = "search cancelled (empty query)".into();
                    return;
                }
                let idx = self.active_tab;
                self.tabs[idx].search_query = q;
                self.refresh_active(false);
            }
            InputMode::Post => {
                let Some(channel) = self.focused_channel() else {
                    self.status = "lost focused channel".into();
                    return;
                };
                let channel_id = channel.id.clone();
                let channel_name = channel.display_name();
                let text = bar.buffer.trim().to_string();
                if text.is_empty() {
                    self.status = "empty post".into();
                    return;
                }
                match slack::chat_post_message(&self.auth, &channel_id, &text, None) {
                    Ok(_) => {
                        self.status = format!("posted to {channel_name}");
                        // Force a detail refresh so the new message shows.
                        self.detail = None;
                        self.maybe_load_detail();
                    }
                    Err(e) => self.status = format!("error: {e}"),
                }
            }
            InputMode::ThreadReply => {
                let Some((channel_id, parent_ts)) = bar.thread_target.clone() else {
                    self.status = "lost thread target".into();
                    return;
                };
                let text = bar.buffer.trim().to_string();
                if text.is_empty() {
                    self.status = "empty thread reply".into();
                    return;
                }
                match slack::chat_post_message(&self.auth, &channel_id, &text, Some(&parent_ts)) {
                    Ok(_) => {
                        self.status = "thread reply sent".into();
                        self.detail = None;
                        self.maybe_load_detail();
                    }
                    Err(e) => self.status = format!("error: {e}"),
                }
            }
        }
    }

    /// `Enter` on the reaction picker.
    pub fn submit_reaction(&mut self) {
        let Some(picker) = self.reaction_picker.take() else {
            return;
        };
        let Some(emoji) = QUICK_REACTIONS.get(picker.selected) else {
            self.status = "reaction picker out of range".into();
            return;
        };
        match slack::reactions_add(&self.auth, &picker.channel_id, &picker.message_ts, emoji) {
            Ok(()) => self.status = format!("reacted :{emoji}:"),
            Err(e) => self.status = format!("error: {e}"),
        }
    }

    /// `y` — copy the permalink for the focused message (channel/DM)
    /// or the focused search hit.
    pub fn yank_permalink(&mut self) {
        match self.focused_item() {
            Some(Item::SearchHit(hit)) => {
                let url = hit.permalink.clone().unwrap_or_default();
                if url.is_empty() {
                    self.status = "no permalink on search hit".into();
                    return;
                }
                let n = url.chars().count();
                match crate::clipboard::copy(&url) {
                    Ok(()) => self.status = format!("copied permalink ({n} chars)"),
                    Err(e) => self.status = format!("copy failed: {e}"),
                }
            }
            Some(Item::Channel(_)) => {
                let Some(channel) = self.focused_channel() else {
                    self.status = "lost focused channel".into();
                    return;
                };
                let channel_id = channel.id.clone();
                let Some(detail) = &self.detail else {
                    self.status = "no detail panel loaded".into();
                    return;
                };
                let Some(msg) = detail.messages.last() else {
                    self.status = "no messages in channel".into();
                    return;
                };
                let ts = msg.ts.clone();
                match slack::chat_get_permalink(&self.auth, &channel_id, &ts) {
                    Ok(url) => {
                        let n = url.chars().count();
                        match crate::clipboard::copy(&url) {
                            Ok(()) => self.status = format!("copied permalink ({n} chars)"),
                            Err(e) => self.status = format!("copy failed: {e}"),
                        }
                    }
                    Err(e) => self.status = format!("error: {e}"),
                }
            }
            Some(Item::ThreadPlaceholder) | None => {
                self.status = "nothing to copy".into();
            }
        }
    }

    /// Tick — periodic background refresh on the current tab.
    pub fn tick(&mut self) -> bool {
        let idx = self.active_tab;
        let kind = self.tabs[idx].spec.kind.clone();
        // Search + threads don't auto-refresh.
        if kind == "search" || kind == "threads" {
            return false;
        }
        let interval = self.cfg.refresh_interval_secs;
        if interval == 0 {
            return false;
        }
        let stale = match self.tabs[idx].last_loaded {
            Some(t) => t.elapsed().as_secs() >= interval,
            None => true,
        };
        if stale && !self.tabs[idx].loading && self.input.is_none() {
            self.refresh_active(false);
            true
        } else {
            false
        }
    }

    pub fn focused_channel(&self) -> Option<&Channel> {
        match self.focused_item()? {
            Item::Channel(c) => Some(c),
            _ => None,
        }
    }

    pub fn resolve_user(&self, uid: &str) -> String {
        self.user_names
            .get(uid)
            .cloned()
            .unwrap_or_else(|| uid.to_string())
    }
}

/// Sort channels for the `channels` tab: members first, then unread
/// (last_read older than now → bumped up), then alphabetical.
fn sort_channels(mut chans: Vec<Channel>) -> Vec<Channel> {
    chans.sort_by(|a, b| {
        b.is_member
            .cmp(&a.is_member)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    chans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tab;

    #[test]
    fn tab_spec_resolves_known_kinds() {
        for kind in &["channels", "dms", "search", "threads"] {
            let t = Tab {
                name: "x".into(),
                kind: kind.to_string(),
                query: None,
            };
            assert!(TabSpec::resolve(&t).is_ok(), "{kind}");
        }
    }

    #[test]
    fn tab_spec_rejects_unknown() {
        let t = Tab {
            name: "x".into(),
            kind: "monkeys".into(),
            query: None,
        };
        assert!(TabSpec::resolve(&t).is_err());
    }

    #[test]
    fn sort_channels_puts_members_first() {
        let chans = vec![
            mk_channel("zebra", false),
            mk_channel("alpha", false),
            mk_channel("delta", true),
            mk_channel("bravo", true),
        ];
        let sorted = sort_channels(chans);
        assert!(sorted[0].is_member);
        assert!(sorted[1].is_member);
        assert!(!sorted[2].is_member);
        assert!(!sorted[3].is_member);
        assert_eq!(sorted[0].name, "bravo");
        assert_eq!(sorted[1].name, "delta");
    }

    fn mk_channel(name: &str, member: bool) -> Channel {
        Channel {
            id: name.to_uppercase(),
            name: name.to_string(),
            is_channel: true,
            is_group: false,
            is_im: false,
            is_mpim: false,
            is_private: false,
            is_archived: false,
            is_member: member,
            num_members: Some(1),
            topic: None,
            user: None,
            last_read: None,
            purpose: None,
        }
    }
}
