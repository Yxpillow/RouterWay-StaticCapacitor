use crate::cache::{FileCache, get_mime_type};
use crate::config::{Config, ApiConfig};
use anyhow::{Result, Context};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode, Uri};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tokio::time::{interval, Duration};
use tracing::{info, warn, error, debug};
use url::Url;
use percent_encoding::percent_decode_str;

pub struct HttpServer {
    config: Arc<Config>,
    cache: Arc<FileCache>,
}

impl HttpServer {
    pub fn new(config: Config) -> Result<Self> {
        let cache = Arc::new(FileCache::new(
            config.get_root_directory().clone(),
            config.get_max_cache_size()?,
            config.is_cache_enabled(),
        ));

        Ok(Self {
            config: Arc::new(config),
            cache,
        })
    }

    pub async fn start(&self) -> Result<()> {
        // 初始化文件缓存
        self.cache.initialize().await?;

        // 启动缓存清理任务
        if self.config.is_cache_enabled() {
            let cache_clone = Arc::clone(&self.cache);
            tokio::spawn(async move {
                let mut interval = interval(Duration::from_secs(300)); // 每5分钟清理一次
                loop {
                    interval.tick().await;
                    cache_clone.cleanup_old_entries(3600); // 清理1小时未访问的条目
                }
            });
        }

        let addr = SocketAddr::from(([0, 0, 0, 0], self.config.get_port()));
        
        let config = Arc::clone(&self.config);
        let cache = Arc::clone(&self.cache);

        let make_svc = make_service_fn(move |_conn| {
            let config = Arc::clone(&config);
            let cache = Arc::clone(&cache);
            
            async move {
                Ok::<_, Infallible>(service_fn(move |req| {
                    handle_request(req, Arc::clone(&config), Arc::clone(&cache))
                }))
            }
        });

        let server = Server::bind(&addr)
            .tcp_nodelay(true)
            .tcp_keepalive(Some(Duration::from_secs(60)))
            .serve(make_svc);

        info!("🚀 RouterWay 服务器启动成功!");
        info!("📍 监听地址: http://{}", addr);
        info!("📁 根目录: {}", self.config.get_root_directory().display());
        info!("💾 缓存状态: {}", if self.config.is_cache_enabled() { "启用" } else { "禁用" });
        info!("🔗 最大连接数: {}", self.config.get_max_connections());
        info!("📋 API配置数量: {}", self.config.get_api_configs().len());

        // 打印API配置信息
        for (i, api) in self.config.get_api_configs().iter().enumerate() {
            info!("  API {}: {} -> {} ({})", i + 1, api.from, api.to, api.name);
        }

        if let Err(e) = server.await {
            error!("服务器运行错误: {}", e);
            return Err(e.into());
        }

        Ok(())
    }
}

async fn handle_request(
    req: Request<Body>,
    config: Arc<Config>,
    cache: Arc<FileCache>,
) -> Result<Response<Body>, Infallible> {
    let method = req.method();
    let uri = req.uri();
    let path = uri.path();

    debug!("收到请求: {} {}", method, path);

    // 处理CORS预检请求
    if method == Method::OPTIONS {
        return Ok(create_cors_response(StatusCode::OK, Body::empty()));
    }

    // URL解码处理中文路径
    let decoded_path = match percent_decode_str(path).decode_utf8() {
        Ok(decoded) => decoded.to_string(),
        Err(_) => {
            warn!("无法解码路径: {}", path);
            return Ok(create_error_response(StatusCode::BAD_REQUEST, "Invalid path encoding"));
        }
    };

    // 检查API代理配置
    for api_config in config.get_api_configs() {
        if decoded_path.starts_with(&api_config.from) {
            return handle_proxy_request(req, api_config, &decoded_path, &config, &cache).await;
        }
    }

    // 处理静态文件请求
    match handle_static_file(&decoded_path, &config, &cache).await {
        Ok(response) => Ok(response),
        Err(e) => {
            error!("处理静态文件请求失败: {}", e);
            Ok(create_error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"))
        }
    }
}

async fn handle_proxy_request(
    mut req: Request<Body>,
    api_config: &ApiConfig,
    original_path: &str,
    config: &Config,
    cache: &FileCache,
) -> Result<Response<Body>, Infallible> {
    // 构建目标URL
    let target_path = original_path.replacen(&api_config.from, &api_config.to, 1);
    
    debug!("代理请求: {} -> {}", original_path, target_path);

    // 解析目标URL
    let target_url = match target_path.parse::<Uri>() {
        Ok(uri) => uri,
        Err(e) => {
            error!("无效的代理目标URL: {} - {}", target_path, e);
            return Ok(create_error_response(StatusCode::BAD_REQUEST, "Invalid proxy target"));
        }
    };

    // 更新请求URI
    *req.uri_mut() = target_url;

    // 创建HTTP客户端并发送请求
    let client = hyper::Client::new();
    match client.request(req).await {
        Ok(mut response) => {
            // 添加CORS头和Server头
            let headers = response.headers_mut();
            headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
            headers.insert("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS".parse().unwrap());
            headers.insert("Access-Control-Allow-Headers", "Content-Type, Authorization".parse().unwrap());
            headers.insert("Server", "RouterWay".parse().unwrap());
            
            Ok(response)
        }
        Err(e) => {
            error!("代理请求失败: {}", e);
            match handle_error_page(StatusCode::BAD_GATEWAY, config, cache).await {
                Ok(response) => Ok(response),
                Err(_) => Ok(create_error_response(StatusCode::BAD_GATEWAY, "Proxy request failed"))
            }
        }
    }
}

async fn handle_static_file(
    path: &str,
    config: &Config,
    cache: &FileCache,
) -> Result<Response<Body>> {
    // 快速路径标准化 - 避免字符串分配
    let normalized_path = match path {
        "/" | "" => "index.html",
        p if p.starts_with('/') => &p[1..],
        p => p,
    };

    // 快速安全检查 - 使用字节级检查避免字符串操作
    if normalized_path.contains("..") || normalized_path.contains("//") {
        return Ok(create_error_response(StatusCode::FORBIDDEN, "Access denied"));
    }

    // 优先从缓存获取 - 使用零拷贝
    if let Some(cached_file) = cache.get_fast(normalized_path) {
        debug!("从缓存返回文件: {}", normalized_path);
        
        // 零拷贝响应 - 直接使用Arc引用
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", &cached_file.mime_type)
            .header("Cache-Control", "public, max-age=3600")
            .header("Access-Control-Allow-Origin", "*")
            .header("Server", "RouterWay")
            .body(Body::from(cached_file.get_content()))?);
    }

    // 缓存未命中时的快速文件读取
    let file_path = config.get_root_directory().join(normalized_path);
    
    debug!("从文件系统读取: {}", file_path.display());

    match fs::read(&file_path).await {
        Ok(content) => {
            let mime_type = get_mime_type(normalized_path);
            
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", mime_type)
                .header("Cache-Control", "public, max-age=3600")
                .header("Access-Control-Allow-Origin", "*")
                .header("Server", "RouterWay")
                .body(Body::from(content))?)
        }
        Err(_) => {
            // 尝试返回404错误页面
            handle_error_page(StatusCode::NOT_FOUND, config, cache).await
        }
    }
}

