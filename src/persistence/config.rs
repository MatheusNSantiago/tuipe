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

pub struct LoadedPreferences {
    pub preferences: Preferences,
    pub quarantined: Option<PathBuf>,
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
    /// Preserva uma configuração inválida para diagnóstico e inicia com os
    /// padrões, sem impedir o usuário de abrir o aplicativo.
    pub fn load_recovering(path: &Path) -> Result<LoadedPreferences> {
        if !path.exists() {
            return Ok(LoadedPreferences {
                preferences: Self::default(),
                quarantined: None,
            });
        }
        restrict_file(path)?;
        let contents = fs::read_to_string(path)?;
        match toml::from_str(&contents) {
            Ok(preferences) => Ok(LoadedPreferences {
                preferences,
                quarantined: None,
            }),
            Err(_) => {
                let parent = path
                    .parent()
                    .expect("o caminho da configuração deve ter um diretório pai");
                let quarantine = parent.join(format!(
                    "config-corrompida-{}.toml",
                    chrono::Utc::now().timestamp_millis()
                ));
                fs::rename(path, &quarantine)?;
                restrict_file(&quarantine)?;
                sync_directory(parent)?;
                Ok(LoadedPreferences {
                    preferences: Self::default(),
                    quarantined: Some(quarantine),
                })
            }
        }
    }

    pub fn validate(path: &Path) -> Result<()> {
        if path.exists() {
            toml::from_str::<Self>(&fs::read_to_string(path)?)?;
        }
        Ok(())
    }

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
        let parent = path
            .parent()
            .expect("o caminho da configuração deve ter um diretório pai");
        fs::create_dir_all(parent)?;
        restrict_directory(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        restrict_file(temporary.path())?;
        temporary.write_all(toml::to_string_pretty(self)?.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary.persist(path)?;
        restrict_file(path)?;
        sync_directory(parent)?;
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

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::File::open(path)?.sync_all()?;
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

    #[test]
    fn configuracao_invalida_e_preservada_e_recuperada() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("config.toml");
        fs::write(&path, "isto não é toml = [").unwrap();

        let loaded = Preferences::load_recovering(&path).unwrap();

        assert_eq!(loaded.preferences.theme, Preferences::default().theme);
        let quarantine = loaded.quarantined.unwrap();
        assert!(quarantine.exists());
        assert!(!path.exists());
        assert_eq!(
            fs::read_to_string(quarantine).unwrap(),
            "isto não é toml = ["
        );
    }
}
