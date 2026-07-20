use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::Result;
use crokey::KeyCombination;
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

use crate::typing::TestConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    #[serde(default)]
    pub test: TestConfig,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub keymap: Keymap,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Keymap {
    #[serde(default = "default_next")]
    pub next: KeyCombination,
    #[serde(default = "default_repeat")]
    pub repeat: KeyCombination,
    #[serde(default = "default_statistics")]
    pub statistics: KeyCombination,
    #[serde(default = "default_statistics_global")]
    pub statistics_global: KeyCombination,
    #[serde(default = "default_favorite")]
    pub favorite: KeyCombination,
    #[serde(default = "default_quit")]
    pub quit: KeyCombination,
    #[serde(default = "default_settings")]
    pub settings: KeyCombination,
    #[serde(default = "default_cancel")]
    pub cancel: KeyCombination,
    #[serde(default = "default_delete_word")]
    pub delete_word: Vec<KeyCombination>,
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
            keymap: Keymap::default(),
        }
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            next: default_next(),
            repeat: default_repeat(),
            statistics: default_statistics(),
            statistics_global: default_statistics_global(),
            favorite: default_favorite(),
            quit: default_quit(),
            settings: default_settings(),
            cancel: default_cancel(),
            delete_word: default_delete_word(),
        }
    }
}

impl Keymap {
    pub fn validate(&self) -> Result<()> {
        let mut bindings = vec![
            ("próximo", self.next),
            ("repetir", self.repeat),
            ("estatísticas", self.statistics),
            ("estatísticas globais", self.statistics_global),
            ("favorito", self.favorite),
            ("sair", self.quit),
            ("configurações", self.settings),
            ("cancelar", self.cancel),
        ];
        anyhow::ensure!(
            !self.delete_word.is_empty(),
            "o atalho de apagar palavra precisa de ao menos uma combinação"
        );
        bindings.extend(
            self.delete_word
                .iter()
                .copied()
                .map(|binding| ("apagar palavra", binding)),
        );
        for (name, binding) in &bindings {
            anyhow::ensure!(
                binding.codes.len() == 1,
                "o atalho de {name} precisa ser uma combinação de uma tecla"
            );
        }
        for (index, (name, binding)) in bindings.iter().enumerate() {
            if let Some((other, _)) = bindings[..index]
                .iter()
                .find(|(_, candidate)| candidate == binding)
            {
                anyhow::bail!("os atalhos de {other} e {name} usam a mesma combinação");
            }
        }
        Ok(())
    }

    pub fn label(binding: KeyCombination) -> String {
        let raw = binding.to_string().to_lowercase();
        let parts = raw.split('-').collect::<Vec<_>>();
        let modifier_count = parts
            .iter()
            .take_while(|part| matches!(part, &&"ctrl" | &&"alt" | &&"shift" | &&"super"))
            .count();
        if modifier_count == 0 {
            raw
        } else {
            format!(
                "{}+{}",
                parts[..modifier_count].join("+"),
                parts[modifier_count..].join("-")
            )
        }
    }
}

fn binding(value: &str) -> KeyCombination {
    crokey::parse(value).expect("os atalhos padrão são válidos")
}

fn default_next() -> KeyCombination {
    binding("enter")
}

fn default_repeat() -> KeyCombination {
    binding("r")
}

fn default_statistics() -> KeyCombination {
    binding("s")
}

fn default_statistics_global() -> KeyCombination {
    binding("ctrl-s")
}

fn default_favorite() -> KeyCombination {
    binding("f")
}

fn default_quit() -> KeyCombination {
    binding("q")
}

fn default_settings() -> KeyCombination {
    binding("esc")
}

fn default_cancel() -> KeyCombination {
    binding("ctrl-c")
}

fn default_delete_word() -> Vec<KeyCombination> {
    vec![binding("ctrl-w"), binding("ctrl-backspace")]
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
        match toml::from_str::<Self>(&contents) {
            Ok(preferences) if preferences.keymap.validate().is_ok() => Ok(LoadedPreferences {
                preferences,
                quarantined: None,
            }),
            _ => {
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
            let preferences = toml::from_str::<Self>(&fs::read_to_string(path)?)?;
            preferences.keymap.validate()?;
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        restrict_file(path)?;
        let preferences: Self = toml::from_str(&fs::read_to_string(path)?)?;
        preferences.keymap.validate()?;
        Ok(preferences)
    }

    /// Escrever, sincronizar e só então renomear impede que uma queda de energia
    /// produza um TOML parcial. A configuração antiga vale até o rename final.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.keymap.validate()?;
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

    #[test]
    fn atalhos_personalizados_sao_persistidos_em_formato_legivel() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("config.toml");
        let mut preferences = Preferences::default();
        preferences.keymap.repeat = crokey::parse("ctrl-r").unwrap();

        preferences.save(&path).unwrap();
        let restored = Preferences::load(&path).unwrap();

        assert_eq!(restored.keymap.repeat, crokey::parse("ctrl-r").unwrap());
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains("repeat = \"Ctrl-r\"")
        );
    }

    #[test]
    fn conflito_de_atalhos_isola_a_configuracao() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("config.toml");
        fs::write(&path, "[keymap]\nnext = 'enter'\nrepeat = 'enter'\n").unwrap();

        let loaded = Preferences::load_recovering(&path).unwrap();

        assert!(loaded.quarantined.is_some());
        assert_eq!(loaded.preferences.keymap, Keymap::default());
    }
}
