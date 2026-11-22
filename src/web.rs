use crate::{
    app::App, clients, detail::Link, files::AttachmentView, jobs::Jobs, mcp::Mcp, memory::*,
    store::Store, web_admin,
};
use anyhow::Result;
use axum::{
    Form, Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query as Params, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use minijinja::Environment;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    sync::Arc,
};

#[derive(Clone)]
pub(crate) struct Web {
    pub store: Arc<Store>,
    pub base: String,
    pub host: String,
    pub alternate: String,
    pub templates: Arc<Environment<'static>>,
    pub jobs: Arc<Jobs>,
    pub app: Arc<App>,
}
pub(crate) type Fields = HashMap<String, String>;

pub fn router(store: Arc<Store>, address: SocketAddr, app: Arc<App>) -> Result<Router> {
    let mut templates = Environment::new();
    templates.add_function("local_time", |value: String, seconds: Option<bool>| {
        chrono::DateTime::parse_from_rfc3339(&value)
            .map(|date| {
                date.with_timezone(&chrono::Local)
                    .format(if seconds.unwrap_or(false) {
                        "%Y-%m-%d %H:%M:%S"
                    } else {
                        "%Y-%m-%d %H:%M"
                    })
                    .to_string()
            })
            .unwrap_or(value)
    });
    macro_rules! templates { ($($name:literal),+) => { $(templates.add_template(concat!($name,".html"),include_str!(concat!("../web/",$name,".html")))?;)+ }; }
    templates!(
        "page",
        "memories",
        "rows",
        "search",
        "overview",
        "editor",
        "memory",
        "relations",
        "date-control",
        "graph-page",
        "entities",
        "entity",
        "data",
        "mcp-guide",
        "copy-button"
    );
    let web = Web {
        store: store.clone(),
        base: format!("http://{address}"),
        host: address.to_string(),
        alternate: format!("localhost:{}", address.port()),
        templates: Arc::new(templates),
        jobs: Arc::new(Jobs::default()),
        app: app.clone(),
    };
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts([web.host.clone(), web.alternate.clone()])
        .with_allowed_origins([web.base.clone(), format!("http://{}", web.alternate)]);
    let shared = Arc::new(crate::mcp::Shared::default());
    shared.keep_fresh(store.clone());
    let mcp = StreamableHttpService::new(
        move || Ok(Mcp::new(store.clone(), shared.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let app = Router::new()
        .route_service("/mcp", mcp)
        .route("/health", get(|| async { "ok\n" }))
        .route("/", get(overview))
        .route("/memories", get(memories_page))
        .route("/graph", get(graph_page))
        .route("/graph-data", get(graph_data))
        .route("/graph-hits", get(graph_hits))
        .route("/graph-lanes", get(graph_lanes))
        .route("/memory/{id}/peek", get(peek))
        .route("/memory/new", get(new_memory).post(create))
        .route("/memory/{id}", get(detail).post(edit))
        .route("/memory/{id}/edit", get(edit_page))
        .route("/memory/{id}/delete", post(delete))
        .route("/memory/{id}/links", post(link))
        .route("/memory/{id}/entities", post(memory_entities))
        .route("/memory/{id}/attachments", post(memory_attachments))
        .route("/entities", get(entities_page))
        .route("/entity/{id}", get(entity_page).post(entity_update))
        .route("/entity/{id}/merge", post(entity_merge))
        .route("/entity/{id}/unmerge", post(entity_unmerge))
        .route("/entity/{id}/delete", post(entity_delete))
        .route("/entity-search", get(entity_search))
        .route(
            "/clients",
            get(|State(w): State<Web>| async move {
                w.render(json!({"View":"clients","Rules":clients::RULES.trim_end()}))
            }),
        )
        .merge(web_admin::routes());
    macro_rules! asset {
        ($app:ident,$name:literal,$mime:literal) => {
            let $app = $app.route(
                concat!("/", $name),
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, $mime)],
                        include_str!(concat!("../web/", $name)),
                    )
                }),
            );
        };
    }
    asset!(app, "style.css", "text/css; charset=utf-8");
    asset!(app, "icon.svg", "image/svg+xml");
    asset!(app, "theme.js", "text/javascript; charset=utf-8");
    asset!(app, "ui.js", "text/javascript; charset=utf-8");
    asset!(app, "controls.js", "text/javascript; charset=utf-8");
    asset!(app, "graph.js", "text/javascript; charset=utf-8");
    asset!(app, "graph-ui.js", "text/javascript; charset=utf-8");
    asset!(app, "lanes.js", "text/javascript; charset=utf-8");
    asset!(
        app,
        "vendor/preact.min.js",
        "text/javascript; charset=utf-8"
    );
    asset!(app, "vendor/htm.min.js", "text/javascript; charset=utf-8");
    asset!(app, "vendor/flatpickr.min.css", "text/css; charset=utf-8");
    asset!(
        app,
        "vendor/flatpickr.min.js",
        "text/javascript; charset=utf-8"
    );
    asset!(
        app,
        "vendor/cytoscape.min.js",
        "text/javascript; charset=utf-8"
    );
    Ok(app
        .with_state(web.clone())
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(middleware::from_fn_with_state(web, local_only)))
}

