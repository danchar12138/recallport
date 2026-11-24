//! Data (backups, restore, automatic backup, start with Windows) and connecting AI clients.
//!
//! Pages: `/data` and `/connect`. Everything they do goes through `/api/data/…` and `/api/clients/…` with JSON in
//! and JSON out; a failed call answers `{"error": "…"}` with a status that says why. The long operations (backup,
//! restore, import) are background jobs: the call answers `202 {"id"}` and the page polls
//! `/api/data/jobs/{id}`.
use super::{Web, failure, human};
use crate::{
    app::App,
    clients::{self, Client, Outcome},
    data::{BackupFile, ImportReport, KEEP_AUTO, Mode},
    jobs::Job,
    service::{Service, Stats},
    shell,
};
use anyhow::{Context, Result, bail, ensure};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path as FsPath, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{Instant, timeout_at},
};
use tower::ServiceExt;
use tower_http::services::ServeFile;

/// A backup ZIP can hold years of attachments; only this route accepts bodies that large.
const MAX_UPLOAD: usize = 64 << 30;

pub(super) fn routes() -> Router<Web> {
    Router::new()
        .route("/data", get(data_page))
        .route("/connect", get(connect_page))
        .route("/backup/{name}", get(download))
        .route("/api/data/backup", post(start_backup))
        .route("/api/data/restore", post(start_restore))
        .route(
            "/api/data/import",
            post(start_import).layer(DefaultBodyLimit::max(MAX_UPLOAD)),
        )
        .route("/api/data/jobs/{id}", get(job_state))
        .route("/api/data/backups/{name}", delete(delete_backup))
        .route("/api/data/auto", post(set_auto))
        .route("/api/data/autostart", post(set_autostart))
        .route("/api/clients/verify", post(verify))
        .route("/api/clients/{id}/connect", post(connect))
        .route("/api/clients/{id}/disconnect", post(disconnect))
        .route("/api/clients/{id}/rules", post(set_rules))
}

fn fail(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({"error": message.into()}))).into_response()
}

fn busy() -> Response {
    fail(
        StatusCode::CONFLICT,
        "另一项耗时操作正在进行，请等它完成后再试。",
    )
}

#[derive(Deserialize)]
struct Toggle {
    enabled: bool,
}

// ---------- /data ----------

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "auto" => "自动",
        "before-restore" => "恢复前的副本",
        _ => "手动",
    }
}

/// The numbers and lists of the data page. `autostart` is `None` where the platform has no such thing.
fn data_model(
    dir: &FsPath,
    stats: &Stats,
    backups: &[BackupFile],
    auto: bool,
    autostart: Option<bool>,
    busy: bool,
) -> Value {
    json!({
        "View": "data",
        "Title": "数据",
        "Nodes": stats.nodes,
        "Trash": stats.deleted,
        "Stored": stats.files.stored,
        "Referenced": stats.files.referenced,
        "FilesSize": human(stats.files.bytes),
        "LastChange": stats.last_change,
        "Dir": dir.display().to_string(),
        "Empty": stats.nodes == 0,
        "Backups": backups.iter().map(|b| json!({
            "name": b.name,
            "kind": b.kind,
            "label": kind_label(b.kind),
            "size": human(b.bytes),
            "at": b.at,
        })).collect::<Vec<_>>(),
        "Auto": auto,
        "Keep": KEEP_AUTO,
        "AutostartSupported": autostart.is_some(),
        "Autostart": autostart.unwrap_or(false),
        "Busy": busy,
    })
}

async fn data_page(State(w): State<Web>) -> Response {
    let read = w
        .service
        .run(|s| (s.stats(), s.backups(), s.auto_backup_enabled()))
        .await;
    let Ok((stats, backups, auto)) = read else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let autostart = shell::SUPPORTED.then(shell::autostart_enabled);
    w.render(data_model(
        &w.service.dir,
        &stats,
        &backups,
        auto,
        autostart,
        w.jobs.busy(),
    ))
}

