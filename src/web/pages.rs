//! Overview, tree, memories and search, one node (read and edit), the recycle bin, attachment files and the
//! read-only JSON the pages' scripts use.
use super::{Web, failure, human};
use crate::{
    service::{
        AttachInput, AttachmentView, Brief, ChangeView, LayerStat, ListQuery, MaintenanceView,
        NodeView, Order, Revision, Service, Stats, UpdateInput, WriteInput,
    },
    tree::SearchOpts,
};
use anyhow::Result;
use axum::{
    Form, Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::collections::HashMap;
use tower::ServiceExt;
use tower_http::services::ServeFile;

/// Who changes are attributed to when the person makes them in these pages.
const ACTOR: &str = "web";
const PAGE_SIZE: usize = 30;
const SEARCH_HITS: usize = 20;
/// A single uploaded file, and the whole form with all its files.
const MAX_FILE: usize = 64 << 20;
const MAX_FORM: usize = 256 << 20;
/// The page policy of the shell plus permission to show a PDF in a frame (only on nodes that have one).
const PDF_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; media-src 'self'; frame-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

pub(super) fn routes() -> Router<Web> {
    let upload = DefaultBodyLimit::max(MAX_FORM);
    Router::new()
        .route("/", get(overview))
        .route("/tree", get(tree))
        .route("/memories", get(memories))
        .route("/node/new", get(new_node).post(create_node).layer(upload))
        .route("/node/{id}", get(node_page))
        .route(
            "/node/{id}/edit",
            get(edit_node).post(save_node).layer(upload),
        )
        .route("/node/{id}/touch", post(touch_node))
        .route("/node/{id}/move", post(move_node))
        .route("/node/{id}/delete", post(delete_node))
        .route("/trash", get(trash))
        .route("/trash/empty", post(empty_trash))
        .route("/trash/{id}/restore", post(restore_node))
        .route("/trash/{id}/purge", post(purge_node))
        .route("/file/{hash}", get(file))
        .route("/api/outline", get(api_outline))
        .route("/api/node/{id}", get(api_node))
        .route("/api/search", get(api_search))
}

// ---------- small helpers ----------

/// The word for what an event did.
fn op_label(op: &str) -> &'static str {
    match op {
        "write" => "新增",
        "update" => "修改",
        "touch" => "确认",
        "reparent" => "移动",
        "delete" => "删除",
        "restore" => "恢复",
        "import" => "导入",
        _ => "改动",
    }
}

/// Who did it, as a person reads it.
fn actor_label(actor: &str) -> String {
    match actor {
        ACTOR => "你".to_owned(),
        "" => "未知".to_owned(),
        other => other.to_owned(),
    }
}

/// Tags typed in a plain text field. Commas (also the Chinese ones) separate them; with no comma at all,
/// spaces do. A single tag that contains a space is written with a trailing comma.
fn parse_tags(text: &str) -> Vec<String> {
    let commas = text.contains([',', '，', '、', ';', '；', '\n']);
    let parts: Vec<&str> = if commas {
        text.split([',', '，', '、', ';', '；', '\n']).collect()
    } else {
        text.split_whitespace().collect()
    };
    let mut out: Vec<String> = vec![];
    for part in parts {
        let tag = part.trim().trim_start_matches('#').trim().to_owned();
        if !tag.is_empty() && !out.contains(&tag) {
            out.push(tag);
        }
    }
    out
}

