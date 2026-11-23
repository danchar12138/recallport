//! The one door to the memory: the MCP tools and the web pages both go through here. It owns the tree, the files
//! and the settings, turns nodes into the shapes people and AIs read, and keeps who-did-what on every change.
use crate::{
    embedding::Embed,
    files::Files,
    tree::{
        Attachment, Draft, Event, Hit, Maintenance, Node, Patch, STRONG, SearchOpts, Status,
        Storage, Tree, no_strong_match,
    },
};
use anyhow::{Context, Result, ensure};
use fs4::fs_std::FileExt;
use serde::Serialize;
use std::{
    collections::HashMap,
    fs::OpenOptions,
    path::{Path, PathBuf},
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

pub struct Service {
    tree: RwLock<Tree>,
    pub dir: PathBuf,
    pub files: Files,
    _lock: std::fs::File,
}

/// A node in a list: enough to recognise it and to know where it sits.
#[derive(Clone, Debug, Serialize)]
pub struct Brief {
    pub id: String,
    /// Its title, or for a memory without one the start of its text.
    pub title: String,
    pub layer: u32,
    pub occurred: String,
    pub updated: String,
    pub children: usize,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AttachmentView {
    pub hash: String,
    pub name: String,
    pub caption: String,
    pub mime: String,
    pub size: u64,
    pub storage: Storage,
    /// False when the stored copy or the referenced original is gone.
    pub available: bool,
    /// Where the bytes are on this computer.
    pub path: String,
    /// Address for viewing or downloading in the web page.
    pub url: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Revision {
    pub version: u32,
    pub op: String,
    pub at: String,
    pub actor: String,
    pub reason: String,
    pub title: String,
    pub content: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct NodeView {
    pub id: String,
    /// The title as stored; empty for a memory that has none.
    pub title: String,
    /// What to call it where one line has to do: the title, or the start of the text.
    pub label: String,
    pub content: String,
    pub tags: Vec<String>,
    pub parent: Option<String>,
    pub version: u32,
    pub occurred: String,
    pub created: String,
    pub updated: String,
    pub deleted: bool,
    pub layer: u32,
    /// Live nodes under it.
    pub descendants: usize,
    /// From the top of the tree down to the parent.
    pub path: Vec<Brief>,
    pub children: Vec<Brief>,
    pub refs: Vec<Brief>,
    pub referenced_by: Vec<Brief>,
    pub sources: Vec<String>,
    pub attachments: Vec<AttachmentView>,
    /// Newest first.
    pub revisions: Vec<Revision>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Link {
    pub id: String,
    pub title: String,
    /// The start of its text (only for the best-matching children that come with a summary).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub snippet: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct HitView {
    pub id: String,
    pub layer: u32,
    pub relevance: f64,
    /// Clearly about the question (exact words, or very close in meaning).
    pub strong: bool,
    pub exact: bool,
    pub matched: &'static str,
    pub title: String,
    pub content: String,
    pub truncated: bool,
    pub tags: Vec<String>,
    pub occurred: String,
    /// How many times its text was rewritten: the earlier versions are in its history.
    #[serde(skip_serializing_if = "is_zero")]
    pub rewrites: usize,
    /// Summaries above it, nearest first.
    pub path: Vec<Link>,
    /// For a summary: the children that match the question best.
    pub expanded: Vec<Link>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Clone, Debug, Serialize)]
pub struct SearchResult {
    pub hits: Vec<HitView>,
    /// True when nothing is clearly about the question: the hits are only clues.
    pub no_strong_match: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct LayerStat {
    pub layer: u32,
    pub count: usize,
    /// How many of them sit under a parent.
    pub covered: usize,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct FileStats {
    pub stored: usize,
    pub referenced: usize,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Stats {
    pub nodes: usize,
    pub deleted: usize,
    pub roots: usize,
    pub layers: Vec<LayerStat>,
    pub tags: Vec<(String, usize)>,
    pub files: FileStats,
    pub last_change: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChangeView {
    pub at: String,
    pub actor: String,
    pub op: String,
    pub reason: String,
    pub node: Brief,
}

#[derive(Clone, Debug, Serialize)]
pub struct StaleView {
    pub summary: Brief,
    pub newer: Vec<Brief>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExtendView {
    pub summary: Brief,
    pub nodes: Vec<Brief>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GapsView {
    pub layer: u32,
    /// Nodes that fit under a summary that already exists.
    pub extend: Vec<ExtendView>,
    /// Batches that want a new summary.
    pub groups: Vec<Vec<Brief>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MaintenanceView {
    pub layers: Vec<LayerStat>,
    /// Summaries without a title.
    pub untitled: Vec<Brief>,
    pub stale: Vec<StaleView>,
    pub gaps: Option<GapsView>,
}

/// Where a node stands among the nodes with the same parent (the top-level nodes for one without a parent), in
/// the order the tree shows them: by date.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Place {
    /// How many nodes there are with this parent, itself included.
    pub count: usize,
    /// Its own position, from 0.
    pub index: usize,
    /// The nearest ones before it (the nearest last) and after it (the nearest first).
    pub before: Vec<Brief>,
    pub after: Vec<Brief>,
}

/// How many neighbours on each side `Service::place` lists.
const NEIGHBOURS: usize = 4;

/// A node of the whole tree in the shape the zoomable picture wants.
#[derive(Clone, Debug, Serialize)]
pub struct OutlineNode {
    pub id: String,
    pub title: String,
    /// The start of its text, for drawing a card.
    pub snippet: String,
    pub layer: u32,
    /// Nodes at or under it.
    pub size: usize,
    /// The first and last date (`YYYY-MM-DD`) anything at or under it happened; empty when none is dated.
    pub from: String,
    pub to: String,
    /// `None` when not loaded yet (ask for it with the node as `root`); empty for a node with nothing under it.
    pub children: Option<Vec<OutlineNode>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Order {
    #[default]
    Updated,
    Created,
    Occurred,
}

#[derive(Clone, Debug, Default)]
pub struct ListQuery {
    pub tag: Option<String>,
    pub min_layer: Option<u32>,
    pub max_layer: Option<u32>,
    pub since: String,
    pub until: String,
    pub order: Order,
    pub offset: usize,
    pub limit: usize,
}

/// A file to attach to a node: by path on this computer, or as inline bytes (a web upload, a base64 snippet).
#[derive(Clone, Debug, Default)]
pub struct AttachInput {
    pub path: Option<String>,
    pub data_base64: Option<String>,
    pub bytes: Option<Vec<u8>>,
    pub name: Option<String>,
    pub caption: String,
    /// Only remember where the file is instead of copying it in (paths only).
    pub reference: bool,
}

#[derive(Clone, Debug, Default)]
pub struct WriteInput {
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub parent: Option<String>,
    pub refs: Vec<String>,
    pub sources: Vec<String>,
    pub occurred: String,
    pub adopt: Vec<String>,
    pub attachments: Vec<AttachInput>,
}

#[derive(Clone, Debug, Default)]
pub struct UpdateInput {
    pub title: Option<String>,
    pub content: Option<String>,
    pub tags: Option<Vec<String>>,
    pub occurred: Option<String>,
    pub refs: Option<Vec<String>>,
    pub sources: Option<Vec<String>>,
    pub add_attachments: Vec<AttachInput>,
    /// Hashes of attachments to take off.
    pub remove_attachments: Vec<String>,
}

/// How much of its text a node carries in the outline: enough to fill a card on the tree page.
pub const OUTLINE_TEXT: usize = 600;

fn snippet(text: &str, chars: usize) -> String {
    let text = text.trim();
    let mut out: String = text.chars().take(chars).collect();
    if text.chars().count() > chars {
        out.push('…');
    }
    out
}

impl Service {
    pub fn open(dir: &Path, embed: Arc<dyn Embed>) -> Result<Arc<Self>> {
        std::fs::create_dir_all(dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("recallport.lock"))?;
        ensure!(
            FileExt::try_lock_exclusive(&lock)?,
            "此记忆库已被另一个 RecallPort 进程打开"
        );
        let files = Files::new(dir);
        files.clean_temporary();
        Ok(Arc::new(Self {
            tree: RwLock::new(Tree::open(dir, embed)?),
            dir: dir.to_owned(),
            files,
            _lock: lock,
        }))
    }

    pub fn read(&self) -> RwLockReadGuard<'_, Tree> {
        self.tree.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn write_lock(&self) -> RwLockWriteGuard<'_, Tree> {
        self.tree.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Run blocking work (model, disk) off the async threads.
    pub async fn run<R: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(&Service) -> R + Send + 'static,
    ) -> Result<R> {
        let this = self.clone();
        Ok(tokio::task::spawn_blocking(move || work(&this)).await?)
    }

    // ---------- shapes ----------

    fn brief(&self, t: &Tree, n: &Node, layers: Option<&HashMap<String, u32>>) -> Brief {
        Brief {
            id: n.id.clone(),
            title: n.label(),
            layer: layers.map_or_else(|| t.layer(&n.id), |l| l.get(&n.id).copied().unwrap_or(0)),
            occurred: n.occurred.clone(),
            updated: n.updated.clone(),
            children: t.children(&n.id).len(),
            tags: n.tags.clone(),
        }
    }

    fn briefs(&self, t: &Tree, ids: &[String]) -> Vec<Brief> {
        ids.iter()
            .filter_map(|id| t.live(id))
            .map(|n| self.brief(t, n, None))
            .collect()
    }

    fn attachment_view(&self, a: &Attachment) -> AttachmentView {
        AttachmentView {
            hash: a.hash.clone(),
            name: a.name.clone(),
            caption: a.caption.clone(),
            mime: a.mime.clone(),
            size: a.size,
            storage: a.storage,
            available: self.files.available(a),
            path: self.files.path_of(a).to_string_lossy().into_owned(),
            url: format!("/file/{}", a.hash),
        }
    }

    // ---------- reading ----------

    pub fn get(&self, id: &str) -> Result<NodeView> {
        let t = self.read();
        let n = t.node(id).with_context(|| format!("没有这个节点：{id}"))?;
        let deleted = n.status == Status::Deleted;
        let mut path: Vec<Brief> = t
            .ancestors(id)
            .into_iter()
            .map(|a| self.brief(&t, a, None))
            .collect();
        path.reverse();
        let referenced_by: Vec<String> = t
            .nodes()
            .filter(|o| o.refs.iter().any(|r| r == id))
            .map(|o| o.id.clone())
            .collect();
        let mut revisions: Vec<Revision> = t
            .history(id)
            .into_iter()
            .map(|e| Revision {
                version: e.node.version,
                op: e.op.clone(),
                at: e.at.clone(),
                actor: e.actor.clone(),
                reason: e.reason.clone(),
                title: e.node.title.clone(),
                content: e.node.content.clone(),
            })
            .collect();
        revisions.reverse();
        Ok(NodeView {
            id: n.id.clone(),
            title: n.title.clone(),
            label: n.label(),
            content: n.content.clone(),
            tags: n.tags.clone(),
            parent: n.parent.clone(),
            version: n.version,
            occurred: n.occurred.clone(),
            created: n.created.clone(),
            updated: n.updated.clone(),
            deleted,
            layer: t.layer(id),
            descendants: if deleted { 0 } else { t.subtree(id) - 1 },
            path,
            children: self.briefs(&t, t.children(id)),
            refs: self.briefs(&t, &n.refs),
            referenced_by: self.briefs(&t, &referenced_by),
            sources: n.sources.clone(),
            attachments: n
                .attachments
                .iter()
                .map(|a| self.attachment_view(a))
                .collect(),
            revisions,
        })
    }

    /// The place of a node among its siblings.
    pub fn place(&self, id: &str) -> Result<Place> {
        let t = self.read();
        let n = t.live(id).with_context(|| format!("没有这个节点：{id}"))?;
        let ids: Vec<String> = match n.parent.as_deref() {
            Some(parent) => t.children(parent).to_vec(),
            None => {
                let mut roots: Vec<&Node> = t.nodes().filter(|o| o.parent.is_none()).collect();
                roots.sort_by(|a, b| a.occurred.cmp(&b.occurred).then_with(|| a.id.cmp(&b.id)));
                roots.into_iter().map(|o| o.id.clone()).collect()
            }
        };
        let index = ids.iter().position(|o| o == id).unwrap_or(0);
        let after_from = (index + 1).min(ids.len());
        Ok(Place {
            count: ids.len(),
            index,
            before: self.briefs(&t, &ids[index.saturating_sub(NEIGHBOURS)..index]),
            after: self.briefs(
                &t,
                &ids[after_from..(after_from + NEIGHBOURS).min(ids.len())],
            ),
        })
    }

    /// The start of the text of each node, in the order of `ids` (empty for one that is gone).
    pub fn leads(&self, ids: &[String], chars: usize) -> Vec<String> {
        let t = self.read();
        ids.iter()
            .map(|id| {
                t.live(id)
                    .map(|n| snippet(&n.content, chars))
                    .unwrap_or_default()
            })
            .collect()
    }

    pub fn search(&self, opts: &SearchOpts) -> Result<SearchResult> {
        let t = self.read();
        let hits: Vec<Hit> = t.search(opts)?;
        let strong = !no_strong_match(&hits);
        let link = |id: &str, title: &str, snippet: &str| Link {
            id: id.to_owned(),
            title: title.to_owned(),
            snippet: snippet.to_owned(),
        };
        let views = hits
            .into_iter()
            .map(|h| {
                let node = t.live(&h.id);
                HitView {
                    layer: h.layer,
                    relevance: (f64::from(h.relevance) * 100.0).round() / 100.0,
                    strong: h.exact || h.relevance >= STRONG,
                    exact: h.exact,
                    matched: h.matched,
                    title: h.title,
                    content: h.content,
                    truncated: h.truncated,
                    tags: node.map(|n| n.tags.clone()).unwrap_or_default(),
                    occurred: node.map(|n| n.occurred.clone()).unwrap_or_default(),
                    rewrites: t.history(&h.id).iter().filter(|e| e.op == "update").count(),
                    path: h
                        .path
                        .iter()
                        .map(|(id, title)| link(id, title, ""))
                        .collect(),
                    expanded: h
                        .expanded
                        .iter()
                        .map(|(id, title, text)| link(id, title, text))
                        .collect(),
                    id: h.id,
                }
            })
            .collect();
        Ok(SearchResult {
            hits: views,
            no_strong_match: !strong,
        })
    }

    pub fn list(&self, q: &ListQuery) -> Page<Brief> {
        let t = self.read();
        let layers = t.layers();
        let mut nodes: Vec<&Node> = t
            .nodes()
            .filter(|n| q.tag.as_ref().is_none_or(|tag| n.tags.contains(tag)))
            .filter(|n| {
                let layer = layers.get(&n.id).copied().unwrap_or(0);
                q.min_layer.is_none_or(|m| layer >= m) && q.max_layer.is_none_or(|m| layer <= m)
            })
            .filter(|n| {
                if q.since.is_empty() && q.until.is_empty() {
                    return true;
                }
                if n.occurred.is_empty() {
                    return false;
                }
                let head = n.occurred.get(..q.until.len()).unwrap_or(&n.occurred);
                (q.since.is_empty() || n.occurred.as_str() >= q.since.as_str())
                    && (q.until.is_empty() || head <= q.until.as_str())
            })
            .collect();
        match q.order {
            Order::Updated => {
                nodes.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)))
            }
            Order::Created => {
                nodes.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.id.cmp(&b.id)))
            }
            Order::Occurred => {
                nodes.sort_by(|a, b| b.occurred.cmp(&a.occurred).then_with(|| a.id.cmp(&b.id)))
            }
        }
        let total = nodes.len();
        let limit = if q.limit == 0 { 30 } else { q.limit };
        Page {
            items: nodes
                .into_iter()
                .skip(q.offset)
                .take(limit)
                .map(|n| self.brief(&t, n, Some(&layers)))
                .collect(),
            total,
        }
    }

    /// An attachment by its hash, for serving the file (it must belong to a node that still exists).
    pub fn attachment(&self, hash: &str) -> Option<AttachmentView> {
        let t = self.read();
        t.nodes()
            .flat_map(|n| n.attachments.iter())
            .find(|a| a.hash == hash)
            .map(|a| self.attachment_view(a))
    }

    pub fn trash(&self) -> Vec<Brief> {
        let t = self.read();
        t.trash()
            .into_iter()
            .map(|n| Brief {
                id: n.id.clone(),
                title: n.label(),
                layer: 0,
                occurred: n.occurred.clone(),
                updated: n.updated.clone(),
                children: 0,
                tags: n.tags.clone(),
            })
            .collect()
    }

    pub fn stats(&self) -> Stats {
        let t = self.read();
        let layers = t.layers();
        let mut counts: std::collections::BTreeMap<u32, (usize, usize)> =
            std::collections::BTreeMap::new();
        let mut roots = 0;
        for n in t.nodes() {
            let at = counts
                .entry(layers.get(&n.id).copied().unwrap_or(0))
                .or_default();
            at.0 += 1;
            if n.parent.is_some() {
                at.1 += 1;
            } else {
                roots += 1;
            }
        }
        let mut files = FileStats::default();
        let mut seen = std::collections::HashSet::new();
        for a in t.nodes().flat_map(|n| n.attachments.iter()) {
            if !seen.insert(a.hash.clone()) {
                continue;
            }
            match a.storage {
                Storage::Stored => files.stored += 1,
                Storage::Referenced => files.referenced += 1,
            }
            files.bytes += a.size;
        }
        Stats {
            nodes: t.len(),
            deleted: t.trash().len(),
            roots,
            layers: counts
                .into_iter()
                .map(|(layer, (count, covered))| LayerStat {
                    layer,
                    count,
                    covered,
                })
                .collect(),
            tags: t.tags().into_iter().take(30).collect(),
            files,
            last_change: t.last_change().to_owned(),
        }
    }

    pub fn activity(&self, limit: usize) -> Vec<ChangeView> {
        let t = self.read();
        let layers = t.layers();
        t.activity(limit)
            .into_iter()
            .map(|e| ChangeView {
                at: e.at.clone(),
                actor: e.actor.clone(),
                op: e.op.clone(),
                reason: e.reason.clone(),
                node: self.brief(&t, &e.node, Some(&layers)),
            })
            .collect()
    }

    pub fn maintenance(&self) -> MaintenanceView {
        let t = self.read();
        let m: Maintenance = t.maintenance();
        let layers = t.layers();
        let b = |id: &str| t.live(id).map(|n| self.brief(&t, n, Some(&layers)));
        let many = |ids: &[String]| ids.iter().filter_map(|id| b(id)).collect::<Vec<_>>();
        MaintenanceView {
            layers: m
                .layers
                .iter()
                .map(|(layer, count, covered)| LayerStat {
                    layer: *layer,
                    count: *count,
                    covered: *covered,
                })
                .collect(),
            untitled: many(&m.untitled),
            stale: m
                .stale
                .iter()
                .filter_map(|(id, newer)| {
                    Some(StaleView {
                        summary: b(id)?,
                        newer: many(newer),
                    })
                })
                .collect(),
            gaps: m.gaps.map(|(layer, extend, groups)| GapsView {
                layer,
                extend: extend
                    .iter()
                    .filter_map(|(id, nodes)| {
                        Some(ExtendView {
                            summary: b(id)?,
                            nodes: many(nodes),
                        })
                    })
                    .collect(),
                groups: groups.iter().map(|g| many(g)).collect(),
            }),
        }
    }

    /// The tree for the zoomable picture, `depth` levels deep: from the top when `root` is `None`, else from that
    /// node. A node below the depth comes without its children (`children: None`) and is asked for later with
    /// itself as `root`. Texts are short; a node's full text comes from `get`.
    pub fn outline(&self, root: Option<&str>, depth: usize) -> Result<Vec<OutlineNode>> {
        let t = self.read();
        let layers = t.layers();
        fn build(
            t: &Tree,
            layers: &HashMap<String, u32>,
            id: &str,
            depth: usize,
        ) -> Option<OutlineNode> {
            let n = t.live(id)?;
            let kids: Vec<OutlineNode> = t
                .children(id)
                .iter()
                .filter_map(|c| build(t, layers, c, depth.saturating_sub(1)))
                .collect();
            let dates = std::iter::once(&n.occurred)
                .chain(kids.iter().flat_map(|k| [&k.from, &k.to]))
                .filter(|d| !d.is_empty())
                .map(|d| d.chars().take(10).collect::<String>());
            let (from, to) = dates.fold((String::new(), String::new()), |(lo, hi), d| {
                (
                    if lo.is_empty() || d < lo {
                        d.clone()
                    } else {
                        lo
                    },
                    if d > hi { d } else { hi },
                )
            });
            Some(OutlineNode {
                id: n.id.clone(),
                title: n.label(),
                snippet: snippet(&n.content, OUTLINE_TEXT),
                layer: layers.get(id).copied().unwrap_or(0),
                size: 1 + kids.iter().map(|c| c.size).sum::<usize>(),
                from,
                to,
                children: (depth > 0 || kids.is_empty()).then_some(kids),
            })
        }
        if let Some(root) = root {
            t.live(root)
                .with_context(|| format!("没有这个节点：{root}"))?;
            return Ok(build(&t, &layers, root, depth).into_iter().collect());
        }
        let mut roots: Vec<&Node> = t.nodes().filter(|n| n.parent.is_none()).collect();
        roots.sort_by(|a, b| a.occurred.cmp(&b.occurred).then_with(|| a.id.cmp(&b.id)));
        Ok(roots
            .into_iter()
            .filter_map(|n| build(&t, &layers, &n.id, depth))
            .collect())
    }

    // ---------- changing ----------

    fn ingest(&self, input: &AttachInput) -> Result<Attachment> {
        if let Some(path) = &input.path {
            return self.files.ingest_path(
                path,
                input.name.as_deref(),
                &input.caption,
                input.reference,
            );
        }
        let name = input.name.clone().unwrap_or_else(|| "file".into());
        if let Some(bytes) = &input.bytes {
            return self.files.ingest_bytes(&name, bytes, &input.caption);
        }
        if let Some(data) = &input.data_base64 {
            return self.files.ingest_base64(&name, data, &input.caption);
        }
        anyhow::bail!("附件要给出 path 或 data_base64")
    }

    pub fn write(&self, input: WriteInput, actor: &str) -> Result<NodeView> {
        let attachments = input
            .attachments
            .iter()
            .map(|a| self.ingest(a))
            .collect::<Result<Vec<_>>>()?;
        let node = self.write_lock().write(
            Draft {
                title: input.title,
                content: input.content,
                tags: input.tags,
                parent: input.parent,
                refs: input.refs,
                sources: input.sources,
                attachments,
                occurred: input.occurred,
                adopt: input.adopt,
            },
            actor,
        )?;
        self.get(&node.id)
    }

    /// `expect` is the version the caller read; without it the latest one is assumed.
    pub fn update(
        &self,
        id: &str,
        expect: Option<u32>,
        input: UpdateInput,
        actor: &str,
    ) -> Result<NodeView> {
        let added = input
            .add_attachments
            .iter()
            .map(|a| self.ingest(a))
            .collect::<Result<Vec<_>>>()?;
        {
            let mut t = self.write_lock();
            let node = t
                .live(id)
                .with_context(|| format!("没有这个节点：{id}"))?
                .clone();
            let attachments =
                (!added.is_empty() || !input.remove_attachments.is_empty()).then(|| {
                    let mut list: Vec<Attachment> = node
                        .attachments
                        .iter()
                        .filter(|a| !input.remove_attachments.contains(&a.hash))
                        .cloned()
                        .collect();
                    list.extend(added);
                    list
                });
            t.update(
                id,
                expect.unwrap_or(node.version),
                Patch {
                    title: input.title,
                    content: input.content,
                    tags: input.tags,
                    occurred: input.occurred,
                    refs: input.refs,
                    sources: input.sources,
                    attachments,
                },
                actor,
            )?;
        }
        self.get(id)
    }

    /// Mark a node as checked and still right.
    pub fn touch(&self, id: &str, expect: Option<u32>, actor: &str) -> Result<NodeView> {
        self.update(id, expect, UpdateInput::default(), actor)
    }

    pub fn move_nodes(&self, ids: &[String], to: Option<&str>, actor: &str) -> Result<()> {
        self.write_lock().reparent(ids, to, actor)
    }

    /// Returns how many children moved up.
    pub fn delete(
        &self,
        id: &str,
        expect: Option<u32>,
        reason: &str,
        actor: &str,
    ) -> Result<usize> {
        let mut t = self.write_lock();
        let version = match expect {
            Some(v) => v,
            None => {
                t.live(id)
                    .with_context(|| format!("没有这个节点：{id}"))?
                    .version
            }
        };
        Ok(t.delete(id, version, reason, actor)?.len())
    }

    pub fn restore(&self, id: &str, actor: &str) -> Result<NodeView> {
        self.write_lock().restore(id, actor)?;
        self.get(id)
    }

    /// Remove deleted nodes for good (and the files nothing mentions any more).
    pub fn purge(&self, ids: &[String]) -> Result<usize> {
        let mut t = self.write_lock();
        let n = t.purge(ids)?;
        self.files.sweep(&t.used_files());
        Ok(n)
    }

    pub fn empty_trash(&self) -> Result<usize> {
        let ids: Vec<String> = self
            .read()
            .trash()
            .into_iter()
            .map(|n| n.id.clone())
            .collect();
        self.purge(&ids)
    }

    /// Housekeeping: what has been in the recycle bin for a month goes.
    pub fn tidy(&self) -> Result<usize> {
        let mut t = self.write_lock();
        let n = t.purge_old(30)?;
        if n > 0 {
            self.files.sweep(&t.used_files());
        }
        Ok(n)
    }

    /// Every event, for a backup.
    pub fn export_events(&self) -> (Vec<Event>, Vec<u8>) {
        let t = self.read();
        (t.all_events(), t.vector_bytes().unwrap_or_default())
    }
}
