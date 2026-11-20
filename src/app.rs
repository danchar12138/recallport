//! State shared between the web server, the MCP handler and the tray.
use crate::store::now;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub struct App {
    /// Notified to stop the server and leave.
    pub quit: tokio::sync::Notify,
    /// True when a tray icon keeps the service resident.
    pub tray: bool,
    seen: Mutex<HashMap<String, Seen>>,
    file: Option<PathBuf>,
}

/// What is known about one MCP client, by the name it gave us.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Seen {
    pub at: String,
    pub calls: u64,
    /// The User-Agent header it last used, when it sent one.
    #[serde(default)]
    pub agent: String,
}

impl App {
    /// `dir` is where the list of seen clients is kept, so it survives a restart.
    pub fn new(tray: bool, dir: Option<PathBuf>) -> Arc<Self> {
        let file = dir.map(|d| d.join("clients.json"));
        let seen = file
            .as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Arc::new(Self {
            quit: tokio::sync::Notify::new(),
            tray,
            seen: Mutex::new(seen),
            file,
        })
    }

    /// An MCP request arrived. `name` is the client's own name from the handshake when known,
    /// `agent` the User-Agent header. Each request counts: clients that use plain requests never
    /// send a handshake.
    pub fn saw_client(&self, name: &str, agent: &str) {
        let name = name.trim();
        let agent = agent.trim();
        // The admin page tests its own endpoint; browsers are not AI clients.
        if name == "RecallPort Web" || (name.is_empty() && agent.starts_with("Mozilla")) {
            return;
        }
        let key = if !name.is_empty() {
            name.to_owned()
        } else {
            // "claude-code/2.1.0 (cli)" -> "claude-code"
            agent
                .split(|c: char| c == '/' || c.is_whitespace())
                .next()
                .unwrap_or("")
                .to_owned()
        };
        if key.is_empty() {
            return;
        }
        let at = now();
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let entry = seen.entry(key.clone()).or_default();
        let fresh = entry.calls == 0;
        entry.calls += 1;
        let minute_ago = entry.at.get(..16) != at.get(..16);
        entry.at = at;
        if !agent.is_empty() {
            entry.agent = agent.chars().take(120).collect();
        }
        if fresh {
            crate::log!("MCP client seen: {key} ({agent})");
        }
        // Writing at most once a minute per client keeps busy sessions cheap.
        if fresh || minute_ago {
            let json = serde_json::to_vec(&*seen).unwrap_or_default();
            drop(seen);
            if let Some(file) = &self.file {
                let _ = std::fs::write(file, json);
            }
        }
    }

    /// When a client whose name or User-Agent contains `needle` last connected.
    pub fn last_seen(&self, needle: &str) -> Option<(String, String)> {
        let needle = needle.to_lowercase();
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(name, s)| {
                name.to_lowercase().contains(&needle) || s.agent.to_lowercase().contains(&needle)
            })
            .max_by(|a, b| a.1.at.cmp(&b.1.at))
            .map(|(name, s)| (name.clone(), s.at.clone()))
    }

    /// Every client seen, most recent first.
    pub fn all_seen(&self) -> Vec<(String, Seen)> {
        let mut list: Vec<_> = self
            .seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        list.sort_by(|a, b| b.1.at.cmp(&a.1.at));
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clients_are_told_apart_by_handshake_name_or_user_agent_and_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::new(false, Some(dir.path().to_path_buf()));
        // Stateless clients never handshake: only the User-Agent identifies them.
        app.saw_client("", "claude-code/2.1.328 (claude-vscode, agent-sdk/0.3.328)");
        app.saw_client("codex-mcp-client", "");
        app.saw_client("", "");
        assert!(app.last_seen("claude").is_some());
        assert!(app.last_seen("codex").is_some());
        assert!(app.last_seen("cursor").is_none());
        assert_eq!(app.all_seen().len(), 2);

        let reopened = App::new(false, Some(dir.path().to_path_buf()));
        let (name, _) = reopened
            .last_seen("claude")
            .expect("remembered across restarts");
        assert_eq!(name, "claude-code");
    }
}