/// The text field's value for a list of tags (the inverse of `parse_tags`).
fn tags_text(tags: &[String]) -> String {
    let mut text = tags.join(", ");
    if tags.len() == 1 && tags[0].contains(char::is_whitespace) {
        text.push(',');
    }
    text
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `2024`, `2024-09`, `2024-09-12` or a full ISO time; anything else is a typo the person should see.
fn check_date(text: &str) -> Result<String, String> {
    let text = text.trim();
    let bytes = text.as_bytes();
    let digits = |from: usize, to: usize| {
        bytes
            .get(from..to)
            .is_some_and(|s| s.iter().all(u8::is_ascii_digit))
    };
    let ok = match bytes.len() {
        0 => true,
        4 => digits(0, 4),
        7 => digits(0, 4) && bytes[4] == b'-' && digits(5, 7),
        n if n >= 10 => {
            digits(0, 4)
                && bytes[4] == b'-'
                && digits(5, 7)
                && bytes[7] == b'-'
                && digits(8, 10)
                && (n == 10 || matches!(bytes[10], b'T' | b' '))
        }
        _ => false,
    };
    if ok {
        Ok(text.to_owned())
    } else {
        Err("时间要写成 2024-09-12，或只写年月 2024-09、只写年 2024".to_owned())
    }
}

/// The layer filter of the memories page: `""` (all), `"0"`, `"1+"` (from layer 1 up) or one layer `"2"`.
fn layer_filter(value: &str) -> (Option<u32>, Option<u32>) {
    if let Some(from) = value.strip_suffix('+') {
        return (from.parse().ok(), None);
    }
    match value.parse::<u32>() {
        Ok(n) => (Some(n), Some(n)),
        Err(_) => (None, None),
    }
}

/// `?a=1&b=2` from the pairs that have a value.
fn query_string(pairs: &[(&str, &str)]) -> String {
    let mut out = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs.iter().filter(|(_, v)| !v.is_empty()) {
        out.append_pair(key, value);
    }
    let query = out.finish();
    if query.is_empty() {
        String::new()
    } else {
        format!("?{query}")
    }
}

/// The part that differs between two texts: what went and what came, without the shared start and end.
fn changed_span(old: &str, new: &str) -> Option<(String, String)> {
    if old == new {
        return None;
    }
    let (o, n): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let start = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
    let end = o[start..]
        .iter()
        .rev()
        .zip(n[start..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let cut = |chars: &[char]| {
        let text: String = chars[start..chars.len() - end].iter().collect();
        if text.chars().count() > 300 {
            format!("{}…", text.chars().take(300).collect::<String>())
        } else {
            text
        }
    };
    Some((cut(&o), cut(&n)))
}

/// What the browser may do with an attachment: its type, and whether it may be shown in the page.
fn serving(mime: &str) -> (String, bool) {
    let mime = mime.trim().to_ascii_lowercase();
    let inline = mime == "text/plain"
        || mime == "application/pdf"
        || (mime.starts_with("image/") && mime != "image/svg+xml")
        || mime.starts_with("audio/")
        || mime.starts_with("video/");
    if !inline || mime.parse::<mime_guess::mime::Mime>().is_err() {
        return ("application/octet-stream".to_owned(), false);
    }
    let kind = if mime == "text/plain" {
        "text/plain; charset=utf-8".to_owned()
    } else {
        mime
    };
    (kind, true)
}

/// `Content-Disposition` with a name that cannot break out of the header (RFC 6266 and RFC 5987).
fn disposition(inline: bool, name: &str) -> String {
    let clean: String = name
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | '"' | ';'))
        .collect();
    let clean = if clean.trim().is_empty() {
        "file".to_owned()
    } else {
        clean.trim().to_owned()
    };
    let ascii: String = clean
        .chars()
        .map(|c| if c.is_ascii() && c != '%' { c } else { '_' })
        .collect();
    let mut encoded = String::new();
    for byte in clean.bytes() {
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!(
        "{}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}",
        if inline { "inline" } else { "attachment" }
    )
}

/// How a page shows an attachment.
fn attachment_kind(a: &AttachmentView) -> &'static str {
    let mime = a.mime.to_ascii_lowercase();
    if mime == "application/pdf" {
        "pdf"
    } else if mime.starts_with("image/") && mime != "image/svg+xml" {
        "image"
    } else if mime.starts_with("audio/") {
        "audio"
    } else if mime.starts_with("video/") {
        "video"
    } else if mime == "text/plain" {
        "text"
    } else {
        "file"
    }
}

fn ok_notice(key: &str, moved: Option<&str>) -> Option<String> {
    Some(match key {
        "created" => "已新建".to_owned(),
        "saved" => "已保存".to_owned(),
        "same" => "没有改动".to_owned(),
        "touched" => "已确认：这条仍然正确".to_owned(),
        "moved" => "已移动".to_owned(),
        "restored" => "已从回收站恢复".to_owned(),
        "purged" => "已永久删除".to_owned(),
        "emptied" => "回收站已清空".to_owned(),
        "deleted" => match moved
            .and_then(|m| m.parse::<usize>().ok())
            .filter(|m| *m > 0)
        {
            Some(n) => format!("已移到回收站，它下面的 {n} 个节点上移了一层"),
            None => "已移到回收站".to_owned(),
        },
        _ => return None,
    })
}

fn broken(error: &anyhow::Error) -> Response {
    crate::log!("Page error: {error:#}");
    (StatusCode::INTERNAL_SERVER_ERROR, failure(error)).into_response()
}

fn with_status(mut response: Response, status: StatusCode) -> Response {
    *response.status_mut() = status;
    response
}

fn brief_value(b: &Brief) -> Value {
    json!(b)
}

/// The change feed row: who, what, when, and the node it is about.
fn change_value(c: &ChangeView) -> Value {
    json!({
        "at": c.at,
        "who": actor_label(&c.actor),
        "label": op_label(&c.op),
        "op": c.op,
        "reason": c.reason,
        "node": c.node,
    })
}

// ---------- overview ----------

/// One row of the layer table, from the top layer down.
fn layer_rows(stats: &Stats) -> Vec<Value> {
    let widest = stats
        .layers
        .iter()
        .map(|l| l.count)
        .max()
        .unwrap_or(1)
        .max(1);
    let top = stats.layers.last().map(|l| l.layer);
    stats
        .layers
        .iter()
        .rev()
        .map(|l: &LayerStat| {
            let open = l.count.saturating_sub(l.covered);
            let state = if Some(l.layer) == top && l.layer > 0 {
                "top"
            } else if open > 0 {
                "open"
            } else {
                "done"
            };
            json!({
                "layer": l.layer,
                "count": l.count,
                "width": (l.count * 100 / widest).max(2),
                "state": state,
                "open": open,
            })
        })
        .collect()
}

fn gaps_value(m: &MaintenanceView) -> Value {
    let Some(g) = &m.gaps else { return Value::Null };
    let missing = m
        .layers
        .iter()
        .find(|l| l.layer == g.layer)
        .map_or(0, |l| l.count.saturating_sub(l.covered));
    json!({
        "layer": g.layer,
        "next": g.layer + 1,
        "missing": missing,
        "extend": g.extend.iter().take(5).map(|e| json!({
            "summary": e.summary,
            "nodes": e.nodes.iter().take(6).map(brief_value).collect::<Vec<_>>(),
            "count": e.nodes.len(),
        })).collect::<Vec<_>>(),
        "extend_more": g.extend.len().saturating_sub(5),
        "groups": g.groups.iter().take(4).map(|group| json!({
            "nodes": group.iter().take(6).map(brief_value).collect::<Vec<_>>(),
            "count": group.len(),
        })).collect::<Vec<_>>(),
        "groups_more": g.groups.len().saturating_sub(4),
    })
}

fn stale_value(m: &MaintenanceView) -> Value {
    json!({
        "total": m.stale.len(),
        "items": m.stale.iter().take(8).map(|s| json!({
            "summary": s.summary,
            "newer": s.newer.iter().take(4).map(brief_value).collect::<Vec<_>>(),
            "count": s.newer.len(),
        })).collect::<Vec<_>>(),
        "more": m.stale.len().saturating_sub(8),
    })
}

async fn overview(State(w): State<Web>) -> Response {
    let data = w
        .service
        .run(|s| {
            let stats = s.stats();
            let backups = if stats.nodes == 0 {
                s.backups().len()
            } else {
                0
            };
            (stats, s.maintenance(), s.activity(20), backups)
        })
        .await;
    let (stats, maintenance, changes, backups) = match data {
        Ok(d) => d,
        Err(e) => return broken(&e),
    };
    let top = stats.layers.last().map_or(0, |l| l.layer);
    w.render(json!({
        "View": "overview",
        "Title": "概览",
        "Empty": stats.nodes == 0,
        "OldBackups": backups > 0,
        "Nodes": stats.nodes,
        "Top": top,
        "TopCount": stats.layers.last().map_or(0, |l| l.count),
        "Deleted": stats.deleted,
        "Layers": layer_rows(&stats),
        "Gaps": gaps_value(&maintenance),
        "Stale": stale_value(&maintenance),
        "Untitled": json!({
            "total": maintenance.untitled.len(),
            "items": maintenance.untitled.iter().take(8).map(brief_value).collect::<Vec<_>>(),
            "more": maintenance.untitled.len().saturating_sub(8),
        }),
        "Whole": maintenance.gaps.is_none() && maintenance.stale.is_empty() && maintenance.untitled.is_empty(),
        "Changes": changes.iter().map(change_value).collect::<Vec<_>>(),
    }))
}

// ---------- tree ----------

async fn tree(State(w): State<Web>) -> Response {
    let nodes = w.service.run(|s| s.stats().nodes).await;
    match nodes {
        Ok(nodes) => w.render(json!({"View": "tree", "Title": "树", "Empty": nodes == 0})),
        Err(e) => broken(&e),
    }
}

// ---------- memories ----------

async fn memories(State(w): State<Web>, Query(p): Query<HashMap<String, String>>) -> Response {
    let param = |key: &str| p.get(key).map(|v| v.trim().to_owned()).unwrap_or_default();
    let (q, layer, tag, from, through) = (
        param("q"),
        param("layer"),
        param("tag"),
        param("from"),
        param("through"),
    );
    let order = match param("order").as_str() {
        "created" => Order::Created,
        "occurred" => Order::Occurred,
        _ => Order::Updated,
    };
    let order_name = match order {
        Order::Updated => "",
        Order::Created => "created",
        Order::Occurred => "occurred",
    };
    let page: usize = param("page").parse().ok().filter(|n| *n >= 1).unwrap_or(1);
    let mut error = None;
    let mut date = |value: &str| {
        check_date(value).unwrap_or_else(|e| {
            error = Some(e);
            String::new()
        })
    };
    let (from_ok, through_ok) = (date(&from), date(&through));
    let (min_layer, max_layer) = layer_filter(&layer);
    let tag_filter = (!tag.is_empty()).then(|| tag.clone());
    let searching = !q.is_empty();
    let work = {
        let (q, tag_filter, from, through) = (
            q.clone(),
            tag_filter.clone(),
            from_ok.clone(),
            through_ok.clone(),
        );
        move |s: &Service| {
            let stats = s.stats();
            if searching {
                let result = s.search(&SearchOpts {
                    query: q,
                    k: SEARCH_HITS,
                    min_layer,
                    max_layer,
                    since: from,
                    until: through,
                    tag: tag_filter,
                    expand: 4,
                    ..SearchOpts::default()
                });
                (stats, Some(result), None)
            } else {
                let list = s.list(&ListQuery {
                    tag: tag_filter,
                    min_layer,
                    max_layer,
                    since: from,
                    until: through,
                    order,
                    offset: (page - 1) * PAGE_SIZE,
                    limit: PAGE_SIZE,
                });
                (stats, None, Some(list))
            }
        }
    };
    let (stats, search, list) = match w.service.run(work).await {
        Ok(d) => d,
        Err(e) => return broken(&e),
    };
    let top = stats.layers.last().map_or(0, |l| l.layer);
    // The filter chips are links that keep everything but the tag.
    let tag_link = |name: &str| {
        query_string(&[
            ("q", q.as_str()),
            ("layer", layer.as_str()),
            ("tag", name),
            ("from", from_ok.as_str()),
            ("through", through_ok.as_str()),
            ("order", order_name),
        ])
    };
    let mut layer_options = vec![
        json!({"value": "", "label": "全部层级"}),
        json!({"value": "0", "label": "记忆本身（第 0 层）"}),
    ];
    if top >= 1 {
        layer_options.push(json!({"value": "1+", "label": "汇总（第 1 层及以上）"}));
    }
    for n in 1..=top.min(8) {
        layer_options.push(json!({"value": n.to_string(), "label": format!("只看第 {n} 层")}));
    }
    let mut model = json!({
        "View": "memories",
        "Title": if searching { format!("搜索：{q}") } else { "记忆".to_owned() },
        "Q": q, "Layer": layer, "Tag": tag, "From": from, "Through": through, "Order": order_name,
        "LayerOptions": layer_options,
        "Tags": stats.tags.iter().map(|(name, count)| json!({"name": name, "count": count, "link": tag_link(name)})).collect::<Vec<_>>(),
        "ClearTag": tag_link(""),
        "Nodes": stats.nodes,
        "Filtered": !(layer.is_empty() && tag.is_empty() && from.is_empty() && through.is_empty()),
    });
    if let Some(e) = error {
        model["Error"] = json!(e);
    }
    match (search, list) {
        (Some(Ok(result)), _) => {
            model["Mode"] = json!("search");
            model["NoStrong"] = json!(result.no_strong_match);
            model["Hits"] = json!(
                result
                    .hits
                    .iter()
                    .map(|h| {
                        let mut path: Vec<_> = h.path.iter().collect();
                        path.reverse();
                        json!({
                            "id": h.id, "layer": h.layer, "title": h.title, "content": h.content, "truncated": h.truncated,
                            "tags": h.tags, "occurred": h.occurred, "exact": h.exact,
                            "matched": match h.matched { "both" => "关键词和语义都命中", "keyword" => "关键词命中", _ => "语义相近" },
                            "strong": h.strong,
                            "path": path, "expanded": h.expanded,
                        })
                    })
                    .collect::<Vec<_>>()
            );
        }
        (Some(Err(e)), _) => {
            model["Mode"] = json!("search");
            model["Hits"] = json!([]);
            model["Error"] = json!(failure(&e));
        }
        (None, Some(list)) => {
            let pages = list.total.div_ceil(PAGE_SIZE).max(1);
            let link = |n: usize| {
                let page = if n > 1 { n.to_string() } else { String::new() };
                query_string(&[
                    ("layer", layer.as_str()),
                    ("tag", tag.as_str()),
                    ("from", from_ok.as_str()),
                    ("through", through_ok.as_str()),
                    ("order", order_name),
                    ("page", page.as_str()),
                ])
            };
            model["Mode"] = json!("list");
            model["Total"] = json!(list.total);
            model["Items"] = json!(list.items);
            model["PageNo"] = json!(page);
            model["Pages"] = json!(pages);
            model["Prev"] = json!(if page > 1 { Some(link(page - 1)) } else { None });
            model["Next"] = json!(if page < pages {
                Some(link(page + 1))
            } else {
                None
            });
        }
        (None, None) => {}
    }
    w.render(model)
}

// ---------- one node ----------

/// A revision with the words for it and what changed since the next older one.
fn revision_values(revisions: &[Revision]) -> Vec<Value> {
    revisions
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let older = revisions.get(i + 1);
            let diff = older.and_then(|o| changed_span(&o.content, &r.content));
            json!({
                "version": r.version, "label": op_label(&r.op), "op": r.op, "at": r.at,
                "who": actor_label(&r.actor), "reason": r.reason, "content": r.content,
                "title": r.title, "old_title": older.map(|o| o.title.clone()),
                "title_changed": older.is_some_and(|o| o.title != r.title),
                "removed": diff.as_ref().map(|d| d.0.clone()),
                "added": diff.as_ref().map(|d| d.1.clone()),
                "same": older.is_some() && diff.is_none() && r.op == "update",
            })
        })
        .collect()
}