async fn handle_error_page(
    status: StatusCode,
    config: &Config,
    cache: &FileCache,
) -> Result<Response<Body>> {
    let error_file = match status {
        StatusCode::NOT_FOUND => "404.html",
        StatusCode::BAD_REQUEST => "400.html",
        StatusCode::FORBIDDEN => "403.html",
        StatusCode::INTERNAL_SERVER_ERROR => "500.html",
        StatusCode::BAD_GATEWAY => "502.html",
        StatusCode::SERVICE_UNAVAILABLE => "503.html",
        _ => "error.html",
    };

    // 构建错误页面的相对路径（相对于Public目录）
    let error_relative_path = format!("Errors/{}", error_file);
    
    // 优先从缓存获取错误页面 - 使用零拷贝
    if let Some(cached_file) = cache.get_fast(&error_relative_path) {
        debug!("从缓存返回错误页面: {}", error_relative_path);
        
        return Ok(Response::builder()
            .status(status)
            .header("Content-Type", "text/html; charset=utf-8")
            .header("Access-Control-Allow-Origin", "*")
            .header("Server", "RouterWay")
            .body(Body::from(cached_file.get_content()))?);
    }

    // 缓存未命中时从文件系统读取
    let error_path = config.get_error_pages_directory().join(error_file);
    
    match fs::read(&error_path).await {
        Ok(content) => {
            Ok(Response::builder()
                .status(status)
                .header("Content-Type", "text/html; charset=utf-8")
                .header("Access-Control-Allow-Origin", "*")
                .header("Server", "RouterWay")
                .body(Body::from(content))?)
        }
        Err(_) => {
            // 如果错误页面也不存在，返回简单的错误信息
            let error_message = match status {
                StatusCode::NOT_FOUND => "404 - 页面未找到",
                StatusCode::INTERNAL_SERVER_ERROR => "500 - 内部服务器错误",
                StatusCode::FORBIDDEN => "403 - 访问被拒绝",
                _ => "发生错误",
            };

            Ok(create_error_response(status, error_message))
        }
    }
}

fn create_error_response(status: StatusCode, message: &str) -> Response<Body> {
    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>{} - RouterWay</title>
    <style>
        body {{ font-family: Arial, sans-serif; text-align: center; margin-top: 50px; }}
        .error {{ color: #e74c3c; }}
    </style>
</head>
<body>
    <h1 class="error">{}</h1>
    <p>{}</p>
    <hr>
    <small>RouterWay Server</small>
</body>
</html>"#,
        status.as_u16(),
        status.as_u16(),
        message
    );

    Response::builder()
        .status(status)
        .header("Content-Type", "text/html; charset=utf-8")
        .header("Access-Control-Allow-Origin", "*")
        .header("Server", "RouterWay")
        .body(Body::from(html))
        .unwrap()
}

fn create_cors_response(status: StatusCode, body: Body) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS")
        .header("Access-Control-Allow-Headers", "Content-Type, Authorization")
        .header("Server", "RouterWay")
        .body(body)
        .unwrap()
}