async fn local_only(State(w): State<Web>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let origin_ok = req
        .headers()
        .get(header::ORIGIN)
        .is_none_or(|v| v.to_str().is_ok_and(|s| s == format!("http://{host}")));
    let site_ok = !req
        .headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| matches!(v, "cross-site" | "same-site"));
    if (host != w.host && host != w.alternate) || !origin_ok || !site_ok {
        return StatusCode::FORBIDDEN.into_response();
    }
    if req.uri().path() == "/mcp" && req.method() == axum::http::Method::POST {
        let (parts, body) = req.into_parts();
        let Ok(body) = axum::body::to_bytes(body, 128 * 1024).await else {
            // A readable JSON-RPC error: the calling model sees it and can retry with a path.
            let reason = "请求体超过 128 KB。附件的 data_base64 请控制在约 96 KB 以内；更大的文件请改用 path（绝对路径），服务会自己读取。";
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                [(header::CONTENT_TYPE, "application/json")],
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":reason}})
                    .to_string(),
            )
                .into_response();
        };
        // Who is knocking: the handshake carries the client's name, other requests only the
        // User-Agent. This is what lets the connection page confirm a client really arrived.
        let agent = parts
            .headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let name = serde_json::from_slice::<Value>(&body)
            .ok()
            .filter(|v| v["method"] == "initialize")
            .and_then(|v| {
                v["params"]["clientInfo"]["name"]
                    .as_str()
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        w.app.saw_client(&name, agent);
        return headers(next.run(Request::from_parts(parts, Body::from(body))).await);
    }
    headers(next.run(req).await)
}

fn headers(mut r: Response) -> Response {
    let h = r.headers_mut();
    // Pages get the strict default. Attachment responses bring their own, stricter policy.
    h.entry(header::CONTENT_SECURITY_POLICY).or_insert("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; media-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'".parse().unwrap());
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    h.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    h.insert(header::REFERRER_POLICY, "same-origin".parse().unwrap());
    r
}

