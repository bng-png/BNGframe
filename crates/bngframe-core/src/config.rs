use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const DEFAULT_PORT: u16 = 17832;
const DEFAULT_EELOG_REL: &str =
    ".local/share/Steam/steamapps/compatdata/230410/pfx/drive_c/users/steamuser/AppData/Local/Warframe/EE.log";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub bind: String,
    pub port: u16,
    pub eelog_path: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub web_dir: Option<PathBuf>,
    pub ocr_lang: String,
    pub inventory_consent: bool,
    pub wfmarket_jwt: Option<String>,
    pub monitor: Option<String>,
    pub overlay_enabled: bool,
    pub auto_open_browser: bool,
    /// UI / overlay language: "ru" or "en"
    pub ui_lang: String,
}

impl Default for Config {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| home.join(".local/share"))
            .join("bngframe");
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| home.join(".cache"))
            .join("bngframe");
        Self {
            bind: "127.0.0.1".into(),
            port: DEFAULT_PORT,
            eelog_path: home.join(DEFAULT_EELOG_REL),
            data_dir,
            cache_dir,
            web_dir: None,
            ocr_lang: "eng".into(),
            inventory_consent: false,
            wfmarket_jwt: None,
            monitor: None,
            overlay_enabled: true,
            auto_open_browser: false,
            ui_lang: "ru".into(),
        }
    }
}

impl Config {
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".config"))
            .join("bngframe")
            .join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if path.exists() {
            let text = fs::read_to_string(&path)
                .with_context(|| format!("read config {}", path.display()))?;
            let cfg: Config = toml::from_str(&text).context("parse config.toml")?;
            cfg.ensure_dirs()?;
            Ok(cfg)
        } else {
            let cfg = Config::default();
            cfg.save()?;
            cfg.ensure_dirs()?;
            Ok(cfg)
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        fs::write(&path, text).with_context(|| format!("write config {}", path.display()))?;
        Ok(())
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(&self.data_dir)?;
        fs::create_dir_all(&self.cache_dir)?;
        Ok(())
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("bngframe.db")
    }

    pub fn listen_addr(&self) -> String {
        format!("{}:{}", self.bind, self.port)
    }

    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.bind, self.port)
    }

    pub fn resolve_web_dir(&self) -> Option<PathBuf> {
        if let Some(ref p) = self.web_dir {
            if p.exists() {
                return Some(p.clone());
            }
        }
        let candidates = [
            PathBuf::from("web/dist"),
            PathBuf::from("../web/dist"),
            PathBuf::from("../../web/dist"),
        ];
        for c in candidates {
            if c.exists() {
                return Some(c);
            }
        }
        None
    }

    pub fn eelog_exists(&self) -> bool {
        Path::new(&self.eelog_path).exists()
    }
}
