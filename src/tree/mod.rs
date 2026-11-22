//! The memory tree: every memory is a node, a node has at most one parent (the summary it sits under), and the
//! AI may write, update, move and delete nodes freely. The file `nodes.jsonl` is an append-only log of every
//! change; the tree in memory is just its latest state, so any mistake can be traced and undone.
mod lexicon;
mod maintain;
mod model;
mod search;

pub use maintain::Maintenance;
pub use model::{Attachment, Draft, Event, MAX_CONTENT, Node, Patch, Status, Storage, now};
pub use search::{Hit, SearchOpts, no_strong_match};

use crate::{embedding::Embed, vectors};
use anyhow::{Context, Result, bail, ensure};
use lexicon::Lexicon;
use model::{MAX_ATTACHMENTS, MAX_LINKS, MAX_SOURCES, MAX_TAGS};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
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
    vlog: vectors::Log,
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

fn clean_attachments(list: &[Attachment]) -> Result<Vec<Attachment>> {
    let mut out: Vec<Attachment> = vec![];
    for a in list {
        if !out.iter().any(|x| x.hash == a.hash) {
            out.push(a.clone());
        }
    }
    ensure!(
        out.len() <= MAX_ATTACHMENTS,
        "附件最多 {MAX_ATTACHMENTS} 个"
    );
    Ok(out)
}

/// What a merge did to the library.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct MergeReport {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
}

fn read_log(path: &Path) -> Result<Vec<Event>> {
    let mut events = vec![];
    if !path.exists() {
        return Ok(events);
    }
    let lines: Vec<String> = BufReader::new(File::open(path)?)
        .lines()
        .collect::<std::io::Result<_>>()?;
    let last = lines.iter().rposition(|l| !l.trim().is_empty());
    let mut torn_at = None;
    for (n, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Event>(line) {
            Ok(event) => events.push(event),
            // a write cut short by a crash can only be the last line
            Err(_) if Some(n) == last => torn_at = Some(n),
            Err(error) => bail!("nodes.jsonl 第 {} 行损坏：{error}", n + 1),
        }
    }
    if let Some(n) = torn_at {
        let kept: String = lines[..n].iter().map(|l| format!("{l}\n")).collect();
        fs::write(path, kept)?;
        crate::log!("nodes.jsonl 的最后一行没写完，已丢弃");
    }
    Ok(events)
}