fn attachment_values(list: &[AttachmentView]) -> Vec<Value> {
    list.iter()
        .map(|a| {
            let mut value = json!(a);
            value["kind"] = json!(attachment_kind(a));
            value["size_text"] = json!(human(a.size));
            value["referenced"] = json!(matches!(a.storage, crate::tree::Storage::Referenced));
            value
        })
        .collect()
}

fn node_model(node: &NodeView, error: Option<String>, notice: Option<String>) -> Value {
    let mut model = json!({
        "View": "node",
        "Title": node.label,
        "Node": node,
        "Attachments": attachment_values(&node.attachments),
        "Revisions": revision_values(&node.revisions),
        "Pdf": node.attachments.iter().any(|a| attachment_kind(a) == "pdf" && a.available),
    });
    if let Some(e) = error {
        model["Error"] = json!(e);
    }
    if let Some(n) = notice {
        model["Notice"] = json!(n);
    }
    model
}

fn render_node(
    w: &Web,
    node: &NodeView,
    error: Option<String>,
    notice: Option<String>,
) -> Response {
    let pdf = node
        .attachments
        .iter()
        .any(|a| attachment_kind(a) == "pdf" && a.available);
    let mut response = w.render(node_model(node, error, notice));
    if pdf {
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(PDF_POLICY),
        );
    }
    response
}

