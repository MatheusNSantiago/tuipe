use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

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
        restrict_file(path)?;
        Ok(toml::from_str(&fs::read_to_string(path)?)?)
    }

    /// Escrever, sincronizar e só então renomear impede que uma queda de energia
    /// produza um TOML parcial. A configuração antiga vale até o rename final.
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().expect("configuration path has a parent");
        fs::create_dir_all(parent)?;
        restrict_directory(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        restrict_file(temporary.path())?;
        temporary.write_all(toml::to_string_pretty(self)?.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary.persist(path)?;
        restrict_file(path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    }
}

fn restrict_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn restrict_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn configuracao_e_diretorio_sao_privados() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("tuipe");
        let path = directory.join("config.toml");

        Preferences::default().save(&path).unwrap();

        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
