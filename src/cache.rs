use anyhow::{Result, Context};
use dashmap::DashMap;
use memmap2::Mmap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tracing::{info, warn, error};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct CachedFile {
    pub content: Arc<Vec<u8>>,
    pub mime_type: String,
    pub last_modified: u64,
    pub access_count: Arc<AtomicUsize>,
    pub last_access: Arc<AtomicU64>,
    pub size: usize,
}

impl CachedFile {
    pub fn new(content: Vec<u8>, mime_type: String, last_modified: u64) -> Self {
        let size = content.len();
        Self {
            content: Arc::new(content),
            mime_type,
            last_modified,
            access_count: Arc::new(AtomicUsize::new(0)),
            last_access: Arc::new(AtomicU64::new(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
            )),
            size,
        }
    }

    pub fn access(&self) -> Arc<Vec<u8>> {
        self.access_count.fetch_add(1, Ordering::Relaxed);
        self.last_access.store(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            Ordering::Relaxed
        );
        Arc::clone(&self.content)
    }

    // 新增：零拷贝内容获取
    pub fn get_content(&self) -> Vec<u8> {
        self.access_count.fetch_add(1, Ordering::Relaxed);
        self.last_access.store(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            Ordering::Relaxed
        );
        (*self.content).clone()
    }
}

pub struct FileCache {
    cache: DashMap<String, CachedFile>,
    total_size: AtomicU64,
    max_size: u64,
    root_path: PathBuf,
    enabled: bool,
}

impl FileCache {
    pub fn new(root_path: PathBuf, max_size: u64, enabled: bool) -> Self {
        Self {
            cache: DashMap::new(),
            total_size: AtomicU64::new(0),
            max_size,
            root_path,
            enabled,
        }
    }

    pub async fn initialize(&self) -> Result<()> {
        if !self.enabled {
            info!("文件缓存已禁用");
            return Ok(());
        }

        info!("开始初始化文件缓存...");
        let mut loaded_count = 0;
        let mut total_size = 0;

        // 遍历根目录下的所有文件
        for entry in WalkDir::new(&self.root_path)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                let path = entry.path();
                
                // 跳过隐藏文件和临时文件
                if let Some(filename) = path.file_name() {
                    let filename_str = filename.to_string_lossy();
                    if filename_str.starts_with('.') || filename_str.ends_with('~') {
                        continue;
                    }
                }

                match self.load_file_to_cache(path).await {
                    Ok(size) => {
                        loaded_count += 1;
                        total_size += size;
                        
                        // 检查缓存大小限制
                        if total_size > self.max_size {
                            warn!("缓存大小超过限制，停止加载更多文件");
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("加载文件到缓存失败 {}: {}", path.display(), e);
                    }
                }
            }
        }

        info!("文件缓存初始化完成: {} 个文件, 总大小: {} MB", 
              loaded_count, total_size / 1024 / 1024);
        
        Ok(())
    }

    async fn load_file_to_cache(&self, file_path: &Path) -> Result<u64> {
        let metadata = fs::metadata(file_path).await
            .with_context(|| format!("无法获取文件元数据: {}", file_path.display()))?;

        let file_size = metadata.len();
        
        // 检查单个文件大小限制（不缓存超过10MB的文件）
        if file_size > 10 * 1024 * 1024 {
            return Ok(0);
        }

        // 检查总缓存大小
        let current_total = self.total_size.load(Ordering::Relaxed);
        if current_total + file_size > self.max_size {
            return Ok(0);
        }

        let content = fs::read(file_path).await
            .with_context(|| format!("无法读取文件: {}", file_path.display()))?;

        let mime_type = mime_guess::from_path(file_path)
            .first_or_octet_stream()
            .to_string();

        let last_modified = metadata
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH)
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // 生成相对路径作为缓存键
        let cache_key = file_path
            .strip_prefix(&self.root_path)
            .unwrap_or(file_path)
            .to_string_lossy()
            .replace('\\', "/");

        let cached_file = CachedFile::new(content, mime_type, last_modified);
        let actual_size = cached_file.size as u64;

        self.cache.insert(cache_key, cached_file);
        self.total_size.fetch_add(actual_size, Ordering::Relaxed);

        Ok(actual_size)
    }

    pub fn get(&self, path: &str) -> Option<CachedFile> {
        if !self.enabled {
            return None;
        }

        // 标准化路径
        let normalized_path = if path.starts_with('/') {
            &path[1..]
        } else {
            path
        };

        // 处理根路径
        let cache_key = if normalized_path.is_empty() || normalized_path == "/" {
            "index.html"
        } else {
            normalized_path
        };

        self.cache.get(cache_key).map(|entry| entry.clone())
    }

    // 新增：快速缓存获取方法
    pub fn get_fast(&self, path: &str) -> Option<CachedFile> {
        if !self.enabled {
            return None;
        }

        // 快速路径标准化
        let cache_key = match path {
            "/" | "" => "index.html",
            p if p.starts_with('/') => &p[1..],
            p => p,
        };

        self.cache.get(cache_key).map(|entry| entry.clone())
    }

    // 新增：异步插入缓存方法
    pub async fn insert_async(&self, path: String, content: Vec<u8>, mime_type: String) {
        if !self.enabled || content.len() > 10 * 1024 * 1024 {
            return;
        }

        let current_total = self.total_size.load(Ordering::Relaxed);
        if current_total + content.len() as u64 > self.max_size {
            return;
        }

        let last_modified = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let cached_file = CachedFile::new(content, mime_type, last_modified);
        let file_size = cached_file.size as u64;

        self.cache.insert(path, cached_file);
        self.total_size.fetch_add(file_size, Ordering::Relaxed);
    }

    pub fn get_stats(&self) -> (usize, u64, u64) {
        let count = self.cache.len();
        let total_size = self.total_size.load(Ordering::Relaxed);
        let max_size = self.max_size;
        (count, total_size, max_size)
    }

    pub fn cleanup_old_entries(&self, max_age_seconds: u64) {
        if !self.enabled {
            return;
        }

        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut removed_count = 0;
        let mut freed_size = 0u64;

        self.cache.retain(|_key, cached_file| {
            let last_access = cached_file.last_access.load(Ordering::Relaxed);
            let age = current_time.saturating_sub(last_access);
            
            if age > max_age_seconds {
                freed_size += cached_file.size as u64;
                removed_count += 1;
                false
            } else {
                true
            }
        });

        if removed_count > 0 {
            self.total_size.fetch_sub(freed_size, Ordering::Relaxed);
            info!("清理了 {} 个过期缓存条目，释放 {} MB", 
                  removed_count, freed_size / 1024 / 1024);
        }
    }
}

pub fn get_mime_type(file_path: &str) -> &'static str {
    let path = Path::new(file_path);
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css",
        Some("js") => "application/javascript",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("txt") => "text/plain",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("xml") => "application/xml",
        _ => "application/octet-stream",
    }
}