fn missing_node(w: &Web, error: &anyhow::Error) -> Response {
    with_status(
        w.render(
            json!({"View": "node", "Title": "没有这个节点", "Node": null, "Error": failure(error)}),
        ),
        StatusCode::NOT_FOUND,
    )
}

async fn load(w: &Web, id: &str) -> Result<NodeView> {
    let id = id.to_owned();
    w.service.run(move |s| s.get(&id)).await?
}

async fn node_page(
    State(w): State<Web>,
    Path(id): Path<String>,
    Query(p): Query<HashMap<String, String>>,
) -> Response {
    match load(&w, &id).await {
        Ok(node) => {
            let notice = p
                .get("ok")
                .and_then(|k| ok_notice(k, p.get("moved").map(String::as_str)));
            render_node(&w, &node, None, notice)
        }
        Err(e) => missing_node(&w, &e),
    }
}

async fn touch_node(
    State(w): State<Web>,
    Path(id): Path<String>,
    Form(f): Form<HashMap<String, String>>,
) -> Response {
    let version = f.get("version").and_then(|v| v.parse().ok());
    let target = id.clone();
    let done = w
        .service
        .run(move |s| s.touch(&target, version, ACTOR))
        .await;
    match done.and_then(|r| r) {
        Ok(_) => Redirect::to(&format!("/node/{id}?ok=touched")).into_response(),
        Err(e) => failed_action(&w, &id, &e).await,
    }
}

