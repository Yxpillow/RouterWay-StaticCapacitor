use anyhow::{Result, Context};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    pub name: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub port: u16,
    pub name: String,
    pub max_cache_size: String,
    pub cache_enabled: bool,
    pub max_connections: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticConfig {
    pub root_directory: PathBuf,
    pub error_pages_directory: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(rename = "static")]
    pub static_config: StaticConfig,
    pub api: Vec<ApiConfig>,
}

impl Config {
    pub fn load_from_file(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("无法读取配置文件: {}", path))?;
        
        let mut config: Config = toml::from_str(&content)
            .with_context(|| format!("配置文件格式错误: {}", path))?;
        
        // 解析缓存大小
        let cache_size = Self::parse_cache_size(&config.server.max_cache_size)?;
        
        info!("配置加载完成:");
        info!("  端口: {}", config.server.port);
        info!("  服务器名称: {}", config.server.name);
        info!("  根目录: {}", config.static_config.root_directory.display());
        info!("  错误页面目录: {}", config.static_config.error_pages_directory.display());
        info!("  最大缓存: {} 字节", cache_size);
        info!("  缓存启用: {}", config.server.cache_enabled);
        info!("  最大连接数: {}", config.server.max_connections);
        info!("  API配置数量: {}", config.api.len());
        
        for (i, api) in config.api.iter().enumerate() {
            info!("  API[{}]: {} -> {} ({})", i, api.from, api.to, api.name);
        }
        
        Ok(config)
    }
    
    pub fn get_port(&self) -> u16 {
        self.server.port
    }
    
    pub fn get_server_name(&self) -> &str {
        &self.server.name
    }
    
    pub fn get_root_directory(&self) -> &PathBuf {
        &self.static_config.root_directory
    }
    
    pub fn get_error_pages_directory(&self) -> &PathBuf {
        &self.static_config.error_pages_directory
    }
    
    pub fn get_max_cache_size(&self) -> Result<u64> {
        Self::parse_cache_size(&self.server.max_cache_size)
    }
    
    pub fn is_cache_enabled(&self) -> bool {
        self.server.cache_enabled
    }
    
    pub fn get_max_connections(&self) -> usize {
        self.server.max_connections
    }
    
    pub fn get_api_configs(&self) -> &Vec<ApiConfig> {
        &self.api
    }

    fn parse_cache_size(value: &str) -> Result<u64> {
        let value = value.to_lowercase();
        
        if let Some(stripped) = value.strip_suffix("kb") {
            let num: u64 = stripped.trim().parse()
                .with_context(|| format!("无效的缓存大小: {}", value))?;
            Ok(num * 1024)
        } else if let Some(stripped) = value.strip_suffix("mb") {
            let num: u64 = stripped.trim().parse()
                .with_context(|| format!("无效的缓存大小: {}", value))?;
            Ok(num * 1024 * 1024)
        } else if let Some(stripped) = value.strip_suffix("gb") {
            let num: u64 = stripped.trim().parse()
                .with_context(|| format!("无效的缓存大小: {}", value))?;
            Ok(num * 1024 * 1024 * 1024)
        } else if let Some(stripped) = value.strip_suffix("k") {
            let num: u64 = stripped.trim().parse()
                .with_context(|| format!("无效的缓存大小: {}", value))?;
            Ok(num * 1024)
        } else if let Some(stripped) = value.strip_suffix("m") {
            let num: u64 = stripped.trim().parse()
                .with_context(|| format!("无效的缓存大小: {}", value))?;
            Ok(num * 1024 * 1024)
        } else if let Some(stripped) = value.strip_suffix("g") {
            let num: u64 = stripped.trim().parse()
                .with_context(|| format!("无效的缓存大小: {}", value))?;
            Ok(num * 1024 * 1024 * 1024)
        } else {
            // 默认按字节处理
            value.trim().parse()
                .with_context(|| format!("无效的缓存大小: {}", value))
        }
    }
}