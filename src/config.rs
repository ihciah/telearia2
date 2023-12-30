use std::path::Path;

use serde::{Deserialize, Serialize};

// max torrent size is 1M
pub const MAX_TORRENT_SIZE: u32 = 1024 * 1024;
// to strip task name
pub const MAX_BRIFE_NAME_LEN: usize = 40;

pub trait Param<T> {
    fn param(&self) -> T;
}

impl<T: Clone> Param<T> for T {
    fn param(&self) -> T {
        self.clone()
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    pub aria2: Aria2Config,
    pub telegram: TelegramConfig,
    pub download: DownloadConfig,
}

impl Config {
    pub fn load_from<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let config_context = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&config_context)?;
        Ok(config)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Aria2Config {
    pub rpc_url: String,
    pub token: String,
    pub channel_buffer_size: Option<usize>,
    pub interval_secs: Option<u64>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct TelegramConfig {
    pub token: String,
    pub admins: Vec<i64>,
    pub subscribe_expire_secs: Option<u64>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct DownloadConfig {
    // name -> path
    pub magnet_dirs: Vec<DirConfig>,
    // name -> path
    pub torrent_dirs: Vec<DirConfig>,
    // name -> path
    pub link_dirs: Vec<DirConfig>,
    pub default_dir: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct DirConfig {
    pub name: String,
    pub path: String,
}

impl Param<Aria2Config> for Config {
    fn param(&self) -> Aria2Config {
        self.aria2.clone()
    }
}

impl Param<TelegramConfig> for Config {
    fn param(&self) -> TelegramConfig {
        self.telegram.clone()
    }
}

impl Param<DownloadConfig> for Config {
    fn param(&self) -> DownloadConfig {
        self.download.clone()
    }
}