async fn move_node(
    State(w): State<Web>,
    Path(id): Path<String>,
    Form(f): Form<HashMap<String, String>>,
) -> Response {
    let top = f.contains_key("top");
    let to = f
        .get("parent")
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty());
    if !top && to.is_none() {
        return failed_action(
            &w,
            &id,
            &anyhow::anyhow!("请先选一个新的位置，或者选择「设为顶层」"),
        )
        .await;
    }
    let to = if top { None } else { to };
    let target = id.clone();
    let done = w
        .service
        .run(move |s| s.move_nodes(&[target], to.as_deref(), ACTOR))
        .await;
    match done.and_then(|r| r) {
        Ok(()) => Redirect::to(&format!("/node/{id}?ok=moved")).into_response(),
        Err(e) => failed_action(&w, &id, &e).await,
    }
}

async fn delete_node(
    State(w): State<Web>,
    Path(id): Path<String>,
    Form(f): Form<HashMap<String, String>>,
) -> Response {
    let version = f.get("version").and_then(|v| v.parse().ok());
    let reason = f
        .get("reason")
        .map(|r| r.trim().to_owned())
        .unwrap_or_default();
    let target = id.clone();
    let done = w
        .service
        .run(move |s| s.delete(&target, version, &reason, ACTOR))
        .await;
    match done.and_then(|r| r) {
        Ok(moved) => Redirect::to(&format!("/trash?ok=deleted&moved={moved}")).into_response(),
        Err(e) => failed_action(&w, &id, &e).await,
    }
}

/// A button on the node page failed: show the page again with the reason.
async fn failed_action(w: &Web, id: &str, error: &anyhow::Error) -> Response {
    match load(w, id).await {
        Ok(node) => with_status(
            render_node(w, &node, Some(failure(error)), None),
            StatusCode::BAD_REQUEST,
        ),
        Err(_) => missing_node(w, error),
    }
}

// ---------- the editor ----------

/// What the editor form sent.
#[derive(Default)]
struct Editor {
    title: String,
    content: String,
    tags: String,
    occurred: String,
    sources: String,
    parent: String,
    version: Option<u32>,
    refs: Vec<String>,
    remove: Vec<String>,
    files: Vec<AttachInput>,
    /// File names chosen, for telling the person they must be chosen again after an error.
    chosen: usize,
}

async fn read_editor(mut form: Multipart) -> Result<Editor, String> {
    let mut e = Editor::default();
    let upload =
        |e: axum::extract::multipart::MultipartError| format!("上传失败：{}", e.body_text());
    while let Some(field) = form.next_field().await.map_err(upload)? {
        let name = field.name().unwrap_or_default().to_owned();
        if name == "file" {
            let file_name = field.file_name().unwrap_or_default().to_owned();
            let bytes = field.bytes().await.map_err(upload)?;
            if file_name.is_empty() && bytes.is_empty() {
                continue;
            }
            e.chosen += 1;
            if bytes.len() > MAX_FILE {
                return Err(format!("「{file_name}」超过了 64 MB，请换一个小一点的文件"));
            }
            e.files.push(AttachInput {
                bytes: Some(bytes.to_vec()),
                name: Some(file_name),
                ..AttachInput::default()
            });
            continue;
        }
        let text = field.text().await.map_err(upload)?;
        match name.as_str() {
            "title" => e.title = text,
            "content" => e.content = text,
            "tags" => e.tags = text,
            "occurred" => e.occurred = text,
            "sources" => e.sources = text,
            "parent" => e.parent = text.trim().to_owned(),
            "version" => e.version = text.trim().parse().ok(),
            "refs" if !text.trim().is_empty() => e.refs.push(text.trim().to_owned()),
            "remove" if !text.trim().is_empty() => e.remove.push(text.trim().to_owned()),
            _ => {}
        }
    }
    Ok(e)
}

/// Pick-list entries for node ids (the editor shows references as chips with their first line).
fn labels(s: &Service, ids: &[String]) -> Vec<Value> {
    ids.iter()
        .filter_map(|id| s.get(id).ok().filter(|n| !n.deleted))
        .map(|n| json!({"id": n.id, "title": n.label, "layer": n.layer}))
        .collect()
}

struct EditorPage<'a> {
    /// The node being edited, or none for a new one.
    node: Option<&'a NodeView>,
    form: &'a Editor,
    error: Option<String>,
}