async fn download(State(w): State<Web>, Path(name): Path<String>, req: Request) -> Response {
    let Some(path) = w.service.backup_path(&name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match ServeFile::new(path).oneshot(req).await {
        Ok(res) => {
            let mut res = res.map(Body::new);
            let h = res.headers_mut();
            h.insert(header::CONTENT_TYPE, "application/zip".parse().unwrap());
            // `valid_name` allows only ASCII letters, digits and dashes, so the name needs no escaping.
            if let Ok(value) = format!("attachment; filename=\"{name}\"").parse() {
                h.insert(header::CONTENT_DISPOSITION, value);
            }
            res.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Run `work` as the background job `job`; whatever it returns (or a panic) ends up in the job state.
fn run_job(
    service: Arc<Service>,
    job: Job,
    work: impl FnOnce(&Service, &Job) -> Result<Value> + Send + 'static,
) -> Response {
    let id = job.snapshot().id;
    tokio::spawn(async move {
        let inner = job.clone();
        match service.run(move |s| work(s, &inner)).await {
            Ok(Ok(result)) => job.succeed(result),
            Ok(Err(error)) => job.fail(failure(&error)),
            Err(error) => job.fail(format!("内部错误：{error}")),
        }
    });
    (StatusCode::ACCEPTED, Json(json!({"id": id}))).into_response()
}

fn parse_mode(text: &str) -> Option<Mode> {
    match text {
        "replace" => Some(Mode::Replace),
        "merge" => Some(Mode::Merge),
        _ => None,
    }
}

/// What a finished restore or import tells the page.
fn report_json(report: &ImportReport, source: &str) -> Value {
    let mut value = serde_json::to_value(report).unwrap_or_default();
    value["source"] = json!(source);
    value
}

async fn start_backup(State(w): State<Web>) -> Response {
    let Some(job) = w.jobs.start("backup") else {
        return busy();
    };
    run_job(w.service.clone(), job, |service, job| {
        let name = service.make_backup(job)?;
        let bytes = std::fs::metadata(service.backup_dir().join(&name))?.len();
        Ok(json!({"name": name, "size": human(bytes)}))
    })
}

#[derive(Deserialize)]
struct RestoreBody {
    name: String,
    mode: String,
}

async fn start_restore(State(w): State<Web>, Json(body): Json<RestoreBody>) -> Response {
    let Some(mode) = parse_mode(&body.mode) else {
        return fail(StatusCode::BAD_REQUEST, "mode 只能是 replace 或 merge");
    };
    let Some(path) = w.service.backup_path(&body.name) else {
        return fail(StatusCode::NOT_FOUND, "没有这个备份");
    };
    let Some(job) = w.jobs.start("restore") else {
        return busy();
    };
    run_job(w.service.clone(), job, move |service, job| {
        let report = service.import_archive(&path, mode, job)?;
        Ok(report_json(&report, &body.name))
    })
}

struct Upload {
    mode: Mode,
    source: String,
}

/// Stream the multipart body to `path`: the ZIP never sits in memory. Fields: `mode` and `file`, in any order.
async fn receive(multipart: &mut Multipart, path: &FsPath) -> Result<Upload> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let (mut mode, mut source, mut size) = (None, String::new(), 0u64);
    while let Some(mut field) = multipart.next_field().await.context("上传中断了")? {
        match field.name() {
            Some("mode") => mode = Some(field.text().await.context("上传中断了")?),
            Some("file") => {
                source = field
                    .file_name()
                    .unwrap_or_default()
                    .chars()
                    .take(120)
                    .collect();
                let mut out = tokio::fs::File::create(path).await?;
                while let Some(chunk) = field.chunk().await.context("上传中断了")? {
                    size += chunk.len() as u64;
                    out.write_all(&chunk).await?;
                }
                out.flush().await?;
            }
            _ => {}
        }
    }
    ensure!(size > 0, "没有收到文件");
    let mode = mode
        .as_deref()
        .and_then(parse_mode)
        .context("没有说明是替换还是合并")?;
    Ok(Upload { mode, source })
}

async fn start_import(State(w): State<Web>, mut multipart: Multipart) -> Response {
    // The job exists from the first byte, so nothing else can start (or sweep the temporary folder) meanwhile.
    let Some(job) = w.jobs.start("import") else {
        return busy();
    };
    job.phase("接收文件", 0.0, 0.0);
    let path = w
        .service
        .files
        .tmp_dir()
        .join(format!("upload-{}.zip", uuid::Uuid::new_v4().simple()));
    match receive(&mut multipart, &path).await {
        Ok(Upload { mode, source }) => run_job(w.service.clone(), job, move |service, job| {
            let report = service.import_archive(&path, mode, job);
            let _ = std::fs::remove_file(&path);
            Ok(report_json(&report?, &source))
        }),
        Err(error) => {
            let _ = tokio::fs::remove_file(&path).await;
            let message = failure(&error);
            job.fail(message.clone());
            fail(StatusCode::BAD_REQUEST, message)
        }
    }
}

async fn job_state(State(w): State<Web>, Path(id): Path<String>) -> Response {
    match w.jobs.get(&id) {
        Some(job) => Json(job.snapshot()).into_response(),
        None => fail(StatusCode::NOT_FOUND, "没有这项操作（服务可能重启过）"),
    }
}

async fn delete_backup(State(w): State<Web>, Path(name): Path<String>) -> Response {
    // A restore may be reading it right now.
    if w.jobs.busy() {
        return busy();
    }
    match w.service.delete_backup(&name) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(error) => fail(StatusCode::NOT_FOUND, failure(&error)),
    }
}

async fn set_auto(State(w): State<Web>, Json(body): Json<Toggle>) -> Response {
    match w.service.set_auto_backup(body.enabled) {
        Ok(()) => Json(json!({"enabled": w.service.auto_backup_enabled()})).into_response(),
        Err(error) => fail(StatusCode::INTERNAL_SERVER_ERROR, failure(&error)),
    }
}

async fn set_autostart(Json(body): Json<Toggle>) -> Response {
    if !shell::SUPPORTED {
        return fail(StatusCode::NOT_FOUND, "此平台不支持开机自启");
    }
    match shell::set_autostart(body.enabled) {
        Ok(()) => Json(json!({"enabled": shell::autostart_enabled()})).into_response(),
        Err(error) => fail(StatusCode::INTERNAL_SERVER_ERROR, failure(&error)),
    }
}

// ---------- /connect ----------

/// What the connection page shows for one client: the state of its config file, the rules block and when a client
/// of that name last called the MCP endpoint.
fn client_card(status: &clients::Status, seen: Option<&(String, String, u64)>) -> Value {
    let mut card = serde_json::to_value(status).unwrap_or_default();
    card["seen"] = match seen {
        Some((name, at, calls)) => json!({"name": name, "at": at, "calls": calls}),
        None => Value::Null,
    };
    card
}

fn seen_of(app: &App, client: Client) -> Option<(String, String, u64)> {
    let (name, at) = app.last_seen(client.handshake_name())?;
    let calls = app
        .all_seen()
        .into_iter()
        .find(|(n, _)| *n == name)
        .map_or(0, |(_, s)| s.calls);
    Some((name, at, calls))
}

fn connect_model(url: &str, cards: Vec<Value>, recent: Vec<Value>) -> Value {
    json!({
        "View": "connect",
        "Title": "接入",
        "Url": url,
        "Clients": cards,
        "Recent": recent,
        "Rules": clients::RULES.trim_end(),
        "Tools": TOOLS,
    })
}

async fn connect_page(State(w): State<Web>) -> Response {
    let url = format!("{}/mcp", w.base);
    let cards = Client::ALL
        .into_iter()
        .map(|c| client_card(&clients::status(c, &url), seen_of(&w.app, c).as_ref()))
        .collect();
    let recent = w
        .app
        .all_seen()
        .into_iter()
        .take(8)
        .map(|(name, s)| json!({"name": name, "at": s.at, "calls": s.calls, "agent": s.agent}))
        .collect();
    w.render(connect_model(&url, cards, recent))
}

#[derive(Deserialize, Default)]
struct ConnectBody {
    /// Replace a same-named entry that points somewhere else.
    #[serde(default)]
    replace: bool,
}

async fn connect(
    State(w): State<Web>,
    Path(id): Path<String>,
    body: Option<Json<ConnectBody>>,
) -> Response {
    let Some(client) = Client::parse(&id) else {
        return fail(StatusCode::NOT_FOUND, "不认识这个客户端");
    };
    let replace = body.is_some_and(|b| b.replace);
    match clients::connect(client, &format!("{}/mcp", w.base), replace) {
        Ok(Outcome::Connected { backup }) => {
            Json(json!({"ok": true, "already": false, "backup": backup})).into_response()
        }
        Ok(Outcome::Already) => Json(json!({"ok": true, "already": true, "backup": null})).into_response(),
        Ok(Outcome::Conflict { existing }) => (
            StatusCode::CONFLICT,
            Json(json!({"error": format!("{} 的配置里已经有一个叫 recallport 的项，指向 {existing}。", client.name()), "conflict": true, "existing": existing})),
        )
            .into_response(),
        Err(error) => fail(StatusCode::BAD_REQUEST, failure(&error)),
    }
}

async fn disconnect(Path(id): Path<String>) -> Response {
    let Some(client) = Client::parse(&id) else {
        return fail(StatusCode::NOT_FOUND, "不认识这个客户端");
    };
    match clients::disconnect(client) {
        Ok((removed, backup)) => {
            Json(json!({"ok": true, "removed": removed, "backup": backup})).into_response()
        }
        Err(error) => fail(StatusCode::BAD_REQUEST, failure(&error)),
    }
}

async fn set_rules(Path(id): Path<String>, Json(body): Json<Toggle>) -> Response {
    let Some(client) = Client::parse(&id) else {
        return fail(StatusCode::NOT_FOUND, "不认识这个客户端");
    };
    let done = if body.enabled {
        clients::write_rules(client).map(|done| (done.changed, done.backup))
    } else {
        clients::remove_rules(client).map(|removed| (removed, None))
    };
    match done {
        Ok((changed, backup)) => {
            Json(json!({"ok": true, "changed": changed, "backup": backup})).into_response()
        }
        Err(error) => fail(StatusCode::BAD_REQUEST, failure(&error)),
    }
}

// ---------- checking the MCP endpoint ----------

/// The tools every client is supposed to find (the contract of the MCP layer).
const TOOLS: [&str; 9] = [
    "search", "get", "write", "update", "move", "delete", "restore", "trash", "maintain",
];

const PROTOCOL: &str = "2025-11-25";
/// The whole check, one request at a time, has to be done by then.
const CHECK_TIME: Duration = Duration::from_secs(10);

/// What the endpoint answered to a real handshake.
#[derive(Debug, PartialEq)]
struct McpCheck {
    server: String,
    protocol: String,
    tools: Vec<String>,
}

/// The server asks its own MCP endpoint what a client would ask: `initialize`, then `tools/list`.
async fn verify(State(w): State<Web>) -> Response {
    let answer = match check_mcp(&w.host).await {
        Ok(check) => {
            let missing: Vec<&str> = TOOLS
                .into_iter()
                .filter(|t| !check.tools.iter().any(|name| name == t))
                .collect();
            let error = (!missing.is_empty()).then(|| format!("缺少工具：{}", missing.join("、")));
            json!({
                "ok": missing.is_empty(),
                "server": check.server,
                "protocol": check.protocol,
                "tools": check.tools,
                "missing": missing,
                "expected": TOOLS.len(),
                "error": error,
            })
        }
        Err(error) => json!({
            "ok": false,
            "tools": [],
            "missing": TOOLS,
            "expected": TOOLS.len(),
            "error": failure(&error),
        }),
    };
    Json(answer).into_response()
}

async fn check_mcp(addr: &str) -> Result<McpCheck> {
    let rpc = |id: Option<u64>, method: &str, params: Value| {
        let mut message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        if let Some(id) = id {
            message["id"] = json!(id);
        }
        message
    };
    let init = rpc(
        Some(1),
        "initialize",
        json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "RecallPort Web", "version": env!("CARGO_PKG_VERSION")},
        }),
    );
    let reply = exchange(addr, "POST", &[], Some(&init), Some(1))
        .await
        .context("连不上本机的 MCP 服务，请确认 RecallPort 仍在运行")?;
    let result = rpc_result(&reply).context("MCP 握手（initialize）失败")?;
    let protocol = result["protocolVersion"]
        .as_str()
        .unwrap_or(PROTOCOL)
        .to_owned();
    let server = format!(
        "{} {}",
        result["serverInfo"]["name"]
            .as_str()
            .unwrap_or("RecallPort"),
        result["serverInfo"]["version"].as_str().unwrap_or_default()
    )
    .trim()
    .to_owned();
    let mut headers = vec![("MCP-Protocol-Version", protocol.clone())];
    if let Some(session) = &reply.session {
        headers.push(("Mcp-Session-Id", session.clone()));
    }
    let done = async {
        let note = rpc(None, "notifications/initialized", json!({}));
        let sent = exchange(addr, "POST", &headers, Some(&note), None).await?;
        ensure!(sent.status / 100 == 2, "HTTP {}", sent.status);
        let list = rpc(Some(2), "tools/list", json!({}));
        let reply = exchange(addr, "POST", &headers, Some(&list), Some(2)).await?;
        rpc_result(&reply)
    }
    .await;
    // Leave no session behind; whether the server cares is its own business.
    if reply.session.is_some() {
        let _ = exchange(addr, "DELETE", &headers, None, None).await;
    }
    let tools = done.context("读取工具列表（tools/list）失败")?;
    Ok(McpCheck {
        server,
        protocol,
        tools: tools["tools"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|t| t["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// The `result` of a JSON-RPC reply, or why there is none.
fn rpc_result(reply: &Reply) -> Result<Value> {
    ensure!(
        reply.status / 100 == 2,
        "HTTP {}{}",
        reply.status,
        if reply.status == 403 {
            "（本机服务拒绝了这个请求）"
        } else {
            ""
        }
    );
    let message = reply.message.as_ref().context("服务没有给出回复")?;
    if let Some(error) = message.get("error") {
        bail!("{}", error["message"].as_str().unwrap_or("服务返回了错误"));
    }
    message.get("result").cloned().context("回复里没有 result")
}

#[derive(Debug)]
struct Reply {
    status: u16,
    session: Option<String>,
    /// The JSON-RPC message with the id that was asked for.
    message: Option<Value>,
}

/// One HTTP/1.1 request to `/mcp` on a fresh connection. There is no HTTP client in this program, and the
/// answer may be plain JSON or a server-sent event stream, so this reads just enough of either: it returns
/// as soon as the awaited message has arrived, or when the server has finished (or the time is up).
async fn exchange(
    addr: &str,
    method: &str,
    headers: &[(&str, String)],
    body: Option<&Value>,
    id: Option<u64>,
) -> Result<Reply> {
    let deadline = Instant::now() + CHECK_TIME;
    let mut stream = timeout_at(deadline, TcpStream::connect(addr))
        .await
        .context("连接超时")??;
    let payload = body.map(Value::to_string).unwrap_or_default();
    let mut request = format!(
        "{method} /mcp HTTP/1.1\r\nHost: {addr}\r\n\
         User-Agent: Mozilla/5.0 (RecallPort admin check)\r\n\
         Accept: application/json, text/event-stream\r\nConnection: close\r\n"
    );
    // The User-Agent above keeps the MCP layer from counting this check as an AI client; the same goes for
    // the `RecallPort Web` name in the handshake.
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if body.is_some() {
        request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            payload.len()
        ));
    }
    request.push_str("\r\n");
    request.push_str(&payload);
    timeout_at(deadline, stream.write_all(request.as_bytes()))
        .await
        .context("发送超时")??;
    let (mut raw, mut chunk) = (Vec::new(), [0u8; 16 * 1024]);
    loop {
        if let Some(reply) = parse_reply(&raw, id, false)? {
            return Ok(reply);
        }
        match timeout_at(deadline, stream.read(&mut chunk)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => raw.extend_from_slice(&chunk[..n]),
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => bail!("等待回复超时"),
        }
    }
    parse_reply(&raw, id, true)?.context("服务返回的不是 HTTP 回复")
}

/// Read what has arrived so far. `None`: wait for more. `finished` says the connection is over.
fn parse_reply(raw: &[u8], id: Option<u64>, finished: bool) -> Result<Option<Reply>> {
    let Some(head_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Ok(None);
    };
    let head = String::from_utf8_lossy(&raw[..head_end]);
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .context("服务返回的不是 HTTP 回复")?;
    let header = |wanted: &str| {
        lines
            .clone()
            .filter_map(|l| l.split_once(':'))
            .find(|(name, _)| name.trim().eq_ignore_ascii_case(wanted))
            .map(|(_, value)| value.trim().to_owned())
    };
    let body = &raw[head_end + 4..];
    let chunked = header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked"));
    let (body, over) = if chunked {
        dechunk(body)
    } else {
        let length = header("content-length").and_then(|v| v.parse::<usize>().ok());
        (
            body.to_vec(),
            finished || length.is_some_and(|n| body.len() >= n),
        )
    };
    let message = id.and_then(|id| {
        let events = header("content-type").is_some_and(|t| t.contains("text/event-stream"));
        find_message(&body, events, id)
    });
    if !(over || finished || message.is_some() || (id.is_none() && status / 100 != 2)) {
        return Ok(None);
    }
    Ok(Some(Reply {
        status,
        session: header("mcp-session-id"),
        message,
    }))
}

/// The bytes of a chunked body that have arrived, and whether its last chunk is among them.
fn dechunk(mut rest: &[u8]) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    loop {
        let Some(line_end) = rest.windows(2).position(|w| w == b"\r\n") else {
            return (out, false);
        };
        let size = String::from_utf8_lossy(&rest[..line_end]);
        let Ok(size) = usize::from_str_radix(size.split(';').next().unwrap_or("").trim(), 16)
        else {
            return (out, true);
        };
        if size == 0 {
            return (out, true);
        }
        let start = line_end + 2;
        if rest.len() < start + size {
            out.extend_from_slice(&rest[start..]);
            return (out, false);
        }
        out.extend_from_slice(&rest[start..start + size]);
        rest = rest.get(start + size + 2..).unwrap_or(&[]);
    }
}

/// The JSON-RPC message with this id, from a JSON body or from the `data:` lines of an event stream.
fn find_message(body: &[u8], events: bool, id: u64) -> Option<Value> {
    let text = String::from_utf8_lossy(body);
    let wanted = |v: &Value| v["id"].as_u64() == Some(id);
    if events {
        text.lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
            .find(wanted)
    } else {
        match serde_json::from_str::<Value>(&text).ok()? {
            Value::Array(list) => list.into_iter().find(wanted),
            single => wanted(&single).then_some(single),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        data::BackupFile,
        embedding::{self, Embed},
        service::{FileStats, WriteInput},
    };
    use axum::{extract::Request as AxumRequest, http::Method};
    use minijinja::{Environment, UndefinedBehavior};

    struct Words;
    impl Embed for Words {
        fn encode(&self, input: &str) -> Result<Vec<f32>> {
            let mut v = vec![0.0f32; embedding::DIMENSIONS as usize];
            let at = input.bytes().fold(7usize, |h, b| {
                h.wrapping_mul(31).wrapping_add(usize::from(b))
            }) % v.len();
            v[at] = 1.0;
            Ok(v)
        }
    }

    // ----- the check of the MCP endpoint, against a stand-in responder -----

    /// A small MCP server. `stream` answers with server-sent events and a session id, as a stateful server does;
    /// `tools` is what it offers.
    async fn fake_mcp(stream: bool, tools: &'static [&'static str]) -> String {
        async fn handle(
            stream: bool,
            tools: &'static [&'static str],
            req: AxumRequest,
        ) -> Response {
            if req.method() == Method::DELETE {
                return StatusCode::NO_CONTENT.into_response();
            }
            let session = req.headers().get("mcp-session-id").is_some();
            let bytes = axum::body::to_bytes(req.into_body(), 1 << 20)
                .await
                .unwrap();
            let message: Value = serde_json::from_slice(&bytes).unwrap();
            let result = match message["method"].as_str() {
                Some("initialize") => {
                    json!({"protocolVersion": "2025-06-18", "serverInfo": {"name": "fake", "version": "9"}})
                }
                Some("tools/list") => {
                    assert!(!stream || session, "the session id must come back");
                    json!({"tools": tools.iter().map(|t| json!({"name": t})).collect::<Vec<_>>()})
                }
                _ => return StatusCode::ACCEPTED.into_response(),
            };
            let reply = json!({"jsonrpc": "2.0", "id": message["id"], "result": result});
            if stream {
                (
                    [
                        (header::CONTENT_TYPE, "text/event-stream"),
                        (header::HeaderName::from_static("mcp-session-id"), "s1"),
                    ],
                    format!("data: \n\nid: 0\nretry: 3000\n\nevent: message\ndata: {reply}\n\n"),
                )
                    .into_response()
            } else {
                Json(reply).into_response()
            }
        }
        let router = Router::new().route(
            "/mcp",
            post(move |req: AxumRequest| handle(stream, tools, req))
                .delete(move |req: AxumRequest| handle(stream, tools, req)),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(listener, router).await });
        addr
    }

    #[tokio::test]
    async fn the_check_lists_the_tools_whether_the_answer_is_json_or_an_event_stream() {
        for stream in [false, true] {
            let addr = fake_mcp(stream, &TOOLS).await;
            let check = check_mcp(&addr).await.unwrap();
            assert_eq!(check.protocol, "2025-06-18");
            assert_eq!(check.server, "fake 9");
            assert_eq!(check.tools, TOOLS);
        }
    }

    #[tokio::test]
    async fn the_check_reports_a_dead_endpoint_and_a_wrong_one() {
        // Nothing listens here.
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().to_string()
        };
        let error = format!("{:#}", check_mcp(&closed).await.unwrap_err());
        assert!(error.contains("连不上"), "{error}");
        // Something answers, but it is no MCP server.
        let site = Router::new().route("/mcp", post(|| async { (StatusCode::NOT_FOUND, "nope") }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(listener, site).await });
        let error = format!("{:#}", check_mcp(&addr).await.unwrap_err());
        assert!(
            error.contains("initialize") && error.contains("HTTP 404"),
            "{error}"
        );
    }

    #[test]
    fn replies_are_read_while_they_arrive() {
        let chunked = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nMcp-Session-Id: abc\r\n\r\n";
        let event = "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\n\n";
        let mut raw = chunked.to_vec();
        // The head alone is not an answer yet.
        assert!(parse_reply(&raw, Some(2), false).unwrap().is_none());
        raw.extend_from_slice(format!("{:x}\r\n{event}\r\n", event.len()).as_bytes());
        // The awaited event is there although the stream is still open.
        let reply = parse_reply(&raw, Some(2), false).unwrap().unwrap();
        assert_eq!(reply.session.as_deref(), Some("abc"));
        assert_eq!(reply.message.unwrap()["result"]["tools"], json!([]));
        // Another id does not count; a half chunk is not misread.
        assert!(parse_reply(&raw, Some(7), false).unwrap().is_none());
        let (text, over) = dechunk(b"5\r\nhel");
        assert_eq!((text.as_slice(), over), (&b"hel"[..], false));
        let (text, over) = dechunk(b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n");
        assert_eq!((text.as_slice(), over), (&b"abcde"[..], true));
        // A notification is answered by a status alone.
        let accepted = parse_reply(
            b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n",
            None,
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(accepted.status, 202);
    }

    // ----- the routes, through the whole router -----

    /// A library with the real router in front of it, also served on a socket so a request can be held open.
    struct Site {
        router: axum::Router,
        addr: String,
        service: Arc<Service>,
    }

    async fn open(dir: &FsPath) -> Site {
        let service = Service::open(dir, Arc::new(Words)).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = super::super::router(service.clone(), address, App::new(false, None)).unwrap();
        let served = router.clone();
        tokio::spawn(async move { axum::serve(listener, served).await });
        Site {
            router,
            addr: address.to_string(),
            service,
        }
    }

    impl Site {
        async fn call(
            &self,
            method: &str,
            uri: &str,
            content_type: Option<&str>,
            body: Vec<u8>,
        ) -> (StatusCode, Vec<u8>) {
            let mut request = AxumRequest::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, &self.addr);
            if let Some(t) = content_type {
                request = request.header(header::CONTENT_TYPE, t);
            }
            let response = self
                .router
                .clone()
                .oneshot(request.body(Body::from(body)).unwrap())
                .await
                .unwrap();
            let status = response.status();
            (
                status,
                axum::body::to_bytes(response.into_body(), 1 << 28)
                    .await
                    .unwrap()
                    .to_vec(),
            )
        }

        async fn json(&self, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
            let (status, bytes) = self
                .call(
                    method,
                    uri,
                    Some("application/json"),
                    body.to_string().into_bytes(),
                )
                .await;
            (
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        }

        /// Post an upload and wait for its job.
        async fn upload(&self, mode: &str, file: &[u8]) -> Value {
            let (content_type, body) = multipart(mode, file);
            let (status, bytes) = self
                .call("POST", "/api/data/import", Some(&content_type), body)
                .await;
            assert_eq!(
                status,
                StatusCode::ACCEPTED,
                "{}",
                String::from_utf8_lossy(&bytes)
            );
            self.finished(
                serde_json::from_slice::<Value>(&bytes).unwrap()["id"]
                    .as_str()
                    .unwrap(),
            )
            .await
        }

        async fn finished(&self, id: &str) -> Value {
            for _ in 0..200 {
                let (status, state) = self
                    .json("GET", &format!("/api/data/jobs/{id}"), json!(null))
                    .await;
                assert_eq!(status, StatusCode::OK);
                if state["finished"] == true {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            panic!("the job never finished");
        }

        /// Occupy the one job slot: an upload that announces a big body and then goes quiet.
        async fn hold_the_job(&self) -> TcpStream {
            let mut stream = TcpStream::connect(&self.addr).await.unwrap();
            let head = format!(
                "POST /api/data/import HTTP/1.1\r\nHost: {}\r\nContent-Type: multipart/form-data; boundary=b\r\nContent-Length: 1000000\r\n\r\n--b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x.zip\"\r\n\r\nPK",
                self.addr
            );
            stream.write_all(head.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(300)).await;
            stream
        }
    }

    fn multipart(mode: &str, file: &[u8]) -> (String, Vec<u8>) {
        let boundary = "----rp-test-boundary";
        let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"mode\"\r\n\r\n{mode}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"my backup.zip\"\r\nContent-Type: application/zip\r\n\r\n").into_bytes();
        body.extend_from_slice(file);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        (format!("multipart/form-data; boundary={boundary}"), body)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_backup_is_made_listed_downloaded_restored_uploaded_and_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let site = open(dir.path()).await;
        site.service
            .write(
                WriteInput {
                    title: "记忆".into(),
                    content: "备份每天凌晨三点跑".into(),
                    ..Default::default()
                },
                "test",
            )
            .unwrap();

        let (status, started) = site.json("POST", "/api/data/backup", json!(null)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let state = site.finished(started["id"].as_str().unwrap()).await;
        assert_eq!(state["error"], Value::Null, "{state}");
        let name = state["result"]["name"].as_str().unwrap().to_owned();
        assert!(
            name.starts_with("RecallPort-backup-")
                && state["result"]["size"].as_str().unwrap().contains('B')
        );
        assert_eq!(site.service.backups().len(), 1);

        // Download: only names that `valid_name` knows, and only files that exist.
        let (status, zip) = site
            .call("GET", &format!("/backup/{name}"), None, vec![])
            .await;
        assert_eq!((status, &zip[..2]), (StatusCode::OK, &b"PK"[..]));
        for bad in [
            "..%2Fsettings.json",
            "RecallPort-backup-20990101-0000.zip",
            "recallport.lock",
        ] {
            assert_eq!(
                site.call("GET", &format!("/backup/{bad}"), None, vec![])
                    .await
                    .0,
                StatusCode::NOT_FOUND,
                "{bad}"
            );
        }

        // Upload the ZIP to a second library: streamed, merged, reported; the temporary copy is gone.
        let other = tempfile::tempdir().unwrap();
        let second = open(other.path()).await;
        let state = second.upload("merge", &zip).await;
        assert_eq!(state["error"], Value::Null, "{state}");
        assert_eq!(state["result"]["nodes"], 1);
        assert_eq!(state["result"]["merge"]["added"], 1);
        assert_eq!(state["result"]["source"], "my backup.zip");
        assert_eq!(second.service.stats().nodes, 1);
        assert!(
            !other
                .path()
                .join("tmp")
                .read_dir()
                .is_ok_and(|mut d| d.next().is_some())
        );
        // A replace keeps a copy of the library it replaces.
        let state = second.upload("replace", &zip).await;
        assert!(
            state["result"]["kept_as"]
                .as_str()
                .unwrap()
                .starts_with("RecallPort-before-restore-")
        );
        // Bad uploads fail readably and do not leave the program busy.
        let state = second.upload("merge", b"this is not a zip").await;
        assert!(state["error"].as_str().unwrap().contains("ZIP"), "{state}");
        let (content_type, body) = multipart("sideways", b"x");
        let (status, error) = second
            .call("POST", "/api/data/import", Some(&content_type), body)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(String::from_utf8_lossy(&error).contains("替换还是合并"));
        assert_eq!(second.upload("merge", &zip).await["error"], Value::Null);

        // Restore from the list: validated name and mode, then a job.
        let restore = |name: &str, mode: &str| json!({"name": name, "mode": mode});
        assert_eq!(
            site.json("POST", "/api/data/restore", restore("../x.zip", "merge"))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            site.json("POST", "/api/data/restore", restore(&name, "both"))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        let (status, started) = site
            .json("POST", "/api/data/restore", restore(&name, "merge"))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let state = site.finished(started["id"].as_str().unwrap()).await;
        assert_eq!(state["result"]["merge"]["unchanged"], 1);

        // Only one heavy job at a time, and a backup is not deleted while one runs.
        let held = site.hold_the_job().await;
        assert_eq!(
            site.json("POST", "/api/data/backup", json!(null)).await.0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            site.json("POST", "/api/data/restore", restore(&name, "merge"))
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            site.json("DELETE", &format!("/api/data/backups/{name}"), json!(null))
                .await
                .0,
            StatusCode::CONFLICT
        );
        drop(held);
        tokio::time::sleep(Duration::from_millis(300)).await;

        let delete = format!("/api/data/backups/{name}");
        assert_eq!(
            site.json("DELETE", &delete, json!(null)).await.0,
            StatusCode::OK
        );
        assert_eq!(
            site.json("DELETE", &delete, json!(null)).await.0,
            StatusCode::NOT_FOUND
        );
        assert!(site.service.backups().is_empty());

        // The automatic backup switch.
        let (_, answer) = site
            .json("POST", "/api/data/auto", json!({"enabled": false}))
            .await;
        assert_eq!(answer["enabled"], false);
        assert!(!site.service.auto_backup_enabled());
        let (_, answer) = site
            .json("POST", "/api/data/auto", json!({"enabled": true}))
            .await;
        assert_eq!(answer["enabled"], true);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dropped_upload_leaves_no_file_and_no_busy_job() {
        let dir = tempfile::tempdir().unwrap();
        let site = open(dir.path()).await;
        drop(site.hold_the_job().await);
        for _ in 0..40 {
            let (status, _) = site.json("POST", "/api/data/backup", json!(null)).await;
            if status == StatusCode::ACCEPTED {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the dropped upload kept the job slot");
    }

    // ----- the two pages' templates, with every kind of model -----

    fn templates() -> Environment<'static> {
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.add_function("local_time", |value: String, _seconds: Option<bool>| value);
        env.add_template("data.html", include_str!("../../web/data.html"))
            .unwrap();
        env.add_template("connect.html", include_str!("../../web/connect.html"))
            .unwrap();
        env
    }

    fn stats(nodes: usize) -> Stats {
        Stats {
            nodes,
            deleted: 2,
            roots: 1,
            layers: vec![],
            tags: vec![],
            files: FileStats {
                stored: 3,
                referenced: 1,
                bytes: 5 << 20,
            },
            last_change: if nodes > 0 {
                "2026-11-22T10:00:00+08:00".into()
            } else {
                String::new()
            },
        }
    }

    fn backup(name: &str, kind: &'static str) -> BackupFile {
        BackupFile {
            name: name.into(),
            kind,
            bytes: 123_456,
            at: "2026-11-22T10:00:00+00:00".into(),
        }
    }

    fn markup_is_clean(html: &str) {
        assert!(
            !html.contains("<script>") && !html.contains(" style=") && !html.contains(" onclick=")
        );
    }

    #[test]
    fn the_data_page_renders_empty_full_and_busy() {
        let env = templates();
        let dir = FsPath::new("RecallPort-data");
        let many: Vec<BackupFile> = (0..12)
            .map(|i| {
                backup(
                    &format!("RecallPort-auto-202611{:02}-0300.zip", i + 1),
                    "auto",
                )
            })
            .chain([
                backup("RecallPort-backup-20261122-1000.zip", "manual"),
                backup(
                    "RecallPort-before-restore-20261122-1001.zip",
                    "before-restore",
                ),
            ])
            .collect();
        for model in [
            data_model(dir, &stats(0), &[], true, None, false),
            data_model(dir, &stats(0), &many[..2], false, Some(true), false),
            data_model(dir, &stats(40), &many, true, Some(false), true),
        ] {
            let html = env
                .get_template("data.html")
                .unwrap()
                .render(&model)
                .unwrap();
            assert!(html.contains("id=\"adm-data\"") && html.contains(&dir.display().to_string()));
            let has_backups = !model["Backups"].as_array().unwrap().is_empty();
            assert!(html.contains("id=\"restore\""));
            assert_eq!(
                html.contains("adm-legacy"),
                model["Empty"] == true && has_backups
            );
            assert_eq!(
                html.contains("data-autostart"),
                model["AutostartSupported"] == true
            );
            assert_eq!(
                html.contains("RecallPort-before-restore-20261122-1001.zip"),
                model["Backups"].as_array().unwrap().len() == 14
            );
            markup_is_clean(&html);
        }
    }

    fn status(state: &'static str) -> clients::Status {
        clients::Status {
            id: "codex",
            name: "Codex",
            scope: "用户级",
            path: "C:/Users/me/.codex/config.toml".into(),
            installed: true,
            state,
            existing: if state == "conflict" {
                "http://127.0.0.1:3999/mcp".into()
            } else {
                String::new()
            },
            error: if state == "error" {
                "配置文件不是有效的 TOML".into()
            } else {
                String::new()
            },
            preview: clients::preview(Client::Codex, "http://127.0.0.1:3765/mcp"),
            command: String::new(),
            rules: state == "connected",
            rules_path: "C:/Users/me/.codex/AGENTS.md".into(),
        }
    }

    #[test]
    fn the_connect_page_renders_every_state_of_a_client_and_documents_every_tool() {
        let env = templates();
        let seen = (
            "codex-mcp-client".to_owned(),
            "2026-11-22T10:00:00+08:00".to_owned(),
            42,
        );
        let recent = vec![
            json!({"name": "codex-mcp-client", "at": "2026-11-22T10:00:00+08:00", "calls": 42, "agent": "codex/1.0"}),
        ];
        for (state, seen) in [
            ("none", None),
            ("connected", Some(&seen)),
            ("connected", None),
            ("conflict", None),
            ("error", None),
        ] {
            let cards = vec![client_card(&status(state), seen)];
            let model = connect_model("http://127.0.0.1:3765/mcp", cards, recent.clone());
            let html = env
                .get_template("connect.html")
                .unwrap()
                .render(&model)
                .unwrap();
            assert!(html.contains("id=\"adm-connect\"") && html.contains("127.0.0.1:3765"));
            markup_is_clean(&html);
        }
        let none = connect_model("http://127.0.0.1:3765/mcp", vec![], vec![]);
        let html = env
            .get_template("connect.html")
            .unwrap()
            .render(&none)
            .unwrap();
        for tool in TOOLS {
            assert!(
                html.contains(&format!("id=\"tool-{tool}\"")),
                "the manual lacks {tool}"
            );
        }
        assert_eq!(
            html.matches("data-tool=").count(),
            TOOLS.len(),
            "no tool in the manual that the server does not offer"
        );
    }

    #[test]
    fn the_client_a_card_belongs_to_is_found_by_the_name_it_gave() {
        let app = App::new(false, None);
        app.saw_client("claude-code", "claude-code/2.1.0 (cli)");
        app.saw_client("claude-code", "");
        app.saw_client("RecallPort Web", "");
        let (name, _, calls) = seen_of(&app, Client::Claude).unwrap();
        assert_eq!((name.as_str(), calls), ("claude-code", 2));
        assert!(seen_of(&app, Client::Codex).is_none());
    }
}
