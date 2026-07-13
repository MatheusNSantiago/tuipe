use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::Result;
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

use crate::typing::TestConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    #[serde(default)]
    pub test: TestConfig,
    #[serde(default = "default_theme")]
    pub theme: String,
}

fn default_theme() -> String {
    "arch".into()
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            test: TestConfig::default(),
            theme: default_theme(),
        }
    }
}

pub fn paths() -> (PathBuf, PathBuf) {
    let base = BaseDirs::new().expect("tuipe requires a home directory");
    (
        base.config_dir().join("tuipe/config.toml"),
        base.data_dir().join("tuipe/tuipe.db"),
    )
}

impl Preferences {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        Ok(toml::from_str(&fs::read_to_string(path)?)?)
    }

    /// Escrever, sincronizar e só então renomear impede que uma queda de energia
    /// produza um TOML parcial. A configuração antiga vale até o rename final.
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().expect("configuration path has a parent");
        fs::create_dir_all(parent)?;
        let temporary = path.with_extension("toml.tmp");
        let mut file = fs::File::create(&temporary)?;
        file.write_all(toml::to_string_pretty(self)?.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(temporary, path)?;
        Ok(())
    }
}
