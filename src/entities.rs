//! Entities and the derived index. Entities are contributed by the connected AI; the server never
//! invents one, never merges by name or vector similarity, and every mention keeps its memory as source.
use crate::{
    kv,
    memory::*,
    store::{Store, now},
};
use anyhow::{Context, Result, anyhow, ensure};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use unicode_normalization::UnicodeNormalization;

/// Normalized key used to compare names: NFKC, case-folded, single spaces.
pub fn norm(s: &str) -> String {
    s.nfkc()
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Derived lookups. Rebuilt on start and after a restore, updated on every write. Never a source of truth.
#[derive(Default)]
pub struct Index {
    pub entities: HashMap<String, Entity>,
    names: HashMap<String, BTreeSet<String>>,
    mentions: HashMap<String, BTreeSet<String>>,
    of: HashMap<String, Vec<String>>,
    /// target memory -> (citing memory, reference)
    pub cites: HashMap<String, Vec<(String, Reference)>>,
    refs_of: HashMap<String, Vec<String>>,
    pub files: HashMap<String, Attachment>,
    /// Ids of memories that exist and are not deleted.
    pub live: HashSet<String>,
}

impl Index {
    pub fn canonical(&self, id: &str) -> Option<&Entity> {
        let mut cur = self.entities.get(id)?;
        for _ in 0..16 {
            match &cur.merged_into {
                Some(next) => cur = self.entities.get(next)?,
                None => return Some(cur),
            }
        }
        None
    }

    fn keys(e: &Entity) -> Vec<String> {
        let mut keys = vec![norm(&e.name)];
        keys.extend(e.aliases.iter().map(|a| norm(a)));
        keys.retain(|k| !k.is_empty());
        keys
    }

    fn remove_names(&mut self, id: &str) {
        for set in self.names.values_mut() {
            set.remove(id);
        }
        self.names.retain(|_, set| !set.is_empty());
    }

    pub fn put_entity(&mut self, e: Entity) {
        self.remove_names(&e.id);
        if e.merged_into.is_none() {
            for key in Self::keys(&e) {
                self.names.entry(key).or_default().insert(e.id.clone());
            }
        }
        self.entities.insert(e.id.clone(), e);
    }

    pub fn drop_entity(&mut self, id: &str) {
        self.remove_names(id);
        self.entities.remove(id);
    }

    pub fn drop_memory(&mut self, id: &str) {
        for entity in self.of.remove(id).unwrap_or_default() {
            if let Some(set) = self.mentions.get_mut(&entity) {
                set.remove(id);
            }
        }
        for target in self.refs_of.remove(id).unwrap_or_default() {
            if let Some(list) = self.cites.get_mut(&target) {
                list.retain(|(source, _)| source != id);
            }
        }
    }

    pub fn forget_memory(&mut self, id: &str) {
        self.drop_memory(id);
        self.live.remove(id);
    }

    pub fn put_memory(&mut self, e: &Entry) {
        self.drop_memory(&e.id);
        self.live.insert(e.id.clone());
        let ids: Vec<String> = e.mentions.iter().filter_map(|m| m.entity.clone()).collect();
        for entity in &ids {
            self.mentions
                .entry(entity.clone())
                .or_default()
                .insert(e.id.clone());
        }
        self.of.insert(e.id.clone(), ids);
        for r in &e.references {
            self.cites
                .entry(r.id.clone())
                .or_default()
                .push((e.id.clone(), r.clone()));
        }
        self.refs_of.insert(
            e.id.clone(),
            e.references.iter().map(|r| r.id.clone()).collect(),
        );
    }

    /// Every entity id that resolves to `canon`, including itself.
    pub fn members(&self, canon: &str) -> Vec<String> {
        self.entities
            .keys()
            .filter(|id| self.canonical(id).is_some_and(|c| c.id == canon))
            .cloned()
            .collect()
    }

    pub fn memory_ids(&self, canon: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for member in self.members(canon) {
            if let Some(set) = self.mentions.get(&member) {
                out.extend(set.iter().cloned());
            }
        }
        out
    }

    pub fn find(&self, name: &str) -> Vec<&Entity> {
        self.names
            .get(&norm(name))
            .map(|ids| ids.iter().filter_map(|id| self.entities.get(id)).collect())
            .unwrap_or_default()
    }

    pub fn has_mentions(&self, id: &str) -> bool {
        self.mentions.get(id).is_some_and(|s| !s.is_empty())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct EntityView {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hint: String,
    pub mentions: usize,
    /// Most recent occurrence (or record) date among its memories.
    pub last: String,
    pub source: String,
    pub created: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct EntityChoice {
    pub name: String,
    pub candidates: Vec<EntityView>,
}

#[derive(Serialize)]
pub struct EntityDetail {
    pub entity: EntityView,
    /// Names of entities that were merged into this one.
    pub merged: Vec<String>,
    pub memories: Vec<Entry>,
    pub related: Vec<(EntityView, usize)>,
}

#[derive(Serialize)]
pub struct Binding {
    pub entity: Option<EntityView>,
    pub choices: Vec<EntityChoice>,
}

fn sort_key(e: &Entry) -> (String, String) {
    (
        if e.occurred.is_empty() {
            String::new()
        } else {
            e.occurred.clone()
        },
        e.recorded.clone(),
    )
}

impl Store {
    pub(crate) async fn load_index(&self) -> Result<()> {
        let mut index = Index::default();
        for payload in kv::payloads(&self.etable).await? {
            index.put_entity(serde_json::from_str(&payload)?);
        }
        for payload in kv::payloads(&self.atable).await? {
            let a: Attachment = serde_json::from_str(&payload)?;
            index.files.insert(a.hash.clone(), a);
        }
        for record in self.records("NOT deleted").await? {
            index.put_memory(&record.entry);
        }
        *self.idx.write().await = index;
        Ok(())
    }

    async fn put_entity_locked(&self, e: Entity) -> Result<()> {
        kv::put(&self.etable, &[(e.id.clone(), serde_json::to_string(&e)?)]).await?;
        self.idx.write().await.put_entity(e);
        Ok(())
    }

    fn view(idx: &Index, e: &Entity, last: &HashMap<String, String>) -> EntityView {
        let ids = idx.memory_ids(&e.id);
        let newest = ids
            .iter()
            .filter_map(|id| last.get(id))
            .max()
            .cloned()
            .unwrap_or_default();
        EntityView {
            id: e.id.clone(),
            name: e.name.clone(),
            aliases: e.aliases.clone(),
            hint: e.hint.clone(),
            mentions: ids.len(),
            last: newest,
            source: e.source.clone(),
            created: e.created.clone(),
        }
    }

    async fn last_map(&self) -> Result<HashMap<String, String>> {
        Ok(self
            .all()
            .await?
            .iter()
            .map(|e| {
                (
                    e.id.clone(),
                    if e.occurred.is_empty() {
                        e.recorded.get(..10).unwrap_or_default().to_owned()
                    } else {
                        e.occurred.clone()
                    },
                )
            })
            .collect())
    }

    /// Resolve what the AI submitted, with the write lock held. Plain names that collide with an
    /// existing entity are left unresolved on purpose: the AI chooses, the server never guesses.
    pub(crate) async fn resolve_entities(
        &self,
        inputs: &[EntityInput],
        source: &str,
    ) -> Result<(Vec<Mention>, Vec<EntityChoice>)> {
        let mut mentions: Vec<Mention> = Vec::new();
        let mut choices = Vec::new();
        let mut seen = HashSet::new();
        let last = self.last_map().await?;
        for input in inputs {
            let key = if input.id.is_empty() {
                format!("n:{}:{}", norm(&input.name), input.new)
            } else {
                format!("i:{}", input.id)
            };
            if !seen.insert(key) {
                continue;
            }
            if !input.id.is_empty() {
                let canon = {
                    let idx = self.idx.read().await;
                    idx.canonical(&input.id)
                        .cloned()
                        .ok_or_else(|| anyhow!("实体不存在：{}", input.id))?
                };
                let patched = Self::enrich(&canon, input);
                if patched != canon {
                    self.put_entity_locked(patched).await?;
                }
                mentions.push(Mention {
                    name: input.name.clone(),
                    entity: Some(canon.id),
                });
                continue;
            }
            let mut candidates: Vec<Entity> = {
                let idx = self.idx.read().await;
                let mut found: Vec<Entity> = idx.find(&input.name).into_iter().cloned().collect();
                for alias in &input.aliases {
                    for e in idx.find(alias) {
                        if !found.iter().any(|f| f.id == e.id) {
                            found.push(e.clone());
                        }
                    }
                }
                found
            };
            candidates.sort_by(|a, b| a.id.cmp(&b.id));
            if input.new || candidates.is_empty() {
                let stamp = now();
                let entity = Entity {
                    id: uuid::Uuid::new_v4().simple().to_string(),
                    name: input.name.clone(),
                    aliases: Self::fresh_aliases(&input.name, &input.aliases),
                    hint: input.hint.clone(),
                    source: source.into(),
                    created: stamp.clone(),
                    updated: stamp,
                    merged_into: None,
                };
                self.put_entity_locked(entity.clone()).await?;
                mentions.push(Mention {
                    name: input.name.clone(),
                    entity: Some(entity.id),
                });
            } else {
                let idx = self.idx.read().await;
                choices.push(EntityChoice {
                    name: input.name.clone(),
                    candidates: candidates
                        .iter()
                        .map(|e| Self::view(&idx, e, &last))
                        .collect(),
                });
                mentions.push(Mention {
                    name: input.name.clone(),
                    entity: None,
                });
            }
        }
        Ok((mentions, choices))
    }

    fn fresh_aliases(name: &str, aliases: &[String]) -> Vec<String> {
        let mut seen = HashSet::from([norm(name)]);
        aliases
            .iter()
            .filter(|a| seen.insert(norm(a)))
            .cloned()
            .collect()
    }

    /// Add aliases and a hint the AI supplied for an entity it explicitly chose.
    fn enrich(entity: &Entity, input: &EntityInput) -> Entity {
        let mut next = entity.clone();
        let mut seen: HashSet<String> = std::iter::once(norm(&next.name))
            .chain(next.aliases.iter().map(|a| norm(a)))
            .collect();
        for alias in &input.aliases {
            if seen.insert(norm(alias)) {
                next.aliases.push(alias.clone());
            }
        }
        if next.hint.is_empty() && !input.hint.is_empty() {
            next.hint = input.hint.clone();
        }
        if next != *entity {
            next.updated = now();
        }
        next
    }

    pub async fn entity_label(&self, id: &str) -> Option<String> {
        self.idx.read().await.canonical(id).map(|e| e.name.clone())
    }

    pub async fn entity_search(&self, query: &str, limit: usize) -> Result<Vec<EntityView>> {
        let last = self.last_map().await?;
        let idx = self.idx.read().await;
        let needle = norm(query);
        let mut scored: Vec<(u8, EntityView)> = Vec::new();
        for e in idx.entities.values().filter(|e| e.merged_into.is_none()) {
            let keys = Index::keys(e);
            let rank = if needle.is_empty() {
                Some(3)
            } else if keys.contains(&needle) {
                Some(0)
            } else if keys.iter().any(|k| k.starts_with(&needle)) {
                Some(1)
            } else if keys.iter().any(|k| k.contains(&needle)) || norm(&e.hint).contains(&needle) {
                Some(2)
            } else {
                None
            };
            if let Some(rank) = rank {
                scored.push((rank, Self::view(&idx, e, &last)));
            }
        }
        scored.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(b.1.mentions.cmp(&a.1.mentions))
                .then(b.1.last.cmp(&a.1.last))
                .then(a.1.name.cmp(&b.1.name))
        });
        Ok(scored.into_iter().take(limit).map(|s| s.1).collect())
    }

    /// Directory page for the web UI: (entities, total).
    pub async fn entity_page(
        &self,
        query: &str,
        sort: &str,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<EntityView>, usize)> {
        let mut all = self.entity_search(query, usize::MAX).await?;
        match sort {
            "name" => all.sort_by(|a, b| a.name.cmp(&b.name)),
            "recent" => all.sort_by(|a, b| b.last.cmp(&a.last).then(a.name.cmp(&b.name))),
            _ => {}
        }
        let total = all.len();
        Ok((all.into_iter().skip(offset).take(limit).collect(), total))
    }

    pub async fn entity_detail(&self, id: &str, limit: usize) -> Result<EntityDetail> {
        ensure!(valid_id(id), "实体 ID 无效");
        let last = self.last_map().await?;
        let (entity, merged, ids) = {
            let idx = self.idx.read().await;
            let canon = idx.canonical(id).cloned().context("未找到实体")?;
            let merged = idx
                .members(&canon.id)
                .into_iter()
                .filter(|m| *m != canon.id)
                .filter_map(|m| idx.entities.get(&m).map(|e| e.name.clone()))
                .collect();
            let view = Self::view(&idx, &canon, &last);
            (view, merged, idx.memory_ids(&canon.id))
        };
        let entries = self.all().await?;
        let mut memories: Vec<Entry> = entries
            .iter()
            .filter(|e| ids.contains(&e.id))
            .cloned()
            .collect();
        memories.sort_by_key(|e| std::cmp::Reverse(sort_key(e)));
        memories.truncate(limit);
        let mut related: HashMap<String, usize> = HashMap::new();
        {
            let idx = self.idx.read().await;
            for entry in entries.iter().filter(|e| ids.contains(&e.id)) {
                let mut once = HashSet::new();
                for m in &entry.mentions {
                    if let Some(c) = m.entity.as_deref().and_then(|x| idx.canonical(x))
                        && c.id != entity.id
                        && once.insert(c.id.clone())
                    {
                        *related.entry(c.id.clone()).or_default() += 1;
                    }
                }
            }
        }
        let idx = self.idx.read().await;
        let mut related: Vec<(EntityView, usize)> = related
            .into_iter()
            .filter_map(|(rid, n)| {
                idx.entities
                    .get(&rid)
                    .map(|e| (Self::view(&idx, e, &last), n))
            })
            .collect();
        related.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.name.cmp(&b.0.name)));
        related.truncate(24);
        Ok(EntityDetail {
            entity,
            merged,
            memories,
            related,
        })
    }

    pub async fn entity_update(
        &self,
        id: &str,
        name: Option<String>,
        add_aliases: Vec<String>,
        remove_aliases: Vec<String>,
        hint: Option<String>,
    ) -> Result<EntityView> {
        let _guard = self.writes.lock().await;
        let mut entity = {
            let idx = self.idx.read().await;
            idx.canonical(id).cloned().context("未找到实体")?
        };
        if let Some(name) = name {
            let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
            ensure!(
                !name.is_empty() && name.chars().count() <= 200,
                "实体名称为空或过长"
            );
            if norm(&name) != norm(&entity.name) {
                // The former name stays findable as an alias.
                let former = std::mem::replace(&mut entity.name, name);
                entity.aliases.push(former);
            } else {
                entity.name = name;
            }
        }
        let drop: HashSet<String> = remove_aliases.iter().map(|a| norm(a)).collect();
        entity.aliases.retain(|a| !drop.contains(&norm(a)));
        let mut seen: HashSet<String> = std::iter::once(norm(&entity.name))
            .chain(entity.aliases.iter().map(|a| norm(a)))
            .collect();
        for alias in add_aliases {
            let alias = alias.split_whitespace().collect::<Vec<_>>().join(" ");
            ensure!(alias.chars().count() <= 200, "实体别名过长");
            if !alias.is_empty() && seen.insert(norm(&alias)) {
                entity.aliases.push(alias);
            }
        }
        ensure!(entity.aliases.len() <= 32, "实体别名过多");
        let drop_name = drop.contains(&norm(&entity.name));
        ensure!(!drop_name, "不能移除实体的主名称");
        if let Some(hint) = hint {
            ensure!(hint.chars().count() <= 400, "实体说明过长");
            entity.hint = hint.trim().to_owned();
        }
        entity.updated = now();
        self.put_entity_locked(entity.clone()).await?;
        let last = self.last_map().await?;
        let idx = self.idx.read().await;
        Ok(Self::view(&idx, &entity, &last))
    }

    /// Redirect `from` to `into`. Mentions keep their original ids and follow the redirect when read,
    /// so a wrong merge can be undone with `entity_unmerge`.
    pub async fn entity_merge(&self, from: &str, into: &str) -> Result<EntityView> {
        ensure!(from != into, "不能把实体合并到它自己");
        let _guard = self.writes.lock().await;
        let (source, target) = {
            let idx = self.idx.read().await;
            let source = idx.canonical(from).cloned().context("未找到要合并的实体")?;
            let target = idx.canonical(into).cloned().context("未找到目标实体")?;
            (source, target)
        };
        ensure!(source.id != target.id, "两者已经是同一个实体");
        let mut target = target;
        let mut seen: HashSet<String> = std::iter::once(norm(&target.name))
            .chain(target.aliases.iter().map(|a| norm(a)))
            .collect();
        for name in std::iter::once(source.name.clone()).chain(source.aliases.clone()) {
            if seen.insert(norm(&name)) {
                target.aliases.push(name);
            }
        }
        target.updated = now();
        let mut source = source;
        source.merged_into = Some(target.id.clone());
        source.updated = now();
        self.put_entity_locked(source).await?;
        self.put_entity_locked(target.clone()).await?;
        let last = self.last_map().await?;
        let idx = self.idx.read().await;
        Ok(Self::view(&idx, &target, &last))
    }

    pub async fn entity_unmerge(&self, id: &str) -> Result<()> {
        let _guard = self.writes.lock().await;
        let mut entity = {
            let idx = self.idx.read().await;
            idx.entities.get(id).cloned().context("未找到实体")?
        };
        ensure!(entity.merged_into.is_some(), "该实体没有被合并");
        entity.merged_into = None;
        entity.updated = now();
        self.put_entity_locked(entity).await
    }

    pub async fn entity_delete(&self, id: &str) -> Result<()> {
        let _guard = self.writes.lock().await;
        {
            let idx = self.idx.read().await;
            ensure!(idx.entities.contains_key(id), "未找到实体");
            ensure!(
                idx.members(id).iter().all(|m| !idx.has_mentions(m)),
                "仍有记忆提及这个实体，请先合并或移除提及"
            );
            ensure!(
                !idx.entities
                    .values()
                    .any(|e| e.merged_into.as_deref() == Some(id)),
                "有实体合并到了这个实体，请先撤销合并"
            );
        }
        kv::delete(&self.etable, id).await?;
        self.idx.write().await.drop_entity(id);
        Ok(())
    }

    /// Names AI submitted that still point to no entity, grouped: (name, memory ids).
    pub async fn unresolved(&self) -> Result<Vec<(String, Vec<String>)>> {
        let mut groups: HashMap<String, (String, Vec<String>)> = HashMap::new();
        for e in self.all().await?.iter() {
            for m in e.mentions.iter().filter(|m| m.entity.is_none()) {
                let slot = groups
                    .entry(norm(&m.name))
                    .or_insert_with(|| (m.name.clone(), vec![]));
                slot.1.push(e.id.clone());
            }
        }
        let mut out: Vec<_> = groups.into_values().collect();
        out.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
        Ok(out)
    }

    /// Attach an entity to an existing memory (or settle an unresolved mention of the same name).
    pub async fn bind_entity(
        &self,
        memory_id: &str,
        input: EntityInput,
        source: &str,
    ) -> Result<Binding> {
        let mut input = input;
        input.clean()?;
        let _guard = self.writes.lock().await;
        let mut record = self.find(memory_id).await?;
        let (found, choices) = self
            .resolve_entities(std::slice::from_ref(&input), source)
            .await?;
        let mention = found.into_iter().next().context("实体解析失败")?;
        let key = norm(&mention.name);
        let entity = mention.entity.clone();
        {
            let idx = self.idx.read().await;
            let canon = |id: &Option<String>| {
                id.as_deref()
                    .and_then(|x| idx.canonical(x))
                    .map(|c| c.id.clone())
            };
            record.entry.mentions.retain(|m| {
                let same_entity = entity.is_some() && canon(&m.entity) == entity;
                let settles = m.entity.is_none() && norm(&m.name) == key;
                !(same_entity || settles)
            });
        }
        ensure!(
            record.entry.mentions.len() < MAX_ENTITIES,
            "一条记忆最多关联 32 个实体"
        );
        record.entry.mentions.push(mention);
        record.entry.updated = now();
        self.rewrite_locked(&record).await?;
        let view = match &entity {
            Some(id) => {
                let last = self.last_map().await?;
                let idx = self.idx.read().await;
                idx.canonical(id).map(|e| Self::view(&idx, e, &last))
            }
            None => None,
        };
        Ok(Binding {
            entity: view,
            choices,
        })
    }

    /// Remove a mention by entity id or by the name as written.
    pub async fn unbind_entity(&self, memory_id: &str, entity_or_name: &str) -> Result<()> {
        let _guard = self.writes.lock().await;
        let mut record = self.find(memory_id).await?;
        let key = norm(entity_or_name);
        let before = record.entry.mentions.len();
        {
            let idx = self.idx.read().await;
            let target = idx.canonical(entity_or_name).map(|c| c.id.clone());
            record.entry.mentions.retain(|m| {
                let canon = m
                    .entity
                    .as_deref()
                    .and_then(|x| idx.canonical(x))
                    .map(|c| c.id.clone());
                let by_entity = target.is_some() && canon == target;
                let by_name = m.entity.is_none() && norm(&m.name) == key;
                !(by_entity || by_name)
            });
        }
        ensure!(
            record.entry.mentions.len() < before,
            "这条记忆没有提及该实体"
        );
        record.entry.updated = now();
        self.rewrite_locked(&record).await
    }
}
