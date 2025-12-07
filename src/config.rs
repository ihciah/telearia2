use std::path::Path;

use serde::Deserialize;

use crate::utils::SingleMultiMap;

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

pub type Aria2ConfigGroup = SingleMultiMap<Aria2Config>;

#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    pub aria2: Aria2ConfigGroup,
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

#[derive(Deserialize, Clone, Debug)]
pub struct Aria2Config {
    pub rpc_url: String,
    pub token: String,
    pub channel_buffer_size: Option<usize>,
    pub interval_secs: Option<u64>,
    pub admins_override: Option<Vec<i64>>,
    pub download_override: Option<DownloadConfig>,
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

impl Param<Aria2ConfigGroup> for Config {
    fn param(&self) -> Aria2ConfigGroup {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_aria2_config() {
        let toml = r#"
[aria2]
rpc_url = "wss://example.org/jsonrpc"
token = "secret"

[telegram]
token = "bot_token"
admins = [123, 456]

[download]
magnet_dirs = []
torrent_dirs = []
link_dirs = []
default_dir = "/data"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(matches!(config.aria2, crate::utils::SingleMultiMap::Single(_)));
        assert_eq!(config.telegram.admins, vec![123, 456]);
        assert_eq!(config.download.default_dir, "/data");
    }

    #[test]
    fn test_parse_multi_aria2_config() {
        let toml = r#"
[aria2.server1]
rpc_url = "wss://server1.org/jsonrpc"
token = "secret1"

[aria2.server2]
rpc_url = "wss://server2.org/jsonrpc"
token = "secret2"

[telegram]
token = "bot_token"
admins = [123]

[download]
magnet_dirs = []
torrent_dirs = []
link_dirs = []
default_dir = "/data"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(matches!(config.aria2, crate::utils::SingleMultiMap::Multi(_)));
    }

    #[test]
    fn test_parse_dir_config() {
        let toml = r#"
[aria2]
rpc_url = "wss://example.org/jsonrpc"
token = "secret"

[telegram]
token = "bot_token"
admins = []

[download]
magnet_dirs = [
    { name = "Movies", path = "/data/movies" },
    { name = "Music", path = "/data/music" },
]
torrent_dirs = []
link_dirs = []
default_dir = "/data"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.download.magnet_dirs.len(), 2);
        assert_eq!(config.download.magnet_dirs[0].name, "Movies");
        assert_eq!(config.download.magnet_dirs[0].path, "/data/movies");
    }
}
