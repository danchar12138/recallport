use crate::{
    app::App, detail::Link, files::AttachmentView, jobs::Jobs, mcp::Mcp, memory::*, store::Store,
    web_admin,
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
        "timeline",
        "rows",
        "overview",
        "editor",
        "relations",
        "date-control",
        "graph-page",
        "graph-canvas",
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
    let mcp = StreamableHttpService::new(
        move || Ok(Mcp::new(store.clone(), app.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let app = Router::new()
        .route_service("/mcp", mcp)
        .route("/health", get(|| async { "ok\n" }))
        .route("/", get(timeline))
        .route("/overview", get(overview))
        .route("/graph", get(graph_page))
        .route("/graph-data", get(graph_data))
        .route("/memory/new", get(new_memory).post(create))
        .route("/memory/{id}", get(detail).post(edit))
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
            get(|State(w): State<Web>| async move { w.render(json!({"View":"clients"})) }),
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
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        };
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
    json!({"ID":e.id,"Text":e.text,"Context":e.context,"Occurred":e.occurred,"Source":e.source,"Recorded":e.recorded,"Revision":e.revision,"Updated":e.updated})
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
    view["Trail"] = json!(
        trail
            .iter()
            .map(|t| json!({"Label":trail_label(t.relation),"ID":t.id,"Relation":t.relation}))
            .collect::<Vec<_>>()
    );
    view["Files"] = json!(e.attachments.len());
    view["Edited"] = json!(e.revision > 1);
    view
}
fn hit(h: Hit, period: &str) -> Value {
    row(&h.entry, &h.trail, &h.other_accounts, period)
}
fn local_back(f: &Fields) -> String {
    let back = get_field(f, "back");
    if !back.contains(['\\', '\n', '\r'])
        && (back == "/"
            || back == "/graph"
            || back == "/entities"
            || back.starts_with("/?")
            || back.starts_with("/graph?")
            || back.starts_with("/entities?")
            || back.starts_with("/entity/"))
    {
        back
    } else {
        "/".into()
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
fn mem_location(id: &str, f: &Fields, saved: bool, anchor: &str) -> String {
    let mut args = BTreeMap::from([("back".into(), local_back(f))]);
    if saved {
        args.insert("saved".into(), "1".into());
    }
    format!("{}{anchor}", uri(&format!("/memory/{id}"), &args))
}

async fn timeline(State(w): State<Web>, Params(f): Params<Fields>) -> Response {
    let q = query(&f, 20);
    let n = number(&f, "page").clamp(1, 10000);
    let ordered = get_field(&f, "order") == "date";
    let mut args = qs(&f);
    args.remove("page");
    args.remove("order");
    let by_rank = uri("/", &args);
    let graph = uri("/graph", &args);
    args.insert("order".into(), "date".into());
    let by_date = uri("/", &args);
    let nav = |page: usize| {
        let mut p = qs(&f);
        p.insert("page".into(), page.to_string());
        uri("/", &p)
    };
    let entity_label = if q.entity.is_empty() {
        String::new()
    } else {
        w.store.entity_label(&q.entity).await.unwrap_or_default()
    };
    let mut p = json!({"View":"timeline","Query":qview(&q),"Back":uri("/",&qs(&f)),"Period":q.period,"Undated":q.undated,"EntityID":q.entity,"EntityName":entity_label,"Chronological":ordered,"ByRank":by_rank,"ByDate":by_date,"GraphURL":graph});
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
            if q.text.is_empty() {
                if n > 1 {
                    p["Prev"] = json!(nav(n - 1));
                }
                if res.more {
                    p["Next"] = json!(nav(n + 1));
                }
            }
            let mut previous = String::new();
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
                        hit(h, &period)
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
        let months=(1..=12).map(|m|json!({"Label":format!("{m:02}"),"Count":counts[m],"Max":max,"URL":format!("/?period={year}-{m:02}")})).collect::<Vec<_>>();
        json!({"Year":year,"Count":counts[0],"URL":format!("/?period={year}"),"Months":months})
    }).collect();
    let entities = w
        .store
        .entity_page("", "mentions", 0, 1)
        .await
        .map(|p| p.1)
        .unwrap_or(0);
    w.render(json!({"View":"overview","Years":years,"Total":list.len(),"Unknown":unknown,"Entities":entities}))
}

async fn graph_page(State(w): State<Web>, Params(f): Params<Fields>) -> Response {
    let q = query(&f, 30);
    w.render(json!({"View":"graph","Query":qview(&q),"Period":q.period,"Undated":q.undated,"Overview":get_field(&f,"view")=="entities"}))
}
async fn graph_data(State(w): State<Web>, Params(f): Params<Fields>) -> Response {
    let q = query(&f, 30);
    let focus = get_field(&f, "focus");
    let overview = get_field(&f, "overview") == "1";
    let n = if q.text.is_empty() {
        number(&f, "page").clamp(1, 10000)
    } else {
        1
    };
    let work = async {
        let mut ids = vec![];
        let mut more = false;
        let mut degraded = false;
        if focus.is_empty() && !overview {
            let results = if q.text.is_empty() {
                w.store.timeline(&q, (n - 1) * q.limit).await?
            } else {
                w.store.recall(&q).await?
            };
            ids = results.memories.into_iter().map(|h| h.entry.id).collect();
            more = results.more;
            degraded = results.degraded;
        }
        let mut g = w
            .store
            .graph(&ids, &focus, get_field(&f, "suggested") == "1", overview)
            .await?;
        let paged = focus.is_empty() && !overview;
        g["page"] = json!(n);
        g["next"] = json!(paged && q.text.is_empty() && more);
        g["limited"] = json!(paged && !q.text.is_empty() && more);
        g["degraded"] = json!(degraded);
        Ok::<_, anyhow::Error>(g)
    }
    .await;
    match work {
        Ok(g) => Json(g).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, public_error(&e)).into_response(),
    }
}

async fn new_memory(State(w): State<Web>, Params(f): Params<Fields>) -> Response {
    w.render(json!({"View":"detail","Back":local_back(&f),"Entry":entry(&Entry::default())}))
}
async fn detail(
    State(w): State<Web>,
    Path(id): Path<String>,
    Params(f): Params<Fields>,
) -> Response {
    detail_view(&w, &id, &f, "", None).await
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
    w.render(json!({"View":"detail","Entry":entry(&d.entry),"Error":error,"Back":local_back(f),"Saved":get_field(f,"saved")=="1",
        "Incoming":links(&d.links_in),"Outgoing":links(&d.links_out),"Mentions":mentions,"Files":files,"History":history,"Trail":trail}))
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
        Err(e) => detail_view(&w, &id, &f, &public_error(&e), Some(d)).await,
    }
}
async fn delete(State(w): State<Web>, Path(id): Path<String>, Form(f): Form<Fields>) -> Response {
    match w
        .store
        .delete(&id, get_field(&f, "revision").parse().unwrap_or(0))
        .await
    {
        Ok(()) => Redirect::to(&local_back(&f)).into_response(),
        Err(e) => detail_view(&w, &id, &f, &public_error(&e), None).await,
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
        Err(e) => detail_view(&w, &id, &f, &public_error(&e), None).await,
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
        Err(e) => detail_view(&w, &id, &f, &public_error(&e), None).await,
    }
}
async fn memory_attachments(
    State(w): State<Web>,
    Path(id): Path<String>,
    Form(f): Form<Fields>,
) -> Response {
    match w.store.unlink_attachment(&id, &get_field(&f, "hash")).await {
        Ok(()) => Redirect::to(&mem_location(&id, &f, false, "#files")).into_response(),
        Err(e) => detail_view(&w, &id, &f, &public_error(&e), None).await,
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
    let d = match w.store.entity_detail(id, 120).await {
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
        "Back":back,"Notice":get_field(f,"done")}))
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