async fn render_editor(w: &Web, page: EditorPage<'_>) -> Response {
    let (refs, parent) = {
        let (refs, parent) = (page.form.refs.clone(), page.form.parent.clone());
        w.service
            .run(move |s| {
                let path = if parent.is_empty() {
                    Value::Null
                } else {
                    s.get(&parent)
                        .map_or(Value::Null, |p| json!({"id": p.id, "title": p.label}))
                };
                (labels(s, &refs), path)
            })
            .await
            .unwrap_or_default()
    };
    let conflict = page
        .error
        .as_deref()
        .is_some_and(|e| e.contains("已被改动"));
    let mut model = json!({
        "View": "editor",
        "Title": if page.node.is_some() { "编辑节点" } else { "新建节点" },
        "Node": page.node,
        "Attachments": page.node.map(|n| attachment_values(&n.attachments)).unwrap_or_default(),
        "Form": {
            "title": page.form.title, "content": page.form.content, "tags": page.form.tags, "occurred": page.form.occurred,
            "sources": page.form.sources, "parent": page.form.parent, "version": page.form.version,
            "refs": refs, "remove": page.form.remove, "chosen": page.form.chosen,
        },
        "Parent": parent,
        "Conflict": conflict,
    });
    if let Some(e) = page.error {
        model["Error"] = json!(e);
    }
    w.render(model)
}

/// The form as it starts for an existing node.
fn form_of(node: &NodeView) -> Editor {
    Editor {
        title: node.title.clone(),
        content: node.content.clone(),
        tags: tags_text(&node.tags),
        occurred: node.occurred.clone(),
        sources: node.sources.join("\n"),
        parent: node.parent.clone().unwrap_or_default(),
        version: Some(node.version),
        refs: node.refs.iter().map(|r| r.id.clone()).collect(),
        ..Editor::default()
    }
}

async fn new_node(State(w): State<Web>, Query(p): Query<HashMap<String, String>>) -> Response {
    let form = Editor {
        parent: p
            .get("parent")
            .map(|p| p.trim().to_owned())
            .unwrap_or_default(),
        ..Editor::default()
    };
    render_editor(
        &w,
        EditorPage {
            node: None,
            form: &form,
            error: None,
        },
    )
    .await
}

async fn edit_node(State(w): State<Web>, Path(id): Path<String>) -> Response {
    match load(&w, &id).await {
        Ok(node) if node.deleted => Redirect::to(&format!("/node/{id}")).into_response(),
        Ok(node) => {
            let form = form_of(&node);
            render_editor(
                &w,
                EditorPage {
                    node: Some(&node),
                    form: &form,
                    error: None,
                },
            )
            .await
        }
        Err(e) => missing_node(&w, &e),
    }
}

async fn create_node(State(w): State<Web>, multipart: Multipart) -> Response {
    let form = match read_editor(multipart).await {
        Ok(f) => f,
        Err(e) => {
            return with_status(
                render_editor(
                    &w,
                    EditorPage {
                        node: None,
                        form: &Editor::default(),
                        error: Some(e),
                    },
                )
                .await,
                StatusCode::BAD_REQUEST,
            );
        }
    };
    let result = match check_date(&form.occurred) {
        Err(e) => Err(anyhow::anyhow!(e)),
        Ok(occurred) => {
            let input = WriteInput {
                title: form.title.clone(),
                content: form.content.clone(),
                tags: parse_tags(&form.tags),
                parent: (!form.parent.is_empty()).then(|| form.parent.clone()),
                refs: form.refs.clone(),
                sources: lines(&form.sources),
                occurred,
                adopt: vec![],
                attachments: form.files.clone(),
            };
            w.service
                .run(move |s| s.write(input, ACTOR))
                .await
                .and_then(|r| r)
        }
    };
    match result {
        Ok(node) => Redirect::to(&format!("/node/{}?ok=created", node.id)).into_response(),
        Err(e) => with_status(
            render_editor(
                &w,
                EditorPage {
                    node: None,
                    form: &form,
                    error: Some(failure(&e)),
                },
            )
            .await,
            StatusCode::BAD_REQUEST,
        ),
    }
}

/// What the form changes about a node: only the differing fields go to the service.
fn update_input(node: &NodeView, form: &Editor, occurred: String) -> UpdateInput {
    let tags = parse_tags(&form.tags);
    let sources = lines(&form.sources);
    let refs: Vec<String> = node.refs.iter().map(|r| r.id.clone()).collect();
    UpdateInput {
        title: (form.title.trim() != node.title).then(|| form.title.clone()),
        content: (form.content.trim() != node.content).then(|| form.content.clone()),
        tags: (tags != node.tags).then_some(tags),
        occurred: (occurred != node.occurred).then_some(occurred),
        refs: (form.refs != refs).then(|| form.refs.clone()),
        sources: (sources != node.sources).then_some(sources),
        add_attachments: form.files.clone(),
        remove_attachments: form.remove.clone(),
    }
}

fn is_unchanged(input: &UpdateInput) -> bool {
    input.title.is_none()
        && input.content.is_none()
        && input.tags.is_none()
        && input.occurred.is_none()
        && input.refs.is_none()
        && input.sources.is_none()
        && input.add_attachments.is_empty()
        && input.remove_attachments.is_empty()
}

