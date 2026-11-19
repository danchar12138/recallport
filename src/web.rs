use crate::{
    mcp::Mcp,
    memory::{Draft, Entry, Hit, Query, Reference, snippet},
    store::{self, Backup, IndexStatus, LaneKind, Store},
};
use anyhow::Result;
use axum::{
    Form, Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, Query as Params, Request, State},
    http::{HeaderMap, StatusCode, header},
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
struct Web {
    store: Arc<Store>,
    base: String,
    host: String,
    alternate: String,
    templates: Arc<Environment<'static>>,
}
type Fields = HashMap<String, String>;

pub fn router(store: Arc<Store>, address: SocketAddr) -> Result<Router> {
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
        "overview",
        "editor",
        "relations",
        "date-control",
        "graph-page",
        "graph-canvas",
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
    };
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts([web.host.clone(), web.alternate.clone()])
        .with_allowed_origins([web.base.clone(), format!("http://{}", web.alternate)]);
    let mcp = StreamableHttpService::new(
        move || Ok(Mcp::new(store.clone())),
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
        .route(
            "/clients",
            get(|State(w): State<Web>| async move { w.render(json!({"View":"clients"})) }),
        )
        .route("/data", get(data))
        .route(
            "/index-status",
            get(|State(w): State<Web>| async move { Json(w.store.status.read().await.clone()) }),
        )
        .route("/backup", post(backup))
        .route(
            "/restore",
            post(restore).layer(DefaultBodyLimit::max(store::MAX_BACKUP + 64 * 1024)),
        )
        .route("/recovery", get(recovery).post(rollback))
        .route("/reindex", post(reindex));
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
    h.insert(header::CONTENT_SECURITY_POLICY,"default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src data:; form-action 'self'; frame-ancestors 'none'; base-uri 'none'".parse().unwrap());
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    h.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    h.insert(header::REFERRER_POLICY, "same-origin".parse().unwrap());
    r
}

