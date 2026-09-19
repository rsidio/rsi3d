//! 本地配置：~/.rsi3d/config.json

use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;

fn default_base() -> String {
    "http://localhost:8282".to_string()
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CliConfig {
    #[serde(default = "default_base")]
    pub base_url: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            base_url: default_base(),
            token: None,
            email: None,
            name: None,
        }
    }
}

/// 配置路径：优先 RSI3D_CONFIG，否则 ~/.rsi3d/config.json
pub fn config_path() -> PathBuf {
    if let Ok(p) = env::var("RSI3D_CONFIG") {
        return PathBuf::from(p);
    }
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".rsi3d").join("config.json")
}

/// 读取配置；文件缺失或损坏时回落到默认值（fail-soft）。
pub fn load() -> CliConfig {
    match fs::read_to_string(config_path()) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => CliConfig::default(),
    }
}

/// 写入配置（自动创建父目录）。
pub fn save(cfg: &CliConfig) -> anyhow::Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, serde_json::to_string_pretty(cfg)?)?;
    Ok(())
}

/// 取 token，未登录时报错并给出指引。
pub fn require_token(cfg: &CliConfig) -> anyhow::Result<String> {
    cfg.token
        .clone()
        .ok_or_else(|| anyhow::anyhow!("尚未登录，请先运行: rsi3d login --email <邮箱> --password <密码>"))
}