async fn save_node(State(w): State<Web>, Path(id): Path<String>, multipart: Multipart) -> Response {
    let node = match load(&w, &id).await {
        Ok(n) if !n.deleted => n,
        Ok(_) => return Redirect::to(&format!("/node/{id}")).into_response(),
        Err(e) => return missing_node(&w, &e),
    };
    let form = match read_editor(multipart).await {
        Ok(f) => f,
        Err(e) => {
            let form = form_of(&node);
            return with_status(
                render_editor(
                    &w,
                    EditorPage {
                        node: Some(&node),
                        form: &form,
                        error: Some(e),
                    },
                )
                .await,
                StatusCode::BAD_REQUEST,
            );
        }
    };
    let result = match check_date(&form.occurred) {
        Err(e) => Err(anyhow::anyhow!(e)),
        Ok(occurred) => {
            let input = update_input(&node, &form, occurred);
            if is_unchanged(&input) {
                return Redirect::to(&format!("/node/{id}?ok=same")).into_response();
            }
            let (target, version) = (id.clone(), form.version);
            w.service
                .run(move |s| s.update(&target, version, input, ACTOR))
                .await
                .and_then(|r| r)
        }
    };
    match result {
        Ok(_) => Redirect::to(&format!("/node/{id}?ok=saved")).into_response(),
        Err(e) => with_status(
            render_editor(
                &w,
                EditorPage {
                    node: Some(&node),
                    form: &form,
                    error: Some(failure(&e)),
                },
            )
            .await,
            StatusCode::BAD_REQUEST,
        ),
    }
}

// ---------- the recycle bin ----------

async fn trash(State(w): State<Web>, Query(p): Query<HashMap<String, String>>) -> Response {
    let notice = p
        .get("ok")
        .and_then(|k| ok_notice(k, p.get("moved").map(String::as_str)));
    render_trash(&w, notice, None).await
}

async fn render_trash(w: &Web, notice: Option<String>, error: Option<String>) -> Response {
    let rows = w
        .service
        .run(|s| {
            s.trash()
                .into_iter()
                .map(|b| {
                    let (reason, who) = s
                        .get(&b.id)
                        .ok()
                        .and_then(|n| {
                            n.revisions
                                .first()
                                .map(|r| (r.reason.clone(), actor_label(&r.actor)))
                        })
                        .unwrap_or_default();
                    json!({"node": b, "reason": reason, "who": who})
                })
                .collect::<Vec<_>>()
        })
        .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => return broken(&e),
    };
    let mut model = json!({"View": "trash", "Title": "回收站", "Rows": rows});
    if let Some(n) = notice {
        model["Notice"] = json!(n);
    }
    if let Some(e) = error {
        model["Error"] = json!(e);
        return with_status(w.render(model), StatusCode::BAD_REQUEST);
    }
    w.render(model)
}

async fn restore_node(State(w): State<Web>, Path(id): Path<String>) -> Response {
    let target = id.clone();
    match w
        .service
        .run(move |s| s.restore(&target, ACTOR))
        .await
        .and_then(|r| r)
    {
        Ok(_) => Redirect::to(&format!("/node/{id}?ok=restored")).into_response(),
        Err(e) => render_trash(&w, None, Some(failure(&e))).await,
    }
}

async fn purge_node(State(w): State<Web>, Path(id): Path<String>) -> Response {
    match w.service.run(move |s| s.purge(&[id])).await.and_then(|r| r) {
        Ok(_) => Redirect::to("/trash?ok=purged").into_response(),
        Err(e) => render_trash(&w, None, Some(failure(&e))).await,
    }
}

async fn empty_trash(State(w): State<Web>) -> Response {
    match w.service.run(|s| s.empty_trash()).await.and_then(|r| r) {
        Ok(_) => Redirect::to("/trash?ok=emptied").into_response(),
        Err(e) => render_trash(&w, None, Some(failure(&e))).await,
    }
}

// ---------- attachment files ----------