impl Web {
    fn render(&self, mut model: Value) -> Response {
        model["Base"] = json!(self.base);
        model["Semantic"] = json!(self.store.uses_vectors());
        match self
            .templates
            .get_template("page.html")
            .and_then(|t| t.render(model))
        {
            Ok(body) => Html(body).into_response(),
            Err(error) => {
                eprintln!("Template error: {error:#}");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

fn get_field(f: &Fields, key: &str) -> String {
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
fn entry(e: &Entry) -> Value {
    json!({"ID":e.id,"Text":e.text,"Context":e.context,"Occurred":e.occurred,"Source":e.source,"Recorded":e.recorded,"Revision":e.revision})
}
fn hit(h: Hit, period: &str) -> Value {
    let mut view = entry(&h.entry);
    view["Period"] = json!(period);
    view["Accounts"] = json!(h.other_accounts.iter().map(entry).collect::<Vec<_>>());
    view
}
fn local_back(f: &Fields) -> String {
    let back = get_field(f, "back");
    if !back.contains(['\\', '\n', '\r'])
        && (back == "/"
            || back == "/graph"
            || back.starts_with("/?")
            || back.starts_with("/graph?"))
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
        eprintln!("Request failed: {err:#}");
        "操作未完成，请检查输入或重试。".into()
    }
}
fn mem_location(id: &str, f: &Fields, saved: bool) -> String {
    let mut args = BTreeMap::from([("back".into(), local_back(f))]);
    if saved {
        args.insert("saved".into(), "1".into());
    }
    uri(&format!("/memory/{id}"), &args)
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
    let mut p = json!({"View":"timeline","Query":qview(&q),"Back":uri("/",&qs(&f)),"Period":q.period,"Undated":q.undated,"Chronological":ordered,"ByRank":by_rank,"ByDate":by_date,"GraphURL":graph});
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
    for e in &list {
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
    w.render(json!({"View":"overview","Years":years,"Total":list.len(),"Unknown":unknown}))
}

async fn graph_page(State(w): State<Web>, Params(f): Params<Fields>) -> Response {
    let q = query(&f, 30);
    w.render(json!({"View":"graph","Query":qview(&q),"Period":q.period,"Undated":q.undated}))
}
async fn graph_data(State(w): State<Web>, Params(f): Params<Fields>) -> Response {
    let q = query(&f, 30);
    let focus = get_field(&f, "focus");
    let n = if q.text.is_empty() {
        number(&f, "page").clamp(1, 10000)
    } else {
        1
    };
    let work = async {
        let mut ids = vec![];
        let mut more = false;
        let mut degraded = false;
        if focus.is_empty() {
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
            .graph(&ids, &focus, get_field(&f, "suggested") == "1")
            .await?;
        g["page"] = json!(n);
        g["next"] = json!(focus.is_empty() && q.text.is_empty() && more);
        g["limited"] = json!(!q.text.is_empty() && more);
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
async fn detail_view(
    w: &Web,
    id: &str,
    f: &Fields,
    error: &str,
    unsaved: Option<Draft>,
) -> Response {
    let mut e = match w.store.read(id).await {
        Ok(e) => e,
        Err(err) => return (StatusCode::NOT_FOUND, public_error(&err)).into_response(),
    };
    if let Some(d) = unsaved {
        e.text = d.text;
        e.context = d.context;
        e.occurred = d.occurred;
    }
    let find = get_field(f, "find");
    let options = w
        .store
        .recall(&Query {
            text: find.clone(),
            limit: 12,
            ..Default::default()
        })
        .await
        .ok()
        .map(|r| {
            r.memories
                .into_iter()
                .filter(|h| h.entry.id != id)
                .map(|h| hit(h, ""))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let outgoing = links(w, &e.references).await;
    let incoming = links(w, &e.cited_by).await;
    w.render(json!({"View":"detail","Entry":entry(&e),"Error":error,"Back":local_back(f),"Saved":get_field(f,"saved")=="1","LinkQuery":find,"Incoming":incoming,"Outgoing":outgoing,"Choices":options}))
}
async fn links(w: &Web, input: &[Reference]) -> Vec<Value> {
    let mut result = vec![];
    for r in input {
        if let Ok(e) = w.store.read(&r.id).await {
            result.push(
                json!({"ID":r.id,"Reason":r.reason,"Date":e.occurred,"Text":snippet(&e.text,100)}),
            );
        }
    }
    result
}
async fn create(State(w): State<Web>, Form(f): Form<Fields>) -> Response {
    let d = draft(&f);
    match w.store.remember(d.clone(),"web","Web").await {
        Ok(e)=>Redirect::to(&mem_location(&e.id,&f,true)).into_response(),
        Err(err)=>w.render(json!({"View":"detail","Error":public_error(&err),"Back":local_back(&f),"Entry":entry(&Entry{text:d.text,context:d.context,occurred:d.occurred,..Default::default()})})),
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
        )
        .await
    {
        Ok(()) => Redirect::to(&mem_location(&id, &f, true)).into_response(),
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
    };
    match w
        .store
        .link(&id, r, get_field(&f, "action") == "remove")
        .await
    {
        Ok(()) => {
            Redirect::to(&format!("{}#relations", mem_location(&id, &f, false))).into_response()
        }
        Err(e) => detail_view(&w, &id, &f, &public_error(&e), None).await,
    }
}

fn index_display(s: &IndexStatus) -> Value {
    fn lane(l: &store::Lane) -> Value {
        json!({"State":l.state,"CheckedAt":l.checked_at,"RepairedAt":l.repaired_at,"Problem":l.problem})
    }
    json!({"Keyword":lane(&s.keyword),"Semantic":lane(&s.semantic),"Model":s.model,"Total":s.total,"Indexed":s.indexed,"Skipped":s.skipped,"Dimensions":s.dimensions,"Ann":s.ann,"AnnIndexed":s.ann_indexed,"KeywordIndexed":s.keyword_indexed})
}
fn configs(s: &IndexStatus) -> Value {
    let row =
        |label: &str, v: String, meaning: &str| json!({"Label":label,"Value":v,"Meaning":meaning});
    json!([
        {"Title":"关键词候选","Rows":[row("索引与分词","LanceDB BM25 · Jieba + 双字".into(),"原文精确匹配优先；路径搜索保留原文。"),row("查询分词上限","36".into(),"限制关键词查询的候选词数量。")]},
        {"Title":"语义编码与检索","Rows":[row("模型",s.model.into(),"官方 Granite 97M Multilingual R2，FP32，CLS 池化与 L2 归一化。"),row("推理",format!("{} tokens / {} 线程 / {} 维",crate::embedding::TOKENS,crate::embedding::THREADS,s.dimensions),"按整条和片段分别编码。"),row("切片",format!("整条 + 最多 {} 段；片段 ≥ {} 字",store::MAX_PASSAGES-1,store::MIN_PASSAGE),"使用最相关的片段相似度作为记忆得分。"),row("候选与建议门槛",format!("{:.2} / {:.2}",store::SEMANTIC,store::GRAPH),"余弦相似度；不是置信度或两路占比。"),row("向量索引",format!("IVF-HNSW-Flat · ≥ {} 条",store::ANN_MIN),"数据较少时精确扫描；ANN 保持浮点向量，不量化。"),row("探测分区 / 重排倍数",format!("{} / 3",store::ANN_PROBES),"日期约束在候选召回前应用。") ]},
        {"Title":"混合排序","Rows":[row("排名融合",format!("偏移 {} / 关键词 {} / 语义 {}",store::RRF,store::LEXICAL_WEIGHT,store::SEMANTIC_WEIGHT),"关键词与语义独立入选，同一条记忆两路命中时分数相加。"),row("候选上限",format!("每路 {} 条",store::CANDIDATES),"候选去重后返回；MCP 默认 8 条、最多 30 条。") ]},
        {"Title":"自动维护","Rows":[row("检查频率","60 秒".into(),"补齐缺失向量并维护派生索引，不修改正文与引用。") ]}
    ])
}
async fn data(State(w): State<Web>, Params(f): Params<Fields>) -> Response {
    let notice = match get_field(&f, "done").as_str() {
        "index" => "已提交索引重建。",
        "restore" => "备份已恢复。",
        _ => "",
    };
    data_view(&w, "", notice).await
}
async fn data_view(w: &Web, error: &str, notice: &str) -> Response {
    let s = w.store.status.read().await.clone();
    w.render(json!({"View":"data","Error":error,"Notice":notice,"Recovery":if w.store.recovery().is_file(){"recovery.recallport.json"}else{""}, "Index":index_display(&s),"SearchConfig":configs(&s)}))
}
fn download(bytes: Vec<u8>, name: &str) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/json".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        bytes,
    )
        .into_response()
}
async fn backup(State(w): State<Web>) -> Response {
    match w
        .store
        .backup()
        .await
        .and_then(|b| Ok(serde_json::to_vec(&b)?))
    {
        Ok(bytes) => download(
            bytes,
            &format!(
                "recallport-{}.recallport.json",
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            ),
        ),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, public_error(&e)).into_response(),
    }
}
async fn restore(State(w): State<Web>, mut multipart: Multipart) -> Response {
    let work = async {
        let (mut confirmation, mut bytes) = (false, None);
        while let Some(field) = multipart.next_field().await? {
            match field.name() {
                Some("confirm") => confirmation = field.text().await? == "yes",
                Some("backup") => bytes = Some(field.bytes().await?),
                _ => {}
            }
        }
        anyhow::ensure!(confirmation, "请确认替换当前记忆");
        let b = Backup::parse(&bytes.ok_or_else(|| anyhow::anyhow!("请选择备份文件"))?)?;
        w.store.replace(b, true).await?;
        w.store.maintain(Some(LaneKind::All)).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    match work {
        Ok(()) => Redirect::to("/data?done=restore").into_response(),
        Err(e) => data_view(&w, &public_error(&e), "").await,
    }
}
async fn recovery(State(w): State<Web>) -> Response {
    match std::fs::read(w.store.recovery()) {
        Ok(bytes) => download(bytes, "recovery.recallport.json"),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn rollback(State(w): State<Web>, Form(f): Form<Fields>) -> Response {
    let work = async {
        anyhow::ensure!(
            get_field(&f, "confirm") == "yes"
                && get_field(&f, "snapshot") == "recovery.recallport.json",
            "请确认回退"
        );
        let backup = Backup::parse(&std::fs::read(w.store.recovery())?)?;
        w.store.replace(backup, true).await?;
        w.store.maintain(Some(LaneKind::All)).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    match work {
        Ok(()) => Redirect::to("/data?done=restore").into_response(),
        Err(e) => data_view(&w, &public_error(&e), "").await,
    }
}
async fn reindex(State(w): State<Web>, headers: HeaderMap, Form(f): Form<Fields>) -> Response {
    let kind = match get_field(&f, "kind").as_str() {
        "" | "all" => LaneKind::All,
        "keyword" => LaneKind::Keyword,
        "semantic" => LaneKind::Semantic,
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    if kind == LaneKind::Semantic && !w.store.uses_vectors() {
        return StatusCode::CONFLICT.into_response();
    }
    if !w.store.queue_rebuild(kind).await {
        return StatusCode::CONFLICT.into_response();
    }
    if headers
        .get(header::ACCEPT)
        .is_some_and(|v| v == "application/json")
    {
        StatusCode::ACCEPTED.into_response()
    } else {
        Redirect::to("/data?done=index").into_response()
    }
}