impl Web {
    pub(crate) fn render(&self, mut model: Value) -> Response {
        model["Base"] = json!(self.base);
        model["Semantic"] = json!(self.store.uses_vectors());
        match self
            .templates
            .get_template("page.html")
            .and_then(|t| t.render(model))
        {
            Ok(body) => Html(body).into_response(),
            Err(error) => {
                crate::log!("Template error: {error:#}");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

pub(crate) fn get_field(f: &Fields, key: &str) -> String {
    f.get(key).map(|s| s.trim().to_owned()).unwrap_or_default()
}
fn number(f: &Fields, key: &str) -> usize {
    get_field(f, key).parse().unwrap_or(0)
}
fn query(f: &Fields, limit: usize) -> Query {
    Query {
        text: get_field(f, "q"),
        from: get_field(f, "from"),
        through: get_field(f, "through"),
        strategy: get_field(f, "strategy"),
        entity: get_field(f, "entity"),
        period: get_field(f, "period"),
        undated: get_field(f, "undated") == "1",
        limit,
        ..Default::default()
    }
}
fn draft(f: &Fields) -> Draft {
    let occurred = get_field(
        f,
        match get_field(f, "precision").as_str() {
            "day" => "occurred_day",
            "month" => "occurred_month",
            "year" => "occurred_year",
            _ => "occurred",
        },
    );
    Draft {
        text: get_field(f, "text"),
        context: get_field(f, "context"),
        occurred,
        ..Default::default()
    }
}
fn qview(q: &Query) -> Value {
    json!({"Text":q.text,"From":q.from,"Through":q.through,"Strategy":q.strategy})
}
fn kind_label(k: RefKind) -> &'static str {
    match k {
        RefKind::Relates => "",
        RefKind::Corrects => "更正",
        RefKind::Summarizes => "汇总",
    }
}
fn kind_of(s: &str) -> RefKind {
    match s {
        "corrects" => RefKind::Corrects,
        "summarizes" => RefKind::Summarizes,
        _ => RefKind::Relates,
    }
}
pub(crate) fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
fn file_kind(mime: &str) -> &'static str {
    if mime.starts_with("image/") && mime != "image/svg+xml" {
        "image"
    } else if mime.starts_with("audio/") {
        "audio"
    } else if mime.starts_with("video/") {
        "video"
    } else if mime == "application/pdf" {
        "pdf"
    } else if mime.starts_with("text/") || mime.contains("json") || mime.contains("xml") {
        "text"
    } else {
        "file"
    }
}
pub(crate) fn file_view(a: &AttachmentView) -> Value {
    json!({"Hash":a.hash,"Name":a.name,"Caption":a.caption,"Mime":a.mime,"Size":human(a.size),
        "Stored":a.storage==Storage::Stored,"Available":a.available,"Path":a.path,"URL":a.url,"Kind":file_kind(&a.mime)})
}
fn entry(e: &Entry) -> Value {
    json!({"ID":e.id,"Text":e.text,"Context":e.context,"Occurred":e.occurred,"Source":e.source,"Recorded":e.recorded,"Revision":e.revision,"Updated":e.updated,"Origin":e.origin})
}
fn trail_label(relation: &str) -> &'static str {
    match relation {
        "corrected_by" => "已被更正",
        "corrects" => "更正了旧记录",
        "summarized_by" => "已被汇总",
        _ => "汇总了其他记录",
    }
}
fn row(e: &Entry, trail: &[Trail], others: &[Entry], period: &str) -> Value {
    let mut view = entry(e);
    view["Period"] = json!(period);
    view["Accounts"] = json!(others.iter().map(entry).collect::<Vec<_>>());
    view["Mentions"] = json!(
        e.mentions
            .iter()
            .take(6)
            .map(|m| json!({"Name":m.name,"ID":m.entity}))
            .collect::<Vec<_>>()
    );
    view["MoreMentions"] = json!(e.mentions.len().saturating_sub(6));
    // Several records of the same kind (say, three summaries) read as one chip with a count.
    let mut chips: Vec<(&str, &str, usize)> = Vec::new();
    for t in trail {
        match chips.iter_mut().find(|c| c.0 == t.relation) {
            Some(chip) => chip.2 += 1,
            None => chips.push((t.relation, t.id.as_str(), 1)),
        }
    }
    view["Trail"] = json!(
        chips
            .iter()
            .map(|(relation, id, count)| {
                let label = trail_label(relation);
                let label = if *count > 1 {
                    format!("{label} ×{count}")
                } else {
                    label.to_owned()
                };
                json!({"Label":label,"ID":id,"Relation":relation})
            })
            .collect::<Vec<_>>()
    );
    view["Files"] = json!(e.attachments.len());
    view["Edited"] = json!(e.revision > 1);
    view
}
fn hit(h: Hit, period: &str, words: &[String]) -> Value {
    let mut view = row(&h.entry, &h.trail, &h.other_accounts, period);
    view["Found"] = json!(crate::store::found_in(&h.entry, words));
    view["Match"] = json!(match h.match_kind {
        "exact" => "原话匹配",
        "strong" => "高度相关",
        "weak" => "弱相关",
        "keyword" => "仅关键词",
        _ => "",
    });
    view["MatchKind"] = json!(h.match_kind);
    view["Relevance"] = json!(h.relevance.map(|r| format!("{r:.2}")));
    view["Replaces"] = json!(h.replaces.len());
    view
}
/// The words of the search the user came from (the `q` inside the back link), to mark them on the memory page.
fn back_terms(back: &str) -> String {
    let query = back.split_once('?').map_or("", |x| x.1);
    let text = url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == "q")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    crate::store::query_words(&text).join(" ")
}
fn local_back(f: &Fields) -> String {
    let back = get_field(f, "back");
    if !back.contains(['\\', '\n', '\r'])
        && (back == "/"
            || back == "/memories"
            || back == "/graph"
            || back == "/entities"
            || back.starts_with("/memories?")
            || back.starts_with("/graph?")
            || back.starts_with("/entities?")
            || back.starts_with("/entity/"))
    {
        back
    } else {
        "/memories".into()
    }
}
fn uri(path: &str, f: &BTreeMap<String, String>) -> String {
    if f.is_empty() {
        return path.into();
    }
    let qs = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(f)
        .finish();
    format!("{path}?{qs}")
}
fn qs(f: &Fields) -> BTreeMap<String, String> {
    f.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}
pub fn public_error(err: &anyhow::Error) -> String {
    let s = err.to_string();
    if s.chars().next().is_some_and(|c| !c.is_ascii()) {
        s
    } else {
        crate::log!("Request failed: {err:#}");
        "操作未完成，请检查输入或重试。".into()
    }
}
/// Where to go after a change. Saving returns to the read-only page; adding an entity or a link keeps the
/// editor open, because that is usually one of several adjustments.
fn mem_location(id: &str, f: &Fields, saved: bool, anchor: &str) -> String {
    let mut args = BTreeMap::from([("back".into(), local_back(f))]);
    if saved {
        args.insert("saved".into(), "1".into());
    }
    let page = if saved { "" } else { "/edit" };
    format!("{}{anchor}", uri(&format!("/memory/{id}{page}"), &args))
}

