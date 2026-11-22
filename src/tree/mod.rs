//! The memory tree: every memory is a node, a node has at most one parent (the summary it sits under), and the
//! AI may write, update, move and delete nodes freely. The file `nodes.jsonl` is an append-only log of every
//! change; the tree in memory is just its latest state, so any mistake can be traced and undone.
#![allow(dead_code)] // reached only from `--tree-eval` and the tests until the MCP tools and the page are wired

pub mod eval;
mod import;
mod lexicon;
mod maintain;
mod model;
mod search;

pub use model::{Draft, Event, Node, Patch, Status};
pub use search::SearchOpts;

use crate::{
    embedding::{self, Embed},
    vectors::VectorCache,
};
use anyhow::{Context, Result, bail, ensure};
use lexicon::Lexicon;
use model::{MAX_CONTENT, MAX_LINKS, MAX_SOURCES, MAX_TAGS, now};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

/// How many nodes one call may move under a new parent.
pub const MAX_MOVE: usize = 64;

pub struct Tree {
    dir: PathBuf,
    embed: Arc<dyn Embed>,
    /// Every node ever written, deleted ones included.
    nodes: HashMap<String, Node>,
    /// Live children of each node, oldest first.
    kids: HashMap<String, Vec<String>>,
    vectors: HashMap<String, Vec<f32>>,
    lex: Lexicon,
    history: HashMap<String, Vec<Event>>,
    log: File,
}

/// What the text of a node is searched by: the content and its tags.
fn index_text(n: &Node) -> String {
    format!("{}\n{}", n.content, n.tags.join(" "))
}

fn clean_list(list: &[String], max: usize, what: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = vec![];
    for item in list.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if !out.iter().any(|x| x == item) {
            out.push(item.to_owned());
        }
    }
    ensure!(out.len() <= max, "{what}最多 {max} 个");
    Ok(out)
}

fn clean_content(content: &str) -> Result<String> {
    let content = content.trim();
    ensure!(!content.is_empty(), "内容不能为空");
    ensure!(
        content.chars().count() <= MAX_CONTENT,
        "内容最多 {MAX_CONTENT} 个字"
    );
    Ok(content.to_owned())
}