async fn file(State(w): State<Web>, Path(hash): Path<String>, req: Request) -> Response {
    let found = w
        .service
        .run(move |s| s.attachment(&hash))
        .await
        .ok()
        .flatten();
    // Files that were only referenced stay where they are: the browser is never pointed at arbitrary paths.
    let Some(a) =
        found.filter(|a| a.available && matches!(a.storage, crate::tree::Storage::Stored))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (mime, inline) = serving(&a.mime);
    let Ok(parsed) = mime.parse::<mime_guess::mime::Mime>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let policy = if a.mime.eq_ignore_ascii_case("application/pdf") {
        "default-src 'none'; frame-ancestors 'self'"
    } else {
        "default-src 'none'; sandbox"
    };
    match ServeFile::new_with_mime(&a.path, &parsed)
        .oneshot(req)
        .await
    {
        Ok(res) => {
            let mut res = res.map(Body::new);
            let h = res.headers_mut();
            if let Ok(v) = disposition(inline, &a.name).parse() {
                h.insert(header::CONTENT_DISPOSITION, v);
            }
            h.insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(policy),
            );
            h.insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            res.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

// ---------- JSON for the pages' scripts ----------

/// `?root=<id>&depth=<n>`: the tree from the top (or from one node) `depth` levels deep (default 3, at most 12).
async fn api_outline(State(w): State<Web>, Query(q): Query<HashMap<String, String>>) -> Response {
    let root = q
        .get("root")
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty());
    let depth = q
        .get("depth")
        .and_then(|d| d.parse::<usize>().ok())
        .unwrap_or(3)
        .min(12);
    match w
        .service
        .run(move |s| s.outline(root.as_deref(), depth))
        .await
        .and_then(|r| r)
    {
        Ok(tree) => Json(tree).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": failure(&e)}))).into_response(),
    }
}

async fn api_node(State(w): State<Web>, Path(id): Path<String>) -> Response {
    match load(&w, &id).await {
        Ok(node) => Json(node).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": failure(&e)}))).into_response(),
    }
}

async fn api_search(State(w): State<Web>, Query(p): Query<HashMap<String, String>>) -> Response {
    let query = p.get("q").map(|q| q.trim().to_owned()).unwrap_or_default();
    if query.is_empty() {
        return Json(json!({"hits": [], "no_strong_match": false})).into_response();
    }
    let opts = SearchOpts {
        query,
        k: p.get("k")
            .and_then(|k| k.parse().ok())
            .unwrap_or(8usize)
            .clamp(1, 50),
        scope: p
            .get("scope")
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty()),
        expand: 0,
        ..SearchOpts::default()
    };
    match w
        .service
        .run(move |s| s.search(&opts))
        .await
        .and_then(|r| r)
    {
        Ok(result) => Json(result).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"error": failure(&e)}))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_split_on_commas_or_spaces() {
        assert_eq!(parse_tags("a b  c"), ["a", "b", "c"]);
        assert_eq!(parse_tags("项目 A, 设备，人"), ["项目 A", "设备", "人"]);
        assert_eq!(parse_tags("my project,"), ["my project"]);
        assert_eq!(parse_tags("#x #x"), ["x"]);
        assert!(parse_tags("  ,, ").is_empty());
    }

    #[test]
    fn tags_text_round_trips() {
        for tags in [
            vec![],
            vec!["a".to_owned()],
            vec!["my project".to_owned()],
            vec!["x y".to_owned(), "z".to_owned()],
        ] {
            assert_eq!(parse_tags(&tags_text(&tags)), tags);
        }
    }

    #[test]
    fn dates_accept_year_month_day() {
        for ok in [
            "",
            "2024",
            "2024-09",
            "2024-09-12",
            "2024-09-12T08:00:00Z",
            " 2024-09 ",
        ] {
            assert!(check_date(ok).is_ok(), "{ok}");
        }
        for bad in [
            "24",
            "2024-9",
            "2024-09-1",
            "2024/09/12",
            "yesterday",
            "2024-09-12x",
        ] {
            assert!(check_date(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn layer_filter_reads_ranges() {
        assert_eq!(layer_filter(""), (None, None));
        assert_eq!(layer_filter("0"), (Some(0), Some(0)));
        assert_eq!(layer_filter("1+"), (Some(1), None));
        assert_eq!(layer_filter("x"), (None, None));
    }

    #[test]
    fn query_strings_skip_empty_values() {
        assert_eq!(query_string(&[("q", ""), ("tag", "a b")]), "?tag=a+b");
        assert_eq!(query_string(&[("q", "")]), "");
    }

    #[test]
    fn changed_span_leaves_out_the_shared_ends() {
        assert_eq!(
            changed_span("我喜欢咖啡", "我喜欢红茶"),
            Some(("咖啡".into(), "红茶".into()))
        );
        assert_eq!(changed_span("a", "a"), None);
        assert_eq!(
            changed_span("abc", "abcd"),
            Some((String::new(), "d".into()))
        );
        assert_eq!(changed_span("aXa", "aa"), Some(("X".into(), String::new())));
    }

    #[test]
    fn only_safe_types_are_shown_inline() {
        assert!(serving("image/png").1);
        assert!(serving("application/pdf").1);
        assert_eq!(
            serving("text/plain"),
            ("text/plain; charset=utf-8".to_owned(), true)
        );
        for risky in [
            "text/html",
            "image/svg+xml",
            "application/xhtml+xml",
            "text/xml",
            "application/javascript",
            "not a type",
        ] {
            assert_eq!(
                serving(risky),
                ("application/octet-stream".to_owned(), false),
                "{risky}"
            );
        }
    }

    #[test]
    fn disposition_cannot_break_out() {
        let d = disposition(false, "a\"b;c\r\nx/y.txt");
        assert!(!d.contains('\r') && !d.contains('\n'));
        assert_eq!(d.matches('"').count(), 2);
        assert!(d.starts_with("attachment; filename=\"abcxy.txt\""));
        let d = disposition(true, "报告 1.pdf");
        assert!(d.starts_with("inline; filename=\"__ 1.pdf\"; filename*=UTF-8''"));
        assert!(d.ends_with("%E6%8A%A5%E5%91%8A%201.pdf"));
        assert!(disposition(false, "  ").contains("filename=\"file\""));
    }

    #[test]
    fn events_have_words() {
        for op in [
            "write", "update", "touch", "reparent", "delete", "restore", "import",
        ] {
            assert_ne!(op_label(op), "改动", "{op}");
        }
        assert_eq!(actor_label("web"), "你");
        assert_eq!(actor_label("Claude"), "Claude");
    }

    #[test]
    fn layers_run_from_the_top_down() {
        let stats = Stats {
            nodes: 14,
            deleted: 0,
            roots: 1,
            layers: vec![
                LayerStat {
                    layer: 0,
                    count: 10,
                    covered: 8,
                },
                LayerStat {
                    layer: 1,
                    count: 3,
                    covered: 3,
                },
                LayerStat {
                    layer: 2,
                    count: 1,
                    covered: 0,
                },
            ],
            tags: vec![],
            files: Default::default(),
            last_change: String::new(),
        };
        let rows = layer_rows(&stats);
        let states: Vec<_> = rows
            .iter()
            .map(|r| (r["layer"].as_u64().unwrap(), r["state"].as_str().unwrap()))
            .collect();
        assert_eq!(states, [(2, "top"), (1, "done"), (0, "open")]);
        assert_eq!(rows[2]["open"], 2);
        assert_eq!(rows[2]["width"], 100);
    }
}