async fn memories_page(State(w): State<Web>, Params(f): Params<Fields>) -> Response {
    let q = query(&f, 20);
    let n = number(&f, "page").clamp(1, 10000);
    let ordered = get_field(&f, "order") == "date";
    let mut args = qs(&f);
    args.remove("page");
    args.remove("order");
    let by_rank = uri("/memories", &args);
    let graph = uri("/graph", &args);
    args.insert("order".into(), "date".into());
    let by_date = uri("/memories", &args);
    let nav = |page: usize| {
        let mut p = qs(&f);
        p.insert("page".into(), page.to_string());
        uri("/memories", &p)
    };
    let entity_label = if q.entity.is_empty() {
        String::new()
    } else {
        w.store.entity_label(&q.entity).await.unwrap_or_default()
    };
    let mut p = json!({"View":"memories","Query":qview(&q),"Back":uri("/memories",&qs(&f)),"Period":q.period,"Undated":q.undated,"EntityID":q.entity,"EntityName":entity_label,"Chronological":ordered,"ByRank":by_rank,"ByDate":by_date,"GraphURL":graph});
    let list = if q.text.is_empty() {
        w.store.timeline(&q, (n - 1) * q.limit).await
    } else {
        w.store.recall(&q).await
    };
    match list {
        Ok(mut res) => {
            if ordered {
                res.memories.sort_by(|a, b| {
                    (a.entry.occurred.is_empty(), &a.entry.occurred)
                        .cmp(&(b.entry.occurred.is_empty(), &b.entry.occurred))
                });
            }
            p["More"] = json!(res.more);
            p["Degraded"] = json!(res.degraded);
            p["NoStrong"] =
                json!(!q.text.is_empty() && res.no_strong_match && !res.memories.is_empty());
            if q.text.is_empty() {
                if n > 1 {
                    p["Prev"] = json!(nav(n - 1));
                }
                if res.more {
                    p["Next"] = json!(nav(n + 1));
                }
            }
            let mut previous = String::new();
            let words = crate::store::query_words(&q.text);
            p["Terms"] = json!(words.join(" "));
            p["Rows"] = json!(
                res.memories
                    .into_iter()
                    .map(|h| {
                        let mut period = String::new();
                        if q.text.is_empty() || ordered {
                            let date = if h.entry.occurred.is_empty() {
                                "日期未记"
                            } else {
                                h.entry.occurred.get(..7).unwrap_or(&h.entry.occurred)
                            };
                            if date != previous {
                                previous = date.to_owned();
                                period = date.into();
                            }
                        }
                        hit(h, &period, &words)
                    })
                    .collect::<Vec<_>>()
            );
        }
        Err(e) => p["Error"] = json!(public_error(&e)),
    }
    w.render(p)
}

async fn overview(State(w): State<Web>) -> Response {
    let Ok(list) = w.store.all().await else {
        return w.render(json!({"View":"overview","Error":"无法读取记忆"}));
    };
    let mut by_year = BTreeMap::<String, [usize; 13]>::new();
    let mut unknown = 0;
    for e in list.iter() {
        if let Some(year) = e.occurred.get(..4) {
            let counts = by_year.entry(year.into()).or_default();
            counts[0] += 1;
            if let Some(month) = e.occurred.get(5..7).and_then(|m| m.parse::<usize>().ok())
                && (1..=12).contains(&month)
            {
                counts[month] += 1;
            }
        } else {
            unknown += 1;
        }
    }
    let years:Vec<_>=by_year.into_iter().rev().map(|(year,counts)|{
        let max=counts[1..].iter().max().copied().unwrap_or(1).max(1);
        let months=(1..=12).map(|m|json!({"Label":format!("{m:02}"),"Count":counts[m],"Max":max,"URL":format!("/memories?period={year}-{m:02}")})).collect::<Vec<_>>();
        json!({"Year":year,"Count":counts[0],"URL":format!("/memories?period={year}"),"Months":months})
    }).collect();
    let entities = w
        .store
        .entity_page("", "mentions", 0, 1)
        .await
        .map(|p| p.1)
        .unwrap_or(0);
    let health = w.store.health().await.ok();
    let layers: Vec<Value> = health
        .as_ref()
        .map(|h| {
            let top = h.layers.iter().map(|l| l.count).max().unwrap_or(1).max(1);
            let highest = h.layers.last().map(|l| l.layer);
            h.layers
                .iter()
                .rev()
                .map(|l| {
                    json!({"Layer":l.layer,"Count":l.count,"Width":l.count * 100 / top,
                    "Top":Some(l.layer) == highest && l.layer > 0,"Open":l.count - l.covered})
                })
                .collect()
        })
        .unwrap_or_default();
    let issues: Vec<Value> = health
        .map(|h| h.issues)
        .unwrap_or_default()
        .into_iter()
        .map(|i| {
            let first = i.examples.first().cloned().unwrap_or_default();
            let link = match i.kind {
                "unresolved_names" => "/entities#unresolved".to_owned(),
                "duplicate_entities" if !first.is_empty() => format!("/entity/{first}"),
                "missing_files" | "index_gap" => "/data".to_owned(),
                "summary_gaps" | "stale_summaries" => format!("/graph?at={}", first.split(' ').find(|w| w.len() > 3 && *w != "extend").unwrap_or_default()),
                _ if !first.is_empty() => format!("/memory/{first}"),
                _ => String::new(),
            };
            let help = match i.kind {
                "duplicate_memories" => "同一段话存了多份。接入的 AI 会保留信息最全的一条、删掉多余的，不用你批准；你也可以自己在记忆页删除。",
                "duplicate_entities" => "两个同名的实体分不清。AI 会自行合并或给它们各写一句说明；你也可以在实体页「合并到…」。",
                "without_entities" => "这些记忆没有关联任何实体，不影响搜索。AI 会陆续补上，你也可以在记忆页手动添加。",
                "unresolved_names" => "AI 提到了名字却没确定是哪个实体，之后会自己确认；你也可以在实体页选一个。",
                "correction_forks" | "correction_loops" => "几条更正之间互相矛盾，现状没有定论。AI 会判断以哪条为准并汇总；你也可以打开看看。",
                "missing_files" => "附件的原文件找不到了。AI 会去找，找不到就移除这条附件。",
                "summary_gaps" => "汇总树是一层一层长的：第 0 层是记忆本身，每一层都要被上一层汇总。这里是最底下还没汇总的那一层，接入的 AI 会按批写汇总，写完再往上一层，不用你操作。",
                "stale_summaries" => "这些汇总写好之后又并进了不少新记忆，可能说不全了。接入的 AI 会重写或确认，再往上的汇总也会跟着检查。",
                "index_gap" => "有些记忆的搜索索引还没建好，服务会在后台自己修复。",
                _ => "",
            };
            json!({"Detail":i.detail,"Count":i.count,"URL":link,"Kind":i.kind,"Help":help})
        })
        .collect();
    w.render(json!({"View":"overview","Years":years,"Total":list.len(),"Unknown":unknown,"Entities":entities,
        "Calendar":calendar(&list),"Changes":changes(&list),"Layers":layers,"Issues":issues}))
}

