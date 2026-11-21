//! Data management over HTTP: index status, backup, restore, merge, rollback, attachments.
//! Long operations are background jobs; the page polls `/jobs/{id}` and shows progress.
use crate::{
    clients::{self, Client, Outcome},
    memory::{Storage, valid_hash},
    shell,
    store::{self, IndexStatus, LaneKind},
    web::{Fields, Web, get_field, human, public_error},
};
use axum::{
    Form, Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, Query as Params, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;
use tower_http::services::ServeFile;

pub(crate) fn routes() -> Router<Web> {
    Router::new()
        .route("/data", get(data))
        .route(
            "/index-status",
            get(|State(w): State<Web>| async move { Json(w.store.status.read().await.clone()) }),
        )
        .route("/reindex", post(reindex))
        .route("/backup", post(backup_start))
        .route("/jobs/{id}", get(job_state))
        .route("/download/{id}", get(download))
        .route(
            "/restore",
            post(restore_upload).layer(DefaultBodyLimit::disable()),
        )
        .route("/recovery/export", post(recovery_export))
        .route("/recovery/rollback", post(recovery_rollback))
        .route("/settings/autostart", post(set_autostart))
        .route("/settings/auto-backup", post(set_auto_backup))
        .route("/backups/{name}", get(auto_backup_file))
        .route("/backups/{name}/import", post(auto_backup_import))
        .route("/settings/quit", post(quit))
        .route("/clients-status", get(clients_status))
        .route("/clients/{id}/connect", post(client_connect))
        .route("/clients/{id}/disconnect", post(client_disconnect))
        .route("/files/cleanup", post(cleanup))
        .route("/files/{hash}", get(file))
        .route("/files/{hash}/text", get(file_text))
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
        {"Title":"关键词候选","Rows":[row("索引与分词","LanceDB BM25 · Jieba + 双字".into(),"原文精确匹配优先；路径搜索保留原文；实体名称和附件说明也参与关键词搜索。"),row("查询分词上限","36".into(),"限制关键词查询的候选词数量。")]},
        {"Title":"语义编码与搜索","Rows":[row("模型",s.model.into(),"官方 Granite 97M Multilingual R2，FP32，CLS 池化与 L2 归一化。"),row("推理",format!("{} tokens / {} 线程 / {} 维",crate::embedding::TOKENS,crate::embedding::THREADS,s.dimensions),"按整条和片段分别编码。"),row("切片",format!("整条 + 最多 {} 段；片段 ≥ {} 字",store::MAX_PASSAGES-1,store::MIN_PASSAGE),"使用最相关的片段相似度作为记忆得分。"),row("候选与建议门槛",format!("{:.2} / {:.2}",store::SEMANTIC,store::GRAPH),"余弦相似度；不是置信度或两路占比。"),row("向量索引",format!("IVF-HNSW-Flat · ≥ {} 条",store::ANN_MIN),"数据较少时精确扫描；ANN 保持浮点向量，不量化。"),row("探测分区 / 重排倍数",format!("{} / 3",store::ANN_PROBES),"日期约束在候选召回前应用。") ]},
        {"Title":"混合排序","Rows":[row("排名融合",format!("偏移 {} / 关键词 {} / 语义 {}",store::RRF,store::LEXICAL_WEIGHT,store::SEMANTIC_WEIGHT),"关键词与语义独立入选，同一条记忆两路命中时分数相加。"),row("候选上限",format!("每路 {} 条",store::CANDIDATES),"候选去重后返回；MCP 默认 8 条、最多 30 条。") ]},
        {"Title":"自动维护","Rows":[row("检查频率","60 秒".into(),"补齐缺失向量并维护派生索引，不修改正文与引用。") ]}
    ])
}

pub(crate) async fn data_model(w: &Web) -> Value {
    let s = w.store.status.read().await.clone();
    let files = w.store.file_stats().await;
    let recovery = w
        .store
        .recovery_info()
        .map(|r| json!({"Memories":r.memories,"Entities":r.entities,"Attachments":r.attachments}));
    let auto = w.store.auto_backup_info();
    let mut model = json!({"View":"data","Index":index_display(&s),"SearchConfig":configs(&s),"Recovery":recovery,
        "Auto":{"Enabled":auto.enabled,"Keep":crate::autobackup::KEEP,"Files":auto.files.iter().map(|f| json!({"Name":f.name,"Size":human(f.bytes),"At":f.at})).collect::<Vec<_>>()},
        "Files":{"Stored":files.stored,"Referenced":files.referenced,"Size":human(files.bytes)},
        "Total":s.total});
    if shell::SUPPORTED {
        model["Settings"] = json!({"Autostart": shell::autostart_enabled()});
    }
    model
}

