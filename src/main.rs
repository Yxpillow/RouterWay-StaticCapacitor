use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info, warn};
use tracing_subscriber;

mod cache;
mod config;
mod server;

use server::HttpServer;

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    info!("🚀 启动 RouterWay 高性能服务器...");

    // 加载配置
    let config = config::Config::load_from_file("config.toml")
        .context("加载配置文件失败")?;

    // 创建并启动服务器
    let server = HttpServer::new(config)?;
    
    // 设置优雅关闭
    let shutdown_signal = async {
        tokio::signal::ctrl_c()
            .await
            .expect("无法安装 Ctrl+C 处理器");
        info!("收到关闭信号，正在优雅关闭服务器...");
    };

    tokio::select! {
        result = server.start() => {
            if let Err(e) = result {
                error!("服务器运行错误: {}", e);
                return Err(e);
            }
        }
        _ = shutdown_signal => {
            info!("服务器已关闭");
        }
    }

    Ok(())
}