/// The last year, one square per day, shaded by how many memories happened that day.
fn calendar(list: &[Entry]) -> Value {
    use chrono::{Datelike, Duration, NaiveDate};
    let today = chrono::Local::now().date_naive();
    let start = today
        - Duration::days(i64::from(today.weekday().num_days_from_monday()))
        - Duration::weeks(52);
    let mut counts = BTreeMap::<NaiveDate, usize>::new();
    for e in list {
        if e.occurred.len() == 10
            && let Ok(day) = NaiveDate::parse_from_str(&e.occurred, "%Y-%m-%d")
            && day >= start
            && day <= today
        {
            *counts.entry(day).or_default() += 1;
        }
    }
    let max = counts.values().copied().max().unwrap_or(0);
    let total: usize = counts.values().sum();
    let mut cells = vec![];
    let mut months = vec![];
    let mut last_month = 0;
    for week in 0..53usize {
        for weekday in 0..7usize {
            let day = start + Duration::days((week * 7 + weekday) as i64);
            if day > today {
                break;
            }
            if weekday == 0 && day.month() != last_month && day.day() <= 7 {
                last_month = day.month();
                months.push(json!({"Label":format!("{} 月",day.month()),"X":30 + week * 14}));
            }
            let n = counts.get(&day).copied().unwrap_or(0);
            let level = if n == 0 {
                0
            } else {
                (n * 4).div_ceil(max.max(1)).clamp(1, 4)
            };
            cells.push(json!({"Date":day.to_string(),"Count":n,"Level":level,"X":30 + week * 14,"Y":20 + weekday * 14}));
        }
    }
    json!({"Cells":cells,"Months":months,"Total":total,"Days":counts.len(),"Max":max})
}

/// What changed lately: corrections and summaries, newest first, with the old and the new text side by side.
fn changes(list: &[Entry]) -> Value {
    let by_id: HashMap<&str, &Entry> = list.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut events: Vec<(&Entry, &Entry, &Reference)> = vec![];
    for e in list {
        for r in &e.references {
            if r.kind != RefKind::Relates
                && let Some(old) = by_id.get(r.id.as_str())
            {
                events.push((e, old, r));
            }
        }
    }
    events.sort_by(|a, b| (&b.0.recorded, &b.0.id).cmp(&(&a.0.recorded, &a.0.id)));
    let total = events.len();
    let items: Vec<Value> = events
        .into_iter()
        .take(12)
        .map(|(new, old, r)| {
            json!({"Kind":if r.kind == RefKind::Corrects {"更正"} else {"汇总"},"Corrects":r.kind == RefKind::Corrects,
                "When":new.recorded,"NewID":new.id,"New":snippet(&new.text,150),"OldID":old.id,"Old":snippet(&old.text,110),
                "Reason":r.reason,"Source":new.source})
        })
        .collect();
    json!({"Items":items,"Total":total})
}

