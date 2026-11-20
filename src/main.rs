// The release build is a Windows GUI program: no console window, a tray icon keeps it running.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[macro_use]
mod log;
mod app;
mod archive;
mod clients;
mod detail;
mod embedding;
mod entities;
mod files;
mod jobs;
mod kv;
mod mcp;
mod memory;
mod shell;
mod store;
#[cfg(test)]
mod tests;
mod web;
mod web_admin;

use anyhow::{Context, Result, bail, ensure};
use app::App;
use clap::Parser;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};
use store::{LaneKind, Store};

const DEFAULT_PORT: u16 = 3765;

#[derive(Parser)]
#[command(version, about = "RecallPort：本地 AI 长期记忆服务")]
struct Args {
    /// 监听地址，仅限回环地址。默认 127.0.0.1:3765；没有指定且被占用时，自动改用相邻的空闲端口
    #[arg(long)]
    listen: Option<SocketAddr>,
    /// 记忆库位置，默认是程序目录下的 data
    #[arg(long)]
    data: Option<PathBuf>,
    /// 语义模型位置，默认是程序目录下的 model
    #[arg(long)]
    model: Option<PathBuf>,
    /// 把备份（ZIP）导入空库后退出
    #[arg(long, value_name = "BACKUP")]
    import: Option<PathBuf>,
    /// 后台运行：启动时不自动打开管理页（开机自启使用）
    #[arg(long)]
    background: bool,
    /// 不显示托盘图标
    #[arg(long)]
    no_tray: bool,
}

fn main() {
    shell::attach_console();
    let args = Args::parse();
    if let Err(error) = run(args) {
        log!("启动失败：{error:#}");
        shell::fatal(&format!("{error:#}"));
        std::process::exit(1);
    }
}

/// Is a RecallPort already answering on this address?
fn is_recallport(addr: SocketAddr) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(400)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let request = format!("GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    let mut reply = String::new();
    stream.write_all(request.as_bytes()).is_ok()
        && stream.read_to_string(&mut reply).is_ok()
        && reply.contains("200")
        && reply.trim_end().ends_with("ok")
}

fn bind(want: SocketAddr, explicit: bool) -> Result<TcpListener> {
    match TcpListener::bind(want) {
        Ok(listener) => Ok(listener),
        Err(error) if explicit => {
            bail!("无法监听 {want}：{error}。端口可能被其他程序占用，请用 --listen 指定其他端口。")
        }
        Err(_) => {
            for port in want.port() + 1..=want.port() + 20 {
                if let Ok(listener) = TcpListener::bind(SocketAddr::new(want.ip(), port)) {
                    log!("端口 {} 被占用，改用 {port}", want.port());
                    return Ok(listener);
                }
            }
            bail!(
                "端口 {} 被其他程序占用，附近的端口也不可用。请关闭占用它的程序，或用 --listen 指定其他端口。",
                want.port()
            )
        }
    }
}

fn open_library(model: &std::path::Path) -> Result<Arc<dyn embedding::Embed>> {
    Ok(Arc::new(embedding::Granite::open(model).with_context(
        || {
            format!(
                "无法加载本地语义模型：{}。请确认 model 文件夹与 RecallPort.exe 在一起。",
                model.display()
            )
        },
    )?))
}

fn import(path: PathBuf, dir: PathBuf, model: PathBuf) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let store = Arc::new(Store::open(&dir, Some(open_library(&model)?)).await?);
        ensure!(
            store.all().await?.is_empty(),
            "导入仅允许写入空库，请在网页的「数据」页使用导入功能"
        );
        let info = store.restore_zip(path, jobs::Job::detached()).await?;
        store.maintain(Some(LaneKind::All)).await?;
        println!("已导入 {} 条记忆：{}", info.memories, dir.display());
        Ok(())
    })
}

