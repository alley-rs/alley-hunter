use std::path::PathBuf;
use std::process::exit;

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::RwLock;

use crate::error::{Error, HunterResult};
use crate::node::ServerNode;

const APP_NAME: &str = "hunter";

lazy_static! {
    pub static ref EXECUTABLE_DIR: PathBuf = dirs::cache_dir().unwrap().join(APP_NAME);
    pub static ref CONFIG_DIR: PathBuf = dirs::config_dir().unwrap().join(APP_NAME);
    pub static ref CONFIG_FILE_PATH: PathBuf = CONFIG_DIR.join("hunter.toml");
    pub static ref TROJAN_CONFIG_FILE_PATH: PathBuf = CONFIG_DIR.join("config.json");
    pub static ref AUTOSTART_DIR: PathBuf = if cfg!(target_os = "macos") {
        dirs::home_dir()
            .unwrap()
            .join("Library")
            .join("LaunchAgents")
    } else if cfg!(target_os = "windows") {
        dirs::data_dir()
            .unwrap()
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup")
    } else {
        dirs::config_dir().unwrap().join("autostart")
    };
    pub static ref CONFIG: RwLock<Config> = RwLock::new(match read_config_file() {
        Ok(c) => c,
        Err(e) => {
            error!(message = "读取配置文件出错", error = ?e);
            exit(1);
        }
    });
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TrojanConfig {
    remote_addr: String,
    remote_port: u16,
    password: Vec<String>,
}

impl From<&ServerNode> for TrojanConfig {
    fn from(value: &ServerNode) -> Self {
        Self {
            remote_addr: value.addr().to_owned(),
            remote_port: value.port(),
            password: vec![value.password().to_owned()],
        }
    }
}

impl TrojanConfig {
    pub fn remote_addr(&self) -> &str {
        &self.remote_addr
    }

    pub fn remote_port(&self) -> u16 {
        self.remote_port
    }

    pub fn password(&self) -> &str {
        &self.password[0]
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Into<i8> for LogLevel {
    fn into(self) -> i8 {
        match self {
            LogLevel::Trace => -1,
            LogLevel::Debug => 0,
            LogLevel::Info => 1,
            LogLevel::Warn => 2,
            LogLevel::Error => 3,
        }
    }
}

impl Default for LogLevel {
    fn default() -> Self {
        LogLevel::Info
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    local_addr: String,
    local_port: u16,
    #[serde(default)]
    log_level: LogLevel,
    pac: String,
    nodes: Vec<ServerNode>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            local_addr: "127.0.0.1".to_owned(),
            local_port: 1086,
            pac: "https://mirror.ghproxy.com/https://raw.githubusercontent.com/thep0y/pac/main/blacklist.pac".to_owned(),
            log_level: LogLevel::default(),
            nodes: Default::default(),
        }
    }
}

impl Config {
    /// 返回 None 时意味着 trojan 配置文件中的节点可能已在 config.nodes
    /// 中被删除，或被人为修改的无效节点
    pub async fn get_using_server_node(&self) -> HunterResult<Option<&ServerNode>> {
        let nodes = &self.nodes;

        match get_trojan_config().await? {
            None => Ok(None),
            Some(trojan_config) => Ok(nodes.iter().find(|node| {
                node.addr() == trojan_config.remote_addr()
                    && node.port() == trojan_config.remote_port()
                    && node.password() == trojan_config.password()
            })),
        }
    }

    pub fn set_local_addr(&mut self, local_addr: &str) {
        self.local_addr = local_addr.to_owned();
    }

    pub fn set_local_port(&mut self, local_port: u16) {
        self.local_port = local_port;
    }

    pub fn set_pac(&mut self, pac: &str) {
        self.pac = pac.to_owned();
    }

    // pub fn server_nodes(&self) -> &[ServerNode] {
    //     &self.nodes
    // }
    //
    // pub fn nodes(&self) -> &[ServerNode] {
    //     &self.nodes
    // }

    pub fn local_socks5_addr(&self) -> String {
        format!("socks5h://{}:{}", self.local_addr, self.local_port)
    }

    pub fn pac(&self) -> &str {
        &self.pac
    }

    pub async fn write_trojan_config_file(&self, server_node: &ServerNode) -> HunterResult<()> {
        let trojan_config = server_node.to_string(
            &self.local_addr,
            self.local_port,
            self.log_level.clone().into(),
        );

        fs::write(&*TROJAN_CONFIG_FILE_PATH, trojan_config)
            .await
            .map_err(|e|{
                error!(message = "写 trojan 配置失败", error = ?e, path = ?*TROJAN_CONFIG_FILE_PATH);
                e.into()
            })
    }

    pub fn update_server_node(&mut self, index: usize, server_node: ServerNode) {
        if index == self.nodes.len() {
            self.add_server_node(&server_node);
        } else {
            self.nodes[index] = server_node;
        }
    }

    pub fn add_server_node(&mut self, server_node: &ServerNode) {
        let node = self
            .nodes
            .iter()
            .find(|&node| node.name() == server_node.name() || node.addr() == server_node.addr());

        if node.is_some() {
            warn!(message = "节点已存在", server_node = ?node);
            // 节点已存在，不保存
            return;
        }

        self.nodes.push(server_node.to_owned());
    }

    /// 设置 node 前应先终止 trojan 进程
    pub async fn set_server_node(&self, name: &str) -> HunterResult<()> {
        let node = self.nodes.iter().find(|&node| node.name() == name);

        let node = match node {
            Some(n) => n,
            None => {
                error!(message = "节点不存在", server_node_name = name);
                return Err(Error::Config(format!("name [{}] 不存在", name).to_owned()));
            }
        };

        self.write_trojan_config_file(node).await?;

        Ok(())
    }

    pub fn set_log_level(&mut self, log_level: LogLevel) {
        self.log_level = log_level;
    }
}

fn read_config_file() -> HunterResult<Config> {
    if !CONFIG_FILE_PATH.exists() {
        info!("配置文件不存在，返回默认配置");
        return Ok(Config::default());
    }

    let data = std::fs::read(&*CONFIG_FILE_PATH).map_err(|e| {
        error!(message = "读取配置文件失败", error = ?e);
        e
    })?;

    let s = String::from_utf8(data).map_err(|e| {
        error!(message = "数组转字符串失败", error = ?e);

        e
    })?;

    let config: Config = toml::from_str(&s).map_err(|e| {
        error!(mesasge = "反序列化 hunter.toml 失败", error = ?e);

        Error::Toml(e.to_string())
    })?;

    info!(message = "已读取 hunter.toml", config = ?config);

    Ok(config)
}

pub async fn write_config_file(config: &Config) -> HunterResult<()> {
    let content = toml::to_string(config).map_err(|e| {
        error!(mesasge = "序列化 hunter.toml 失败", error = ?e);
        Error::Toml(e.to_string())
    })?;

    fs::write(&*CONFIG_FILE_PATH, content).await.map_err(|e| {
        error!(mesasge = "保存 hunter.toml 失败", error = ?e);

        e.into()
    })
}

pub async fn get_trojan_config() -> HunterResult<Option<TrojanConfig>> {
    if !TROJAN_CONFIG_FILE_PATH.exists() {
        return Ok(None);
    }

    let data = fs::read(&*TROJAN_CONFIG_FILE_PATH).await.map_err(|e| {
        error!(message = "读取 trojan 配置失败", error = ?e);
        e
    })?;

    let trojan_config: TrojanConfig = serde_json::from_slice(&data).map_err(|e| {
        error!(message = "反序列化 trojan 配置失败", error = ?e);
        e
    })?;

    Ok(Some(trojan_config))
}