/// A search on the map shows at most this many memories as hits.
const GRAPH_LIMIT: usize = 150;
async fn graph_page(State(w): State<Web>, Params(f): Params<Fields>) -> Response {
    let q = query(&f, GRAPH_LIMIT);
    w.render(json!({"View":"graph","Query":qview(&q),"Period":q.period,"Undated":q.undated,"At":get_field(&f,"at"),"Node":get_field(&f,"node")}))
}
/// The map's data: the entity territories, or the memories inside one territory (`focus`, a page at a time).
async fn graph_data(State(w): State<Web>, Params(f): Params<Fields>) -> Response {
    let focus = get_field(&f, "focus");
    let overview = get_field(&f, "overview") == "1";
    match w.store.graph(&focus, number(&f, "page"), overview).await {
        Ok(g) => Json(g).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, public_error(&e)).into_response(),
    }
}
/// Search on the map: which memories match (by words, meaning or date), and the territory each is drawn in.
async fn graph_hits(State(w): State<Web>, Params(f): Params<Fields>) -> Response {
    let q = Query {
        wide: true,
        ..query(&f, GRAPH_LIMIT)
    };
    let work = async {
        let results = if q.text.is_empty() {
            w.store.timeline(&q, 0).await?
        } else {
            w.store.recall(&q).await?
        };
        let ids: Vec<String> = results.memories.iter().map(|h| h.entry.id.clone()).collect();
        let lands = w.store.lands_of(&ids).await?;
        let hits: Vec<Value> = results
            .memories
            .iter()
            .map(|h| {
                json!({"id":h.entry.id,"text":snippet(&h.entry.text,120),"occurred":h.entry.occurred,"source":h.entry.source,
                    "lands":lands.get(&h.entry.id),"match":h.match_kind,
                    "relevance":h.relevance.map(|r| format!("{r:.2}"))})
            })
            .collect();
        Ok::<_, anyhow::Error>(json!({"hits":hits,"more":results.more,"degraded":results.degraded,"no_strong":results.no_strong_match}))
    }
    .await;
    match work {
        Ok(g) => Json(g).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, public_error(&e)).into_response(),
    }
}
/// The memories for the swim-lane view (the entities come from the overview data).
async fn graph_lanes(State(w): State<Web>) -> Response {
    match w.store.lane_memories(3000).await {
        Ok(list) => Json(list).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, public_error(&e)).into_response(),
    }
}
/// One memory in full, for the floating card on the map (the same facts as the memory page).
async fn peek(State(w): State<Web>, Path(id): Path<String>) -> Response {
    let d = match w.store.detail(&id).await {
        Ok(d) => d,
        Err(err) => return (StatusCode::NOT_FOUND, public_error(&err)).into_response(),
    };
    let link = |l: &Link| json!({"id":l.id,"text":l.text,"kind":l.kind,"label":kind_label(l.kind),"reason":l.reason,"occurred":l.occurred});
    let mentions: Vec<Value> = d
        .mentions
        .iter()
        .map(|m| json!({"name":m.name,"entity":m.entity,"entity_name":m.entity_name,"hint":m.hint}))
        .collect();
    let trail: Vec<Value> = d
        .trail
        .iter()
        .map(|t| json!({"label":trail_label(t.relation),"id":t.id,"source":t.source,"recorded":t.recorded,"reason":t.reason,"text":t.text}))
        .collect();
    let files: Vec<Value> = d
        .attachments
        .iter()
        .map(|a| json!({"name":a.name,"caption":a.caption,"kind":file_kind(&a.mime),"url":a.url,"available":a.available}))
        .collect();
    Json(json!({"id":d.entry.id,"text":d.entry.text,"context":d.entry.context,"occurred":d.entry.occurred,
        "source":d.entry.source,"recorded":d.entry.recorded,"revision":d.entry.revision,
        "edited":d.entry.history.len(),"mentions":mentions,
        "out":d.links_out.iter().map(link).collect::<Vec<_>>(),"in":d.links_in.iter().map(link).collect::<Vec<_>>(),
        "trail":trail,"files":files}))
    .into_response()
}

async fn new_memory(State(w): State<Web>, Params(f): Params<Fields>) -> Response {
    w.render(json!({"View":"detail","Back":local_back(&f),"Entry":entry(&Entry::default())}))
}
/// Reading comes first: the memory page shows the memory; editing is a separate page.
async fn detail(
    State(w): State<Web>,
    Path(id): Path<String>,
    Params(f): Params<Fields>,
) -> Response {
    detail_view(&w, &id, &f, "", None, false).await
}
async fn edit_page(
    State(w): State<Web>,
    Path(id): Path<String>,
    Params(f): Params<Fields>,
) -> Response {
    detail_view(&w, &id, &f, "", None, true).await
}

fn links(list: &[Link]) -> Vec<Value> {
    list.iter()
        .map(|l| json!({"ID":l.id,"Reason":l.reason,"Date":l.occurred,"Text":l.text,"Kind":l.kind,"KindLabel":kind_label(l.kind)}))
        .collect()
}