impl Tree {
    pub fn open(dir: &Path, embed: Arc<dyn Embed>) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("nodes.jsonl");
        let mut nodes = HashMap::new();
        let mut history: HashMap<String, Vec<Event>> = HashMap::new();
        if path.exists() {
            for (n, line) in BufReader::new(File::open(&path)?).lines().enumerate() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let event: Event = serde_json::from_str(&line)
                    .with_context(|| format!("nodes.jsonl 第 {} 行损坏", n + 1))?;
                nodes.insert(event.node.id.clone(), event.node.clone());
                history
                    .entry(event.node.id.clone())
                    .or_default()
                    .push(event);
            }
        }
        let log = OpenOptions::new().create(true).append(true).open(&path)?;
        let mut tree = Self {
            dir: dir.to_owned(),
            embed,
            nodes,
            kids: HashMap::new(),
            vectors: HashMap::new(),
            lex: Lexicon::default(),
            history,
            log,
        };
        tree.rebuild()?;
        Ok(tree)
    }

    /// Kids, the keyword index and the vectors, from the nodes. Vectors that were saved for the same text are reused.
    fn rebuild(&mut self) -> Result<()> {
        self.kids.clear();
        self.lex = Lexicon::default();
        let cache = std::fs::read(self.dir.join("vectors.bin"))
            .ok()
            .and_then(|bytes| VectorCache::decode(&bytes, embedding::ID).ok())
            .unwrap_or_default();
        let mut missing: Vec<String> = vec![];
        let mut live: Vec<&Node> = self
            .nodes
            .values()
            .filter(|n| n.status == Status::Live)
            .collect();
        live.sort_by(|a, b| a.id.cmp(&b.id));
        for n in &live {
            self.lex.add(&n.id, &index_text(n));
            match cache
                .get(&n.id, &n.content)
                .and_then(|v| v.into_iter().next())
            {
                Some(v) => {
                    self.vectors.insert(n.id.clone(), v);
                }
                None => missing.push(n.id.clone()),
            }
            if let Some(parent) = &n.parent {
                self.kids
                    .entry(parent.clone())
                    .or_default()
                    .push(n.id.clone());
            }
        }
        for batch in missing.chunks(12) {
            let texts: Vec<String> = batch
                .iter()
                .map(|id| self.nodes[id].content.clone())
                .collect();
            for (id, v) in batch.iter().zip(self.embed.encode_many(&texts)?) {
                self.vectors.insert(id.clone(), v);
            }
        }
        let nodes = &self.nodes;
        for list in self.kids.values_mut() {
            list.sort_by(|a, b| (&nodes[a].occurred, a).cmp(&(&nodes[b].occurred, b)));
        }
        if !missing.is_empty() {
            self.flush()?;
        }
        Ok(())
    }

    /// Save the vectors next to the log so the next start does not run the model again.
    pub fn flush(&self) -> Result<()> {
        let mut cache = VectorCache::default();
        for (id, v) in &self.vectors {
            cache.insert(id, &self.nodes[id].content, std::slice::from_ref(v));
        }
        std::fs::write(self.dir.join("vectors.bin"), cache.encode(embedding::ID))?;
        Ok(())
    }

    // ---------- reading ----------

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id)
    }

    pub fn live(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id).filter(|n| n.status == Status::Live)
    }

    pub fn len(&self) -> usize {
        self.nodes
            .values()
            .filter(|n| n.status == Status::Live)
            .count()
    }

    pub fn children(&self, id: &str) -> &[String] {
        self.kids.get(id).map_or(&[], Vec::as_slice)
    }

    /// The layer of a node: 0 for one with nothing under it, else one more than its tallest child.
    pub fn layer(&self, id: &str) -> u32 {
        self.children(id)
            .iter()
            .map(|c| self.layer(c) + 1)
            .max()
            .unwrap_or(0)
    }

    /// Parents, nearest first.
    pub fn ancestors(&self, id: &str) -> Vec<&Node> {
        let mut out = vec![];
        let mut at = self.live(id).and_then(|n| n.parent.as_deref());
        while let Some(p) = at {
            let Some(n) = self.live(p) else { break };
            if out.len() > self.nodes.len() {
                break;
            }
            out.push(n);
            at = n.parent.as_deref();
        }
        out
    }

    /// Is `id` the node `top` itself or somewhere under it?
    pub fn within(&self, id: &str, top: &str) -> bool {
        id == top || self.ancestors(id).iter().any(|n| n.id == top)
    }

    /// Every revision of a node, oldest first.
    pub fn history(&self, id: &str) -> Vec<&Event> {
        self.history
            .get(id)
            .map(|h| h.iter().collect())
            .unwrap_or_default()
    }

    /// When the content of a node last changed (moving it around does not count).
    pub fn changed_at(&self, id: &str) -> &str {
        self.last_event(id, &["write", "update", "import", "restore"])
    }

    /// When a node was last written, changed or marked as checked.
    pub fn checked_at(&self, id: &str) -> &str {
        self.last_event(id, &["write", "update", "touch", "import", "restore"])
    }

    fn last_event(&self, id: &str, ops: &[&str]) -> &str {
        self.history
            .get(id)
            .and_then(|h| h.iter().rev().find(|e| ops.contains(&e.op.as_str())))
            .map_or("", |e| e.at.as_str())
    }

    /// The recycle bin: deleted nodes, newest first.
    pub fn trash(&self) -> Vec<&Node> {
        let mut out: Vec<&Node> = self
            .nodes
            .values()
            .filter(|n| n.status == Status::Deleted)
            .collect();
        out.sort_by(|a, b| b.updated.cmp(&a.updated));
        out
    }

    // ---------- changing ----------

    fn must_live(&self, id: &str) -> Result<&Node> {
        self.live(id).with_context(|| format!("没有这个节点：{id}"))
    }

    fn check_version(node: &Node, expect: u32) -> Result<()> {
        ensure!(
            node.version == expect,
            "节点已被改动（现在是第 {} 版，你拿的是第 {expect} 版），请重新读取再改",
            node.version
        );
        Ok(())
    }

    fn check_links(&self, refs: &[String], own: Option<&str>) -> Result<()> {
        for r in refs {
            ensure!(Some(r.as_str()) != own, "节点不能引用自己");
            self.must_live(r)?;
        }
        Ok(())
    }

    /// Record a new state of a node and make the in-memory tree match.
    fn put(
        &mut self,
        node: Node,
        vector: Option<Vec<f32>>,
        op: &str,
        actor: &str,
        reason: &str,
    ) -> Result<()> {
        let event = Event {
            at: now(),
            actor: actor.to_owned(),
            op: op.to_owned(),
            reason: reason.to_owned(),
            node: node.clone(),
        };
        writeln!(self.log, "{}", serde_json::to_string(&event)?)?;
        self.log.flush()?;
        if let Some(old) = self.nodes.get(&node.id)
            && old.status == Status::Live
            && let Some(parent) = &old.parent
            && let Some(list) = self.kids.get_mut(parent)
        {
            list.retain(|k| k != &node.id);
        }
        let id = node.id.clone();
        if node.status == Status::Live {
            self.lex.add(&id, &index_text(&node));
            if let Some(v) = vector {
                self.vectors.insert(id.clone(), v);
            }
            if let Some(parent) = &node.parent {
                self.kids
                    .entry(parent.clone())
                    .or_default()
                    .push(id.clone());
            }
        } else {
            self.lex.remove(&id);
            self.vectors.remove(&id);
        }
        self.nodes.insert(id.clone(), node);
        if let Some(parent) = self.nodes[&id].parent.clone() {
            let nodes = &self.nodes;
            if let Some(list) = self.kids.get_mut(&parent) {
                list.sort_by(|a, b| (&nodes[a].occurred, a).cmp(&(&nodes[b].occurred, b)));
            }
        }
        self.history.entry(id).or_default().push(event);
        Ok(())
    }

    /// Write a node. Naming `adopt` makes it the parent of those nodes, which is how a summary is written.
    pub fn write(&mut self, draft: Draft, actor: &str) -> Result<Node> {
        let content = clean_content(&draft.content)?;
        let tags = clean_list(&draft.tags, MAX_TAGS, "标签")?;
        let refs = clean_list(&draft.refs, MAX_LINKS, "引用")?;
        let sources = clean_list(&draft.sources, MAX_SOURCES, "来源")?;
        let adopt = clean_list(&draft.adopt, MAX_MOVE, "收纳的节点")?;
        self.check_links(&refs, None)?;
        if let Some(parent) = &draft.parent {
            self.must_live(parent)?;
            for a in &adopt {
                ensure!(
                    !self.within(parent, a),
                    "不能把节点收纳到它自己的子孙下面（{a}）"
                );
            }
        }
        for a in &adopt {
            self.must_live(a)?;
        }
        let vector = self.embed.encode(&content)?;
        let stamp = now();
        let node = Node {
            id: uuid::Uuid::new_v4().simple().to_string(),
            content,
            tags,
            parent: draft.parent,
            refs,
            sources,
            version: 1,
            occurred: draft.occurred.trim().to_owned(),
            created: stamp.clone(),
            updated: stamp,
            status: Status::Live,
        };
        self.put(node.clone(), Some(vector), "write", actor, "")?;
        for a in adopt {
            let mut moved = self.nodes[&a].clone();
            moved.parent = Some(node.id.clone());
            moved.version += 1;
            moved.updated = now();
            self.put(moved, None, "reparent", actor, "收进新汇总")?;
        }
        Ok(node)
    }

    /// Change a node. `expect` is the version the caller read: a different one means somebody else changed it.
    pub fn update(&mut self, id: &str, expect: u32, patch: Patch, actor: &str) -> Result<Node> {
        let mut node = self.must_live(id)?.clone();
        Self::check_version(&node, expect)?;
        let mut vector = None;
        if let Some(content) = &patch.content {
            node.content = clean_content(content)?;
            vector = Some(self.embed.encode(&node.content)?);
        }
        if let Some(tags) = &patch.tags {
            node.tags = clean_list(tags, MAX_TAGS, "标签")?;
        }
        if let Some(refs) = &patch.refs {
            node.refs = clean_list(refs, MAX_LINKS, "引用")?;
            self.check_links(&node.refs, Some(id))?;
        }
        if let Some(sources) = &patch.sources {
            node.sources = clean_list(sources, MAX_SOURCES, "来源")?;
        }
        if let Some(occurred) = &patch.occurred {
            node.occurred = occurred.trim().to_owned();
        }
        let touch = patch.content.is_none()
            && patch.tags.is_none()
            && patch.refs.is_none()
            && patch.sources.is_none()
            && patch.occurred.is_none();
        node.version += 1;
        node.updated = now();
        self.put(
            node.clone(),
            vector,
            if touch { "touch" } else { "update" },
            actor,
            "",
        )?;
        Ok(node)
    }

    /// Move nodes under `to` (or make them roots). A node cannot go under itself or one of its own descendants.
    pub fn reparent(&mut self, ids: &[String], to: Option<&str>, actor: &str) -> Result<()> {
        let ids = clean_list(ids, MAX_MOVE, "移动的节点")?;
        ensure!(!ids.is_empty(), "没有要移动的节点");
        if let Some(to) = to {
            self.must_live(to)?;
        }
        for id in &ids {
            self.must_live(id)?;
            if let Some(to) = to {
                ensure!(
                    !self.within(to, id),
                    "不能把节点移到它自己或它的子孙下面（{id}）"
                );
            }
        }
        for id in ids {
            let mut node = self.nodes[&id].clone();
            if node.parent.as_deref() == to {
                continue;
            }
            node.parent = to.map(str::to_owned);
            node.version += 1;
            node.updated = now();
            self.put(node, None, "reparent", actor, "")?;
        }
        Ok(())
    }

    /// Soft delete: the node goes to the recycle bin and its children move up to its parent. Returns the children moved.
    pub fn delete(
        &mut self,
        id: &str,
        expect: u32,
        reason: &str,
        actor: &str,
    ) -> Result<Vec<String>> {
        let node = self.must_live(id)?.clone();
        Self::check_version(&node, expect)?;
        ensure!(!reason.trim().is_empty(), "删除要写明原因");
        let moved: Vec<String> = self.children(id).to_vec();
        for kid in &moved {
            let mut k = self.nodes[kid].clone();
            k.parent = node.parent.clone();
            k.version += 1;
            k.updated = now();
            self.put(k, None, "reparent", actor, "父节点被删除，上移一层")?;
        }
        let mut gone = node;
        gone.status = Status::Deleted;
        gone.version += 1;
        gone.updated = now();
        self.put(gone, None, "delete", actor, reason.trim())?;
        self.kids.remove(id);
        Ok(moved)
    }

    /// Take a node back out of the recycle bin. It returns to its old parent when that is still there.
    pub fn restore(&mut self, id: &str, actor: &str) -> Result<Node> {
        let node = self.nodes.get(id).filter(|n| n.status == Status::Deleted);
        let mut node = match node {
            Some(n) => n.clone(),
            None => bail!("回收站里没有这个节点：{id}"),
        };
        if node
            .parent
            .as_deref()
            .is_some_and(|p| self.live(p).is_none())
        {
            node.parent = None;
        }
        let vector = self.embed.encode(&node.content)?;
        node.status = Status::Live;
        node.version += 1;
        node.updated = now();
        self.put(node.clone(), Some(vector), "restore", actor, "")?;
        Ok(node)
    }

    /// Fill an empty tree by replaying ready-made events (for importing an older library).
    pub fn import(&mut self, events: Vec<Event>) -> Result<()> {
        ensure!(self.nodes.is_empty(), "只能导入到空的树里");
        let last: HashMap<&str, &Node> = events
            .iter()
            .map(|e| (e.node.id.as_str(), &e.node))
            .collect();
        for (id, n) in &last {
            if let Some(p) = &n.parent {
                ensure!(
                    last.contains_key(p.as_str()),
                    "节点 {id} 的父节点 {p} 不在导入的数据里"
                );
            }
        }
        for event in events {
            writeln!(self.log, "{}", serde_json::to_string(&event)?)?;
            self.nodes.insert(event.node.id.clone(), event.node.clone());
            self.history
                .entry(event.node.id.clone())
                .or_default()
                .push(event);
        }
        self.log.flush()?;
        self.rebuild()
    }
}

#[cfg(test)]
mod tests;
