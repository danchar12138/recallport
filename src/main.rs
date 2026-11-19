mod embedding;
mod mcp;
mod memory;
mod store;
mod web;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use store::{Backup, LaneKind, Store};

#[derive(Parser)]
#[command(version, about = "RecallPort: portable personal AI memory")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:3765")]
    listen: SocketAddr,
    #[arg(long)]
    data: Option<PathBuf>,
    #[arg(long)]
    model: Option<PathBuf>,
    /// Restore a portable backup into an empty library, then exit.
    #[arg(long, value_name = "BACKUP")]
    import: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        args.listen.ip().is_loopback(),
        "RecallPort 无认证，仅允许监听回环地址"
    );
    let exe = std::env::current_exe()?;
    let root = exe.parent().context("无法找到程序目录")?;
    let model = args.model.unwrap_or_else(|| root.join("model"));
    let engine: Option<Arc<dyn embedding::Embed>> = Some(Arc::new(
        embedding::Granite::open(&model)
            .with_context(|| format!("无法加载本地模型：{}", model.display()))?,
    ));
    let dir = args.data.unwrap_or_else(|| root.join("data"));
    let dir = std::path::absolute(dir)?;
    let store = Arc::new(Store::open(&dir, engine).await?);
    if let Some(path) = args.import {
        ensure!(
            store.backup().await?.memories.is_empty(),
            "导入仅允许写入空库，请在 Web 中使用恢复功能替换已有库"
        );
        ensure!(
            std::fs::metadata(&path)?.len() <= store::MAX_BACKUP as u64,
            "备份文件过大"
        );
        let backup = Backup::parse(&std::fs::read(path)?)?;
        let count = backup.memories.len();
        store.replace(backup, false).await?;
        store.maintain(Some(LaneKind::All)).await?;
        println!("已导入 {count} 条记忆：{}", dir.display());
        return Ok(());
    }
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    let app = web::router(store.clone(), listener.local_addr()?)?;
    store.maintain(None).await?;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            if let Err(error) = store.maintain(None).await {
                eprintln!("Index maintenance: {error:#}");
            }
        }
    });
    println!(
        "RecallPort  http://{}/\nMCP         http://{}/mcp",
        listener.local_addr()?,
        listener.local_addr()?
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