async fn detail_view(
    w: &Web,
    id: &str,
    f: &Fields,
    error: &str,
    unsaved: Option<Draft>,
    editing: bool,
) -> Response {
    let mut d = match w.store.detail(id).await {
        Ok(d) => d,
        Err(err) => return (StatusCode::NOT_FOUND, public_error(&err)).into_response(),
    };
    if let Some(u) = unsaved {
        d.entry.text = u.text;
        d.entry.context = u.context;
        d.entry.occurred = u.occurred;
    }
    let history: Vec<Value> = d
        .entry
        .history
        .iter()
        .rev()
        .map(|h| json!({"Revision":h.revision,"Text":h.text,"Context":h.context,"Occurred":h.occurred,"At":h.replaced_at,"By":h.replaced_by}))
        .collect();
    let trail: Vec<Value> = d
        .trail
        .iter()
        .map(|t| json!({"Label":trail_label(t.relation),"Relation":t.relation,"ID":t.id,"Source":t.source,"Recorded":t.recorded,"Reason":t.reason,"Text":t.text}))
        .collect();
    let mentions: Vec<Value> = d
        .mentions
        .iter()
        .map(|m| json!({"Name":m.name,"Entity":m.entity,"EntityName":m.entity_name,"Hint":m.hint}))
        .collect();
    let files: Vec<Value> = d.attachments.iter().map(file_view).collect();
    w.render(json!({"View":if editing {"detail"} else {"memory"},"Entry":entry(&d.entry),"Error":error,"Back":local_back(f),"Saved":get_field(f,"saved")=="1",
        "Incoming":links(&d.links_in),"Outgoing":links(&d.links_out),"Mentions":mentions,"Files":files,"History":history,"Trail":trail,
        "Terms":back_terms(&local_back(f))}))
}
async fn create(State(w): State<Web>, Form(f): Form<Fields>) -> Response {
    let d = draft(&f);
    match w.store.remember(d.clone(), "web", "Web").await {
        Ok(saved) => Redirect::to(&mem_location(&saved.entry.id, &f, true, "")).into_response(),
        Err(err) => w.render(json!({"View":"detail","Error":public_error(&err),"Back":local_back(&f),"Entry":entry(&Entry{text:d.text,context:d.context,occurred:d.occurred,..Default::default()})})),
    }
}
async fn edit(State(w): State<Web>, Path(id): Path<String>, Form(f): Form<Fields>) -> Response {
    let d = draft(&f);
    match w
        .store
        .edit(
            &id,
            get_field(&f, "revision").parse().unwrap_or(0),
            d.clone(),
            "Web",
        )
        .await
    {
        Ok(()) => Redirect::to(&mem_location(&id, &f, true, "")).into_response(),
        Err(e) => detail_view(&w, &id, &f, &public_error(&e), Some(d), true).await,
    }
}
async fn delete(State(w): State<Web>, Path(id): Path<String>, Form(f): Form<Fields>) -> Response {
    match w
        .store
        .delete(&id, get_field(&f, "revision").parse().unwrap_or(0))
        .await
    {
        Ok(()) => Redirect::to(&local_back(&f)).into_response(),
        Err(e) => detail_view(&w, &id, &f, &public_error(&e), None, false).await,
    }
}
async fn link(State(w): State<Web>, Path(id): Path<String>, Form(f): Form<Fields>) -> Response {
    let r = Reference {
        id: get_field(&f, "target"),
        reason: get_field(&f, "reason"),
        kind: kind_of(&get_field(&f, "kind")),
    };
    match w
        .store
        .link(&id, r, get_field(&f, "action") == "remove")
        .await
    {
        Ok(()) => Redirect::to(&mem_location(&id, &f, false, "#relations")).into_response(),
        Err(e) => detail_view(&w, &id, &f, &public_error(&e), None, true).await,
    }
}
async fn memory_entities(
    State(w): State<Web>,
    Path(id): Path<String>,
    Form(f): Form<Fields>,
) -> Response {
    let result = if get_field(&f, "action") == "remove" {
        w.store.unbind_entity(&id, &get_field(&f, "key")).await
    } else {
        w.store
            .bind_entity(
                &id,
                EntityInput {
                    name: get_field(&f, "name"),
                    id: get_field(&f, "entity"),
                    hint: get_field(&f, "hint"),
                    new: get_field(&f, "new") == "1",
                    ..Default::default()
                },
                "Web",
            )
            .await
            .map(|_| ())
    };
    match result {
        Ok(()) => Redirect::to(&mem_location(&id, &f, false, "#entities")).into_response(),
        Err(e) => detail_view(&w, &id, &f, &public_error(&e), None, true).await,
    }
}
async fn memory_attachments(
    State(w): State<Web>,
    Path(id): Path<String>,
    Form(f): Form<Fields>,
) -> Response {
    match w.store.unlink_attachment(&id, &get_field(&f, "hash")).await {
        Ok(()) => Redirect::to(&mem_location(&id, &f, false, "#files")).into_response(),
        Err(e) => detail_view(&w, &id, &f, &public_error(&e), None, true).await,
    }
}

// ---- entities

