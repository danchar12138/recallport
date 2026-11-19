//! One memory seen in full: its links in both directions, entities, attachments and correction trail.
//! Shared by the web detail page and the MCP `read` tool.
use crate::{files::AttachmentView, memory::*, store::Store};
use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize)]
pub struct Link {
    pub id: String,
    #[serde(skip_serializing_if = "RefKind::is_relates")]
    pub kind: RefKind,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
    pub occurred: String,
    pub recorded: String,
    pub source: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct MentionView {
    /// The name as the AI wrote it.
    pub name: String,
    /// Set once the mention points to an entity; empty while it still needs the AI's choice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// The entity's current name (it may have been renamed or merged since).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub entity_name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hint: String,
}

#[derive(Serialize)]
pub struct Detail {
    pub entry: Entry,
    pub links_out: Vec<Link>,
    pub links_in: Vec<Link>,
    pub mentions: Vec<MentionView>,
    pub attachments: Vec<AttachmentView>,
    pub trail: Vec<Trail>,
}

impl Store {
    pub async fn detail(&self, id: &str) -> Result<Detail> {
        let entry = self.read(id).await?;
        let snapshot = self.all().await?;
        let by_id: HashMap<&str, &Entry> = snapshot.iter().map(|e| (e.id.as_str(), e)).collect();
        let link = |r: &Reference| -> Option<Link> {
            let e = by_id.get(r.id.as_str())?;
            Some(Link {
                id: e.id.clone(),
                kind: r.kind,
                reason: r.reason.clone(),
                occurred: e.occurred.clone(),
                recorded: e.recorded.clone(),
                source: e.source.clone(),
                text: snippet(&e.text, 160),
            })
        };
        let links_out = entry.references.iter().filter_map(link).collect();
        let links_in = entry.cited_by.iter().filter_map(link).collect();
        let mentions = {
            let idx = self.idx.read().await;
            entry
                .mentions
                .iter()
                .map(|m| {
                    let entity = m.entity.as_deref().and_then(|id| idx.canonical(id));
                    MentionView {
                        name: m.name.clone(),
                        entity: entity.map(|e| e.id.clone()),
                        entity_name: entity.map(|e| e.name.clone()).unwrap_or_default(),
                        hint: entity.map(|e| e.hint.clone()).unwrap_or_default(),
                    }
                })
                .collect()
        };
        let attachments = self.attachment_views(&entry.attachments).await;
        let trail = self.trail(&entry, &by_id).await;
        Ok(Detail {
            entry,
            links_out,
            links_in,
            mentions,
            attachments,
            trail,
        })
    }
}