fn run(args: Args) -> Result<()> {
    let exe = std::env::current_exe()?;
    let root = exe.parent().context("无法找到程序目录")?.to_path_buf();
    let dir = std::path::absolute(args.data.clone().unwrap_or_else(|| root.join("data")))?;
    std::fs::create_dir_all(&dir)?;
    log::init(&dir);
    let model = args.model.clone().unwrap_or_else(|| root.join("model"));
    if let Some(path) = args.import.clone() {
        return import(path, dir, model);
    }
    let explicit = args.listen.is_some();
    let want = args
        .listen
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)));
    ensure!(
        want.ip().is_loopback(),
        "RecallPort 无认证，仅允许监听回环地址"
    );
    // Double-clicking again just brings up the page of the copy that is already running.
    if is_recallport(want) {
        if !args.background {
            shell::open_url(&format!("http://{want}/"));
        }
        return Ok(());
    }
    let listener = bind(want, explicit)?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}/");
    let app = App::new(shell::SUPPORTED && !args.no_tray, Some(dir.clone()));

    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let server = {
        let app = app.clone();
        std::thread::Builder::new()
            .name("server".into())
            .spawn(move || serve(listener, dir, model, app, ready_tx))?
    };
    // Whatever ends the server also ends the tray.
    let watcher = std::thread::spawn(move || {
        let result = server.join();
        shell::close_tray();
        result
    });
    let background = args.background;
    let announce = {
        let url = url.clone();
        move || match ready_rx.recv() {
            Ok(Ok(())) => {
                shell::set_tooltip(&format!("RecallPort · {addr}"));
                if !background {
                    shell::open_url(&url);
                }
            }
            Ok(Err(message)) => {
                shell::fatal(&message);
                shell::close_tray();
            }
            Err(_) => shell::close_tray(),
        }
    };
    if app.tray {
        std::thread::spawn(announce);
        let (open, quit) = (url.clone(), app.clone());
        shell::run_tray(
            move || shell::open_url(&open),
            move || quit.quit.notify_one(),
        )?;
        app.quit.notify_one();
    } else {
        announce();
    }
    match watcher.join() {
        Ok(Ok(result)) => result,
        _ => bail!("服务意外停止，详情见数据目录里的 recallport.log"),
    }
}

fn friendly(error: &anyhow::Error) -> String {
    let text = format!("{error:#}");
    if text.contains("已被另一个 RecallPort 进程打开") {
        "RecallPort 已经在运行（可以在系统托盘里找到它）。".to_owned()
    } else {
        text
    }
}

fn serve(
    listener: TcpListener,
    dir: PathBuf,
    model: PathBuf,
    app: Arc<App>,
    ready: mpsc::Sender<Result<(), String>>,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let boot = async {
            let engine = open_library(&model)?;
            let store = Arc::new(Store::open(&dir, Some(engine)).await?);
            store.clean_temporary();
            if let Some(notice) = store.recover_interrupted().await? {
                log!("{notice}");
            }
            listener.set_nonblocking(true)?;
            let listener = tokio::net::TcpListener::from_std(listener)?;
            let router = web::router(store.clone(), listener.local_addr()?, app.clone())?;
            store.maintain(None).await?;
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    if let Err(error) = store.maintain(None).await {
                        log!("Index maintenance: {error:#}");
                    }
                    match store.compact().await {
                        Ok(true) => log!("Compacted the library files"),
                        Ok(false) => {}
                        Err(error) => log!("Compaction: {error:#}"),
                    }
                }
            });
            anyhow::Ok((listener, router))
        }
        .await;
        match boot {
            Err(error) => {
                log!("启动失败：{error:#}");
                let _ = ready.send(Err(friendly(&error)));
                Ok(())
            }
            Ok((listener, router)) => {
                let _ = ready.send(Ok(()));
                let addr = listener.local_addr()?;
                println!("RecallPort  http://{addr}/\nMCP         http://{addr}/mcp");
                log!("已启动 http://{addr}/");
                axum::serve(listener, router)
                    .with_graceful_shutdown(async move {
                        tokio::select! {
                            _ = app.quit.notified() => {}
                            _ = tokio::signal::ctrl_c() => {}
                        }
                    })
                    .await?;
                log!("已退出");
                Ok(())
            }
        }
    })
}