async fn data(State(w): State<Web>) -> Response {
    let model = data_model(&w).await;
    w.render(model)
}

fn busy() -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"error":"另一项耗时操作正在进行，请等它完成后再试。"})),
    )
        .into_response()
}

fn accepted(id: String) -> Response {
    (StatusCode::ACCEPTED, Json(json!({"id":id}))).into_response()
}

async fn backup_start(State(w): State<Web>) -> Response {
    let Some(job) = w.jobs.start("export") else {
        return busy();
    };
    let dir = w.store.export_dir();
    if let Ok(old) = std::fs::read_dir(&dir) {
        for item in old.flatten() {
            let _ = std::fs::remove_file(item.path());
        }
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        job.fail(public_error(&e.into()));
        return accepted(job.snapshot().id);
    }
    let name = format!(
        "recallport-{}.recallport.zip",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let (store, id, j, dest) = (
        w.store.clone(),
        job.snapshot().id,
        job.clone(),
        dir.join(&name),
    );
    tokio::spawn(async move {
        match store.export_to(dest, j.clone()).await {
            Ok(info) => j.succeed(json!({"file":name,"size":human(info.bytes),"info":info})),
            Err(e) => j.fail(public_error(&e)),
        }
    });
    accepted(id)
}

async fn recovery_export(State(w): State<Web>) -> Response {
    let Some(job) = w.jobs.start("export") else {
        return busy();
    };
    let dir = w.store.export_dir();
    let _ = std::fs::create_dir_all(&dir);
    let name = format!(
        "recallport-rollback-{}.recallport.zip",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let (store, id, j, dest) = (
        w.store.clone(),
        job.snapshot().id,
        job.clone(),
        dir.join(&name),
    );
    tokio::spawn(async move {
        match store.export_recovery_to(dest, j.clone()).await {
            Ok(info) => j.succeed(json!({"file":name,"size":human(info.bytes),"info":info})),
            Err(e) => j.fail(public_error(&e)),
        }
    });
    accepted(id)
}

async fn recovery_rollback(State(w): State<Web>) -> Response {
    let Some(job) = w.jobs.start("rollback") else {
        return busy();
    };
    let (store, id, j) = (w.store.clone(), job.snapshot().id, job.clone());
    tokio::spawn(async move {
        match store.rollback(j.clone()).await {
            Ok(info) => {
                j.phase("重建搜索索引", 0.99, 1.0);
                let _ = store.maintain(Some(LaneKind::All)).await;
                j.succeed(json!({"mode":"rollback","info":info}));
            }
            Err(e) => j.fail(public_error(&e)),
        }
    });
    accepted(id)
}

async fn job_state(State(w): State<Web>, Path(id): Path<String>) -> Response {
    match w.jobs.get(&id) {
        Some(job) => Json(job.snapshot()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn disposition(kind: &str, name: &str) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(name.as_bytes()).collect();
    format!("{kind}; filename*=UTF-8''{}", encoded.replace('+', "%20"))
}

async fn serve(
    path: PathBuf,
    req: Request,
    mime: &str,
    content_disposition: String,
    policy: Option<&'static str>,
) -> Response {
    match ServeFile::new(path).oneshot(req).await {
        Ok(res) => {
            let mut res = res.map(Body::new);
            let h = res.headers_mut();
            if let Ok(v) = mime.parse() {
                h.insert(header::CONTENT_TYPE, v);
            }
            if let Ok(v) = content_disposition.parse() {
                h.insert(header::CONTENT_DISPOSITION, v);
            }
            if let Some(p) = policy {
                h.insert(header::CONTENT_SECURITY_POLICY, p.parse().unwrap());
            }
            res.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn download(State(w): State<Web>, Path(id): Path<String>, req: Request) -> Response {
    let Some(state) = w.jobs.get(&id).map(|j| j.snapshot()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let file = state
        .result
        .as_ref()
        .and_then(|r| r["file"].as_str())
        .map(str::to_owned);
    let Some(name) = file.filter(|_| state.kind == "export" && state.error.is_none()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Only the bare file name from our own job result is ever joined to the export directory.
    let Some(bare) = std::path::Path::new(&name).file_name() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = w.store.export_dir().join(bare);
    if !path.is_file() {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve(
        path,
        req,
        "application/zip",
        disposition("attachment", &name),
        None,
    )
    .await
}

async fn restore_upload(State(w): State<Web>, mut multipart: Multipart) -> Response {
    if w.jobs.busy() {
        return busy();
    }
    let dir = w.store.export_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return (StatusCode::INTERNAL_SERVER_ERROR, public_error(&e.into())).into_response();
    }
    let path = dir.join(format!("upload-{}.zip", uuid::Uuid::new_v4().simple()));
    let mut mode = String::from("restore");
    let mut received = false;
    let outcome: anyhow::Result<()> = async {
        while let Some(mut field) = multipart.next_field().await? {
            match field.name() {
                Some("mode") => mode = field.text().await?,
                Some("backup") => {
                    let mut out = tokio::fs::File::create(&path).await?;
                    let mut size = 0u64;
                    while let Some(chunk) = field.chunk().await? {
                        size += chunk.len() as u64;
                        anyhow::ensure!(size <= crate::archive::MAX_ARCHIVE, "备份文件过大");
                        out.write_all(&chunk).await?;
                    }
                    out.flush().await?;
                    received = size > 0;
                }
                _ => {}
            }
        }
        anyhow::ensure!(received, "请选择备份文件");
        Ok(())
    }
    .await;
    if let Err(e) = outcome {
        let _ = std::fs::remove_file(&path);
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":public_error(&e)})),
        )
            .into_response();
    }
    start_import(&w, path, mode == "merge", true)
}

/// Run an import or merge of the ZIP at `path` as a background job. An uploaded copy is deleted afterwards;
/// an automatic backup stays where it is.
fn start_import(w: &Web, path: PathBuf, merge: bool, temporary: bool) -> Response {
    let Some(job) = w.jobs.start(if merge { "merge" } else { "restore" }) else {
        if temporary {
            let _ = std::fs::remove_file(&path);
        }
        return busy();
    };
    let (store, id, j) = (w.store.clone(), job.snapshot().id, job.clone());
    tokio::spawn(async move {
        let result = if merge {
            store
                .merge_zip(path.clone(), j.clone())
                .await
                .map(|r| json!({"mode":"merge","report":r}))
        } else {
            store
                .restore_zip(path.clone(), j.clone())
                .await
                .map(|i| json!({"mode":"restore","info":i}))
        };
        if result.is_ok() {
            j.phase("重建搜索索引", 0.99, 1.0);
            let _ = store.maintain(Some(LaneKind::All)).await;
        }
        if temporary {
            let _ = std::fs::remove_file(&path);
        }
        match result {
            Ok(v) => j.succeed(v),
            Err(e) => j.fail(public_error(&e)),
        }
    });
    accepted(id)
}

async fn auto_backup_file(
    State(w): State<Web>,
    Path(name): Path<String>,
    req: Request,
) -> Response {
    match w.store.backup_path(&name) {
        Some(path) => {
            serve(
                path,
                req,
                "application/zip",
                disposition("attachment", &name),
                None,
            )
            .await
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn auto_backup_import(
    State(w): State<Web>,
    Path(name): Path<String>,
    Form(f): Form<Fields>,
) -> Response {
    match w.store.backup_path(&name) {
        Some(path) => start_import(&w, path, get_field(&f, "mode") == "merge", false),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn set_auto_backup(State(w): State<Web>, Form(f): Form<Fields>) -> Response {
    match w.store.set_auto_backup(get_field(&f, "enabled") == "1") {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, public_error(&e)).into_response(),
    }
}

async fn cleanup(State(w): State<Web>) -> Response {
    match w.store.gc_files().await {
        Ok((files, bytes)) => Json(json!({"files":files,"size":human(bytes)})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, public_error(&e)).into_response(),
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
        Redirect::to("/data").into_response()
    }
}

// ---- attachments

/// Types a browser can show without running anything.
fn inline_safe(mime: &str) -> bool {
    matches!(
        mime,
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/avif"
            | "audio/mpeg"
            | "audio/wav"
            | "audio/x-wav"
            | "audio/ogg"
            | "audio/mp4"
            | "audio/aac"
            | "audio/flac"
            | "audio/webm"
            | "video/mp4"
            | "video/webm"
            | "video/ogg"
            | "application/pdf"
            | "text/plain"
    )
}

async fn file(
    State(w): State<Web>,
    Path(hash): Path<String>,
    Params(f): Params<Fields>,
    req: Request,
) -> Response {
    if !valid_hash(&hash) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(a) = w.store.attachment(&hash).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Referenced files are never read on behalf of a browser request.
    if a.storage != Storage::Stored || !w.store.blob(&hash).is_file() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let inline = inline_safe(&a.mime) && get_field(&f, "download") != "1";
    let policy = if a.mime == "application/pdf" {
        "default-src 'none'; frame-ancestors 'self'"
    } else {
        "default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'; sandbox"
    };
    let mime = if inline {
        a.mime.clone()
    } else {
        "application/octet-stream".into()
    };
    serve(
        w.store.blob(&hash),
        req,
        &mime,
        disposition(if inline { "inline" } else { "attachment" }, &a.name),
        Some(policy),
    )
    .await
}

/// The first part of a text file, for a preview that needs no extra permissions.
async fn file_text(State(w): State<Web>, Path(hash): Path<String>) -> Response {
    if !valid_hash(&hash) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(a) = w.store.attachment(&hash).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let texty = a.mime.starts_with("text/") || a.mime.contains("json") || a.mime.contains("xml");
    if !texty || a.storage != Storage::Stored {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let Ok(mut f) = tokio::fs::File::open(w.store.blob(&hash)).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut buffer = vec![0u8; 64 * 1024];
    let mut read = 0;
    while read < buffer.len() {
        match tokio::io::AsyncReadExt::read(&mut f, &mut buffer[read..]).await {
            Ok(0) | Err(_) => break,
            Ok(n) => read += n,
        }
    }
    buffer.truncate(read);
    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; sandbox",
            ),
        ],
        String::from_utf8_lossy(&buffer).into_owned(),
    )
        .into_response()
}

// ---- settings and AI clients

async fn set_autostart(Form(f): Form<Fields>) -> Response {
    match shell::set_autostart(get_field(&f, "enabled") == "1") {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, public_error(&e)).into_response(),
    }
}

async fn quit(State(w): State<Web>) -> Response {
    // Answer first, then stop: the page shows a goodbye instead of an error.
    let app = w.app.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        app.quit.notify_one();
    });
    StatusCode::NO_CONTENT.into_response()
}

fn mcp_url(w: &Web) -> String {
    format!("{}/mcp", w.base)
}

async fn clients_status(State(w): State<Web>) -> Response {
    let url = mcp_url(&w);
    let list: Vec<Value> = Client::ALL
        .into_iter()
        .map(|c| {
            let mut v = serde_json::to_value(clients::status(c, &url)).unwrap_or(Value::Null);
            v["last_seen"] = match w.app.last_seen(c.handshake_name()) {
                Some((name, at)) => json!({"client": name, "at": at}),
                None => Value::Null,
            };
            v
        })
        .collect();
    let recent: Vec<Value> = w
        .app
        .all_seen()
        .into_iter()
        .take(8)
        .map(|(name, s)| json!({"name": name, "at": s.at, "calls": s.calls, "agent": s.agent}))
        .collect();
    Json(json!({"url": url, "clients": list, "recent": recent})).into_response()
}

async fn client_connect(
    State(w): State<Web>,
    Path(id): Path<String>,
    Form(f): Form<Fields>,
) -> Response {
    let Some(c) = Client::parse(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // The usage rules go in with the connection unless the caller opts out (rules=0).
    let rules = |ok: bool| -> Value {
        if !ok || get_field(&f, "rules") == "0" {
            return json!({"rules": false});
        }
        match clients::write_rules(c) {
            Ok(_) => json!({"rules": true}),
            Err(e) => json!({"rules": false, "rules_error": public_error(&e)}),
        }
    };
    match clients::connect(c, &mcp_url(&w), get_field(&f, "replace") == "1") {
        Ok(Outcome::Connected { backup }) => {
            let mut v = json!({"ok": true, "backup": backup});
            v["rules"] = rules(true)["rules"].clone();
            Json(v).into_response()
        }
        Ok(Outcome::Already) => {
            let mut v = json!({"ok": true, "already": true});
            let r = rules(true);
            v["rules"] = r["rules"].clone();
            if clients::rules_present(c) {
                v["already"] = json!(true);
            }
            Json(v).into_response()
        }
        Ok(Outcome::Conflict { existing }) => (
            StatusCode::CONFLICT,
            Json(json!({"conflict": true, "existing": existing})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": public_error(&e)})),
        )
            .into_response(),
    }
}

async fn client_disconnect(Path(id): Path<String>) -> Response {
    let Some(c) = Client::parse(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match clients::disconnect(c) {
        Ok((removed, backup)) => {
            let rules_removed = clients::remove_rules(c).unwrap_or(false);
            Json(json!({"ok": true, "removed": removed || rules_removed, "backup": backup, "rules_removed": rules_removed})).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": public_error(&e)})),
        )
            .into_response(),
    }
}
