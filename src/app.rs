//! State shared between the web server, the MCP handler and the tray.
use crate::store::now;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

pub struct App {
    /// Notified to stop the server and leave.
    pub quit: tokio::sync::Notify,
    /// True when a tray icon keeps the service resident.
    pub tray: bool,
    seen: Mutex<HashMap<String, String>>,
}

impl App {
    pub fn new(tray: bool) -> Arc<Self> {
        Arc::new(Self {
            quit: tokio::sync::Notify::new(),
            tray,
            seen: Mutex::new(HashMap::new()),
        })
    }

    /// An MCP client completed its handshake.
    pub fn saw_client(&self, name: &str) {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_owned(), now());
    }

    /// When a client whose self-reported name contains `needle` last connected.
    pub fn last_seen(&self, needle: &str) -> Option<(String, String)> {
        let needle = needle.to_lowercase();
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(name, _)| name.to_lowercase().contains(&needle))
            .max_by(|a, b| a.1.cmp(b.1))
            .map(|(name, at)| (name.clone(), at.clone()))
    }
}
