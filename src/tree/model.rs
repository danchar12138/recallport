//! The one thing the tree stores: a node. Layer 0 nodes are what the AI extracts from conversations; a node of a
//! higher layer is what the AI distilled from the nodes under it. Nothing about a node is permanent: the AI may
//! rewrite, move or delete it, and every such change is an event in an append-only log, so nothing is lost until
//! a deleted node is purged on purpose.
use serde::{Deserialize, Serialize};

pub const MAX_CONTENT: usize = 4000;
pub const MAX_LINKS: usize = 64;
pub const MAX_TAGS: usize = 12;
pub const MAX_SOURCES: usize = 16;
pub const MAX_ATTACHMENTS: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Live,
    /// In the recycle bin: out of every search and listing, kept until it is purged.
    Deleted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Storage {
    /// A copy lives in the data folder.
    Stored,
    /// Only the place of the original is remembered.
    Referenced,
}

/// A file, image, recording or document that belongs to a node. The bytes never travel in the content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    /// SHA-256 of the bytes; also the name of the stored copy.
    pub hash: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caption: String,
    pub mime: String,
    pub size: u64,
    pub storage: Storage,
    /// Where the original is, for referenced files.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub content: String,
    /// Cross-cutting labels (a project, a device, a person). Only searched, never structural.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The one node this one is summarized under; the chain of parents is the way up the tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Related nodes elsewhere in the tree (a memory often belongs to more than one story).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<String>,
    /// Where the content came from (a conversation, a file, a URL); lets a summary be traced to its evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    pub version: u32,
    /// When it happened (`YYYY-MM-DD` or a longer ISO time); empty when unknown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub occurred: String,
    pub created: String,
    /// Last change of anything about the node, including having been checked and found still right.
    pub updated: String,
    pub status: Status,
}

/// One line of the log: the node as it was after the change.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub at: String,
    pub actor: String,
    /// write, update, touch, reparent, delete, restore or import.
    pub op: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    pub node: Node,
}

/// A new node. `adopt` lists existing nodes that become its children: that is how a summary is written.
#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub content: String,
    pub tags: Vec<String>,
    pub parent: Option<String>,
    pub refs: Vec<String>,
    pub sources: Vec<String>,
    pub attachments: Vec<Attachment>,
    pub occurred: String,
    pub adopt: Vec<String>,
}

/// Changes to a node; what is `None` stays as it is. An empty patch only marks the node as checked.
#[derive(Clone, Debug, Default)]
pub struct Patch {
    pub content: Option<String>,
    pub tags: Option<Vec<String>>,
    pub occurred: Option<String>,
    pub refs: Option<Vec<String>>,
    pub sources: Option<Vec<String>>,
    pub attachments: Option<Vec<Attachment>>,
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        self.content.is_none()
            && self.tags.is_none()
            && self.occurred.is_none()
            && self.refs.is_none()
            && self.sources.is_none()
            && self.attachments.is_none()
    }
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}