impl Tree {
    pub fn open(dir: &Path, embed: Arc<dyn Embed>) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join("nodes.jsonl");
        let mut nodes = HashMap::new();
        let mut history: HashMap<String, Vec<Event>> = HashMap::new();
        for event in read_log(&path)? {
            nodes.insert(event.node.id.clone(), event.node.clone());
            history
                .entry(event.node.id.clone())
                .or_default()
                .push(event);
        }
        let log = OpenOptions::new().create(true).append(true).open(&path)?;
        let (vlog, known) = vectors::Log::open(&dir.join("vectors.bin"))?;
        let mut tree = Self {
            dir: dir.to_owned(),
            embed,
            nodes,
            kids: HashMap::new(),
            vectors: HashMap::new(),
            lex: Lexicon::default(),
            history,
            log,
            vlog,
        };
        tree.rebuild(&known)?;
        Ok(tree)
    }

    /// The kids, the keyword index and the vectors, from the nodes. Vectors that were saved for the same text are
    /// reused; the rest are computed (and saved).
    fn rebuild(&mut self, known: &HashMap<String, (u64, Vec<f32>)>) -> Result<()> {
        self.kids.clear();
        self.vectors.clear();
        self.lex = Lexicon::default();
        let mut missing: Vec<String> = vec![];
        let mut live: Vec<&Node> = self
            .nodes
            .values()
            .filter(|n| n.status == Status::Live)
            .collect();
        live.sort_by(|a, b| a.id.cmp(&b.id));
        for n in &live {
            self.lex.add(&n.id, &index_text(n));
            match known.get(&n.id) {
                Some((key, v)) if *key == vectors::key(&n.content) => {
                    self.vectors.insert(n.id.clone(), v.clone());
                }
                _ => missing.push(n.id.clone()),
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
                self.vlog
                    .append(id, vectors::key(&self.nodes[id].content), &v)?;
                self.vectors.insert(id.clone(), v);
            }
        }
        let nodes = &self.nodes;
        for list in self.kids.values_mut() {
            list.sort_by(|a, b| (&nodes[a].occurred, a).cmp(&(&nodes[b].occurred, b)));
        }
        // the file only needs the latest vector of each live node
        if known.len() > self.vectors.len() * 2 + 64 {
            self.compact_vectors()?;
        }
        Ok(())
    }

    fn compact_vectors(&mut self) -> Result<()> {
        let keep: Vec<(String, u64, Vec<f32>)> = self
            .vectors
            .iter()
            .map(|(id, v)| (id.clone(), vectors::key(&self.nodes[id].content), v.clone()))
            .collect();
        self.vlog.compact(keep.into_iter())
    }

    /// Replace the whole log with these events and rebuild the tree from them. Vectors already computed for
    /// the same text are kept; `extra` offers vectors from elsewhere (a backup).
    fn restart(
        &mut self,
        events: &[Event],
        extra: &HashMap<String, (u64, Vec<f32>)>,
    ) -> Result<()> {
        let tmp = self.dir.join("nodes.jsonl.tmp");
        let mut text = String::new();
        for e in events {
            text.push_str(&serde_json::to_string(e)?);
            text.push('\n');
        }
        fs::write(&tmp, text)?;
        fs::rename(&tmp, self.dir.join("nodes.jsonl"))?;
        self.log = OpenOptions::new()
            .append(true)
            .open(self.dir.join("nodes.jsonl"))?;
        let mut known: HashMap<String, (u64, Vec<f32>)> = extra.clone();
        for (id, v) in &self.vectors {
            if let Some(n) = self.nodes.get(id) {
                known.insert(id.clone(), (vectors::key(&n.content), v.clone()));
            }
        }
        self.nodes.clear();
        self.history.clear();
        for e in events {
            self.nodes.insert(e.node.id.clone(), e.node.clone());
            self.history
                .entry(e.node.id.clone())
                .or_default()
                .push(e.clone());
        }
        self.rebuild(&known)
    }

    /// The vector file, for putting in a backup.
    pub fn vector_bytes(&self) -> Result<Vec<u8>> {
        self.vlog.bytes()
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

    /// Every live node.
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values().filter(|n| n.status == Status::Live)
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

    /// The layer of every live node, in one pass.
    pub fn layers(&self) -> HashMap<String, u32> {
        fn walk(t: &Tree, id: &str, memo: &mut HashMap<String, u32>) -> u32 {
            if let Some(l) = memo.get(id) {
                return *l;
            }
            let l = t
                .children(id)
                .iter()
                .map(|c| walk(t, c, memo) + 1)
                .max()
                .unwrap_or(0);
            memo.insert(id.to_owned(), l);
            l
        }
        let mut memo = HashMap::new();
        for n in self.nodes() {
            walk(self, &n.id, &mut memo);
        }
        memo
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

    /// Live nodes at or under `id`, itself included.
    pub fn subtree(&self, id: &str) -> usize {
        1 + self
            .children(id)
            .iter()
            .map(|c| self.subtree(c))
            .sum::<usize>()
    }

    /// Every revision of a node, oldest first.
    pub fn history(&self, id: &str) -> Vec<&Event> {
        self.history
            .get(id)
            .map(|h| h.iter().collect())
            .unwrap_or_default()
    }

    /// The most recent changes anywhere, newest first.
    pub fn activity(&self, limit: usize) -> Vec<&Event> {
        let mut all: Vec<&Event> = self.history.values().flatten().collect();
        all.sort_by(|a, b| b.at.cmp(&a.at));
        all.truncate(limit);
        all
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

    /// Tags and how many live nodes carry each, most used first.
    pub fn tags(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for n in self.nodes() {
            for t in &n.tags {
                *counts.entry(t).or_default() += 1;
            }
        }
        let mut out: Vec<(String, usize)> =
            counts.into_iter().map(|(t, n)| (t.to_owned(), n)).collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }

    /// Every stored attachment any node (deleted ones included) still mentions.
    pub fn used_files(&self) -> HashSet<String> {
        self.nodes
            .values()
            .flat_map(|n| n.attachments.iter())
            .filter(|a| a.storage == Storage::Stored)
            .map(|a| a.hash.clone())
            .collect()
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

    /// The latest change anywhere, for "nothing changed since the last backup".
    pub fn last_change(&self) -> &str {
        self.history
            .values()
            .filter_map(|h| h.last())
            .map(|e| e.at.as_str())
            .max()
            .unwrap_or("")
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
                self.vlog.append(&id, vectors::key(&node.content), &v)?;
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
        let attachments = clean_attachments(&draft.attachments)?;
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
            attachments,
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
        if let Some(attachments) = &patch.attachments {
            node.attachments = clean_attachments(attachments)?;
        }
        if let Some(occurred) = &patch.occurred {
            node.occurred = occurred.trim().to_owned();
        }
        let touch = patch.is_empty();
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
        let mut node = match self.nodes.get(id).filter(|n| n.status == Status::Deleted) {
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

    /// Remove deleted nodes for good, history included. Returns how many went.
    pub fn purge(&mut self, ids: &[String]) -> Result<usize> {
        let gone: HashSet<&str> = ids
            .iter()
            .filter(|id| {
                self.nodes
                    .get(*id)
                    .is_some_and(|n| n.status == Status::Deleted)
            })
            .map(String::as_str)
            .collect();
        if gone.is_empty() {
            return Ok(0);
        }
        let events: Vec<Event> = self
            .all_events()
            .into_iter()
            .filter(|e| !gone.contains(e.node.id.as_str()))
            .collect();
        let count = gone.len();
        self.restart(&events, &HashMap::new())?;
        Ok(count)
    }

    /// Purge what has been in the recycle bin longer than `days`.
    pub fn purge_old(&mut self, days: i64) -> Result<usize> {
        let limit = (chrono::Utc::now() - chrono::Duration::days(days))
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        let old: Vec<String> = self
            .trash()
            .into_iter()
            .filter(|n| n.updated < limit)
            .map(|n| n.id.clone())
            .collect();
        self.purge(&old)
    }

    // ---------- whole-library operations ----------

    /// Every event, grouped by node and in time order within a node.
    pub fn all_events(&self) -> Vec<Event> {
        let mut ids: Vec<&String> = self.history.keys().collect();
        ids.sort();
        ids.into_iter()
            .flat_map(|id| self.history[id].iter().cloned())
            .collect()
    }

    /// Make the library exactly this history (a restored backup), keeping vectors that still fit.
    pub fn replace(
        &mut self,
        events: Vec<Event>,
        vectors: &HashMap<String, (u64, Vec<f32>)>,
    ) -> Result<()> {
        let events = settle(events)?;
        self.restart(&events, vectors)
    }

    /// Add what another library knows: events this one does not have are added, and a node both changed ends
    /// up as the version that was changed last (both histories stay).
    pub fn merge(
        &mut self,
        incoming: Vec<Event>,
        vectors: &HashMap<String, (u64, Vec<f32>)>,
    ) -> Result<MergeReport> {
        let mut report = MergeReport::default();
        let have: HashSet<(String, u32, String)> = self
            .history
            .values()
            .flatten()
            .map(|e| (e.node.id.clone(), e.node.version, e.at.clone()))
            .collect();
        let mut touched: HashSet<String> = HashSet::new();
        let mut all = self.all_events();
        for e in incoming {
            if have.contains(&(e.node.id.clone(), e.node.version, e.at.clone())) {
                continue;
            }
            touched.insert(e.node.id.clone());
            all.push(e);
        }
        for id in &touched {
            if self.nodes.contains_key(id) {
                report.updated += 1;
            } else {
                report.added += 1;
            }
        }
        report.unchanged = self.nodes.len().saturating_sub(report.updated);
        if touched.is_empty() {
            return Ok(report);
        }
        let events = settle(all)?;
        self.restart(&events, vectors)?;
        Ok(report)
    }
}

/// Put events in the order they happened per node, and make the result consistent: every parent exists and is
/// live, and no node is its own ancestor (two libraries may have moved nodes in conflicting ways).
fn settle(mut events: Vec<Event>) -> Result<Vec<Event>> {
    for e in &events {
        ensure!(
            e.node.id.len() == 32 && e.node.id.bytes().all(|b| b.is_ascii_hexdigit()),
            "备份里有无效的节点编号：{}",
            e.node.id
        );
        ensure!(
            !e.node.content.trim().is_empty() && e.node.content.chars().count() <= MAX_CONTENT,
            "备份里有内容无效的节点：{}",
            e.node.id
        );
    }
    events.sort_by(|a, b| {
        a.node
            .id
            .cmp(&b.node.id)
            .then_with(|| a.at.cmp(&b.at))
            .then_with(|| a.node.version.cmp(&b.node.version))
    });
    // the final state of each node
    let mut last: HashMap<String, usize> = HashMap::new();
    for (i, e) in events.iter().enumerate() {
        last.insert(e.node.id.clone(), i);
    }
    let live: HashSet<&str> = last
        .iter()
        .filter(|(_, i)| events[**i].node.status == Status::Live)
        .map(|(id, _)| id.as_str())
        .collect();
    let mut parent: HashMap<String, Option<String>> = last
        .iter()
        .map(|(id, i)| {
            let p = events[*i]
                .node
                .parent
                .clone()
                .filter(|p| live.contains(p.as_str()));
            (id.clone(), p)
        })
        .collect();
    let ids: Vec<String> = parent.keys().cloned().collect();
    for start in ids {
        let mut at = start.clone();
        for _ in 0..=parent.len() {
            match parent.get(&at).cloned().flatten() {
                Some(up) if up == start => {
                    parent.insert(start.clone(), None);
                    break;
                }
                Some(up) => at = up,
                None => break,
            }
        }
    }
    for (id, i) in &last {
        let fixed = parent[id].clone();
        if events[*i].node.parent != fixed {
            events[*i].node.parent = fixed;
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests;