fn entity_json(e: &crate::entities::EntityView) -> Value {
    json!({"ID":e.id,"Name":e.name,"Aliases":e.aliases,"Hint":e.hint,"Mentions":e.mentions,"Last":e.last,"Source":e.source})
}
async fn entities_page(State(w): State<Web>, Params(f): Params<Fields>) -> Response {
    let q = get_field(&f, "q");
    let sort = get_field(&f, "sort");
    let n = number(&f, "page").clamp(1, 10000);
    let per = 40;
    let mut p = json!({"View":"entities","Q":q,"Sort":if sort.is_empty(){"mentions".to_owned()}else{sort.clone()},"Back":"/entities"});
    match w.store.entity_page(&q, &sort, (n - 1) * per, per).await {
        Ok((list, total)) => {
            p["Total"] = json!(total);
            p["Entities"] = json!(list.iter().map(entity_json).collect::<Vec<_>>());
            let args = |page: usize| {
                let mut a = BTreeMap::new();
                if !q.is_empty() {
                    a.insert("q".to_owned(), q.clone());
                }
                if !sort.is_empty() {
                    a.insert("sort".to_owned(), sort.clone());
                }
                a.insert("page".into(), page.to_string());
                uri("/entities", &a)
            };
            if n > 1 {
                p["Prev"] = json!(args(n - 1));
            }
            if n * per < total {
                p["Next"] = json!(args(n + 1));
                p["Left"] = json!(total - n * per);
            }
        }
        Err(e) => p["Error"] = json!(public_error(&e)),
    }
    if q.is_empty()
        && n == 1
        && let Ok(waiting) = w.store.unresolved().await
    {
        p["Unresolved"] = json!(
            waiting
                .into_iter()
                .take(30)
                .map(|(name, ids)| json!({"Name":name,"Count":ids.len(),"Example":ids.first()}))
                .collect::<Vec<_>>()
        );
    }
    w.render(p)
}
async fn entity_view(w: &Web, id: &str, f: &Fields, error: &str) -> Response {
    // Thirty memories at a time; "show more" widens the same page instead of rendering thousands.
    let page = number(f, "page").clamp(1, 200);
    let d = match w.store.entity_detail(id, page * 30).await {
        Ok(d) => d,
        Err(e) => return (StatusCode::NOT_FOUND, public_error(&e)).into_response(),
    };
    let mut previous = String::new();
    let rows: Vec<Value> = d
        .memories
        .iter()
        .map(|e| {
            let date = if e.occurred.is_empty() {
                "日期未记"
            } else {
                e.occurred.get(..7).unwrap_or(&e.occurred)
            };
            let mut period = String::new();
            if date != previous {
                previous = date.to_owned();
                period = date.into();
            }
            row(e, &[], &[], &period)
        })
        .collect();
    let back = uri(&format!("/entity/{}", d.entity.id), &BTreeMap::new());
    w.render(json!({"View":"entity","Error":error,"Entity":entity_json(&d.entity),"Merged":d.merged,
        "Rows":rows,"Related":d.related.iter().map(|(e,n)| json!({"ID":e.id,"Name":e.name,"Hint":e.hint,"Count":n})).collect::<Vec<_>>(),
        "Back":back,"Notice":get_field(f,"done"),
        "MoreCount":d.entity.mentions.saturating_sub(page * 30),"MoreURL":format!("/entity/{}?page={}",d.entity.id,page + 1)}))
}
async fn entity_page(
    State(w): State<Web>,
    Path(id): Path<String>,
    Params(f): Params<Fields>,
) -> Response {
    entity_view(&w, &id, &f, "").await
}
fn lines(text: &str) -> Vec<String> {
    text.split(['\n', ',', '，', '、'])
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}
async fn entity_update(
    State(w): State<Web>,
    Path(id): Path<String>,
    Form(f): Form<Fields>,
) -> Response {
    let name = get_field(&f, "name");
    // The form submits the whole alias list; work out what to add and what to drop.
    let wanted = lines(&get_field(&f, "aliases"));
    let current = match w.store.entity_detail(&id, 0).await {
        Ok(d) => d.entity.aliases,
        Err(e) => return entity_view(&w, &id, &f, &public_error(&e)).await,
    };
    let norm = |s: &str| crate::entities::norm(s);
    let add: Vec<String> = wanted
        .iter()
        .filter(|a| !current.iter().any(|c| norm(c) == norm(a)))
        .cloned()
        .collect();
    let remove: Vec<String> = current
        .iter()
        .filter(|c| !wanted.iter().any(|a| norm(a) == norm(c)))
        .cloned()
        .collect();
    let result = w
        .store
        .entity_update(
            &id,
            (!name.is_empty()).then_some(name),
            add,
            remove,
            f.get("hint").map(|h| h.trim().to_owned()),
        )
        .await;
    match result {
        Ok(_) => Redirect::to(&format!("/entity/{id}?done=saved")).into_response(),
        Err(e) => entity_view(&w, &id, &f, &public_error(&e)).await,
    }
}
async fn entity_merge(
    State(w): State<Web>,
    Path(id): Path<String>,
    Form(f): Form<Fields>,
) -> Response {
    let into = get_field(&f, "into");
    match w.store.entity_merge(&id, &into).await {
        Ok(v) => Redirect::to(&format!("/entity/{}?done=merged", v.id)).into_response(),
        Err(e) => entity_view(&w, &id, &f, &public_error(&e)).await,
    }
}
async fn entity_unmerge(State(w): State<Web>, Path(id): Path<String>) -> Response {
    match w.store.entity_unmerge(&id).await {
        Ok(()) => Redirect::to(&format!("/entity/{id}?done=unmerged")).into_response(),
        Err(e) => entity_view(&w, &id, &Fields::new(), &public_error(&e)).await,
    }
}
async fn entity_delete(State(w): State<Web>, Path(id): Path<String>) -> Response {
    match w.store.entity_delete(&id).await {
        Ok(()) => Redirect::to("/entities?done=deleted").into_response(),
        Err(e) => entity_view(&w, &id, &Fields::new(), &public_error(&e)).await,
    }
}
async fn entity_search(State(w): State<Web>, Params(f): Params<Fields>) -> Response {
    match w.store.entity_search(&get_field(&f, "q"), 20).await {
        Ok(list) => Json(json!({"entities":list.iter().map(entity_json).collect::<Vec<_>>()}))
            .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, public_error(&e)).into_response(),
    }
}
