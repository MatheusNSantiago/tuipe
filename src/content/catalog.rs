use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::typing::QuoteLength;

#[derive(Debug, Clone, Deserialize)]
struct LanguageAsset {
    name: String,
    words: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct QuoteAsset {
    quotes: Vec<Quote>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Quote {
    pub text: String,
    pub source: String,
    pub length: u16,
    pub id: u32,
}

impl QuoteLength {
    fn contains(self, quote: &Quote) -> bool {
        match self {
            Self::All => true,
            Self::Short => quote.length <= 100,
            Self::Medium => (101..=300).contains(&quote.length),
            Self::Long => quote.length >= 301,
        }
    }
}

/// Cores usadas pelos papéis visuais do tuipe. O schema espelha os campos
/// correspondentes do objeto `Theme` do Monkeytype.
#[derive(Debug, Clone, Deserialize)]
pub struct Theme {
    pub bg: String,
    pub main: String,
    pub caret: String,
    pub sub: String,
    #[serde(rename = "subAlt")]
    pub sub_alt: String,
    pub text: String,
    pub error: String,
    #[serde(rename = "errorExtra")]
    pub error_extra: String,
    #[serde(rename = "colorfulError")]
    pub colorful_error: String,
    #[serde(rename = "colorfulErrorExtra")]
    pub colorful_error_extra: String,
}

#[derive(Debug, Clone)]
pub struct ContentCatalog {
    word_packs: BTreeMap<(String, String), Vec<String>>,
    quotes: BTreeMap<String, Vec<Quote>>,
    themes: BTreeMap<String, Theme>,
}

impl ContentCatalog {
    pub fn bundled() -> Result<Self> {
        let mut catalog = Self {
            word_packs: BTreeMap::new(),
            quotes: BTreeMap::new(),
            themes: BTreeMap::new(),
        };

        for (language, pack, source) in bundled_word_packs() {
            let asset: LanguageAsset = serde_json::from_str(source)
                .with_context(|| format!("invalid bundled {language}/{pack} word pack"))?;
            let expected_name = if pack == "common" {
                language.to_owned()
            } else {
                format!("{language}_{pack}")
            };
            if asset.name != expected_name || asset.words.is_empty() {
                anyhow::bail!("invalid bundled {language}/{pack} word pack");
            }
            catalog
                .word_packs
                .insert((language.into(), pack.into()), asset.words);
        }

        for (language, source) in bundled_quotes() {
            let asset: QuoteAsset = serde_json::from_str(source)
                .with_context(|| format!("invalid bundled {language} quotes"))?;
            catalog.quotes.insert(language.into(), asset.quotes);
        }

        for (name, source) in bundled_themes() {
            let theme: Theme = serde_json::from_str(source)
                .with_context(|| format!("invalid bundled {name} theme"))?;
            catalog.themes.insert(name.into(), theme);
        }

        Ok(catalog)
    }

    pub fn word_pack(&self, language: &str, pack: &str) -> Option<&[String]> {
        self.word_packs
            .get(&(language.into(), pack.into()))
            .map(Vec::as_slice)
    }

    pub fn quotes(&self, language: &str, length: QuoteLength) -> Vec<&Quote> {
        self.quotes
            .get(language)
            .into_iter()
            .flatten()
            .filter(|quote| length.contains(quote))
            .collect()
    }

    pub fn theme(&self, name: &str) -> Option<&Theme> {
        self.themes.get(name)
    }

    pub fn theme_names(&self) -> impl Iterator<Item = &str> {
        self.themes.keys().map(String::as_str)
    }

    /// Temas fornecidos pelo usuário são arquivos TOML declarativos com o nome
    /// do tema. Os temas embarcados continuam disponíveis se um arquivo falhar.
    pub fn load_user_themes(&mut self, directory: &Path) -> Result<()> {
        if !directory.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "toml")
            {
                let source = fs::read_to_string(entry.path())?;
                let theme: Theme = toml::from_str(&source)
                    .with_context(|| format!("invalid custom theme {}", entry.path().display()))?;
                let name = entry
                    .path()
                    .file_stem()
                    .expect("theme extension implies a file stem")
                    .to_string_lossy()
                    .into_owned();
                self.themes.insert(name, theme);
            }
        }
        Ok(())
    }
}

fn bundled_word_packs() -> [(&'static str, &'static str, &'static str); 6] {
    [
        (
            "portuguese",
            "common",
            include_str!("../../assets/languages/portuguese_common.json"),
        ),
        (
            "portuguese",
            "1k",
            include_str!("../../assets/languages/portuguese_1k.json"),
        ),
        (
            "portuguese",
            "5k",
            include_str!("../../assets/languages/portuguese_5k.json"),
        ),
        (
            "english",
            "common",
            include_str!("../../assets/languages/english_common.json"),
        ),
        (
            "english",
            "1k",
            include_str!("../../assets/languages/english_1k.json"),
        ),
        (
            "english",
            "5k",
            include_str!("../../assets/languages/english_5k.json"),
        ),
    ]
}

fn bundled_quotes() -> [(&'static str, &'static str); 2] {
    [
        (
            "portuguese",
            include_str!("../../assets/quotes/portuguese.json"),
        ),
        ("english", include_str!("../../assets/quotes/english.json")),
    ]
}

fn bundled_themes() -> [(&'static str, &'static str); 10] {
    [
        ("arch", include_str!("../../assets/themes/arch.json")),
        (
            "serika_dark",
            include_str!("../../assets/themes/serika_dark.json"),
        ),
        ("serika", include_str!("../../assets/themes/serika.json")),
        (
            "catppuccin",
            include_str!("../../assets/themes/catppuccin.json"),
        ),
        ("dracula", include_str!("../../assets/themes/dracula.json")),
        ("nord", include_str!("../../assets/themes/nord.json")),
        (
            "gruvbox_dark",
            include_str!("../../assets/themes/gruvbox_dark.json"),
        ),
        (
            "rose_pine",
            include_str!("../../assets/themes/rose_pine.json"),
        ),
        (
            "solarized_dark",
            include_str!("../../assets/themes/solarized_dark.json"),
        ),
        ("monokai", include_str!("../../assets/themes/monokai.json")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_content_has_the_promised_quote_corpora() {
        let catalog = ContentCatalog::bundled().unwrap();
        assert_eq!(catalog.quotes("portuguese", QuoteLength::All).len(), 109);
        assert_eq!(catalog.quotes("english", QuoteLength::All).len(), 6_488);
        assert!(catalog.word_pack("portuguese", "5k").unwrap().len() >= 5_000);
        assert!(catalog.theme("arch").is_some());
    }

    #[test]
    fn quote_lengths_partition_the_complete_corpus() {
        let catalog = ContentCatalog::bundled().unwrap();
        let short = catalog.quotes("portuguese", QuoteLength::Short);
        let medium = catalog.quotes("portuguese", QuoteLength::Medium);
        let long = catalog.quotes("portuguese", QuoteLength::Long);

        assert!(short.iter().all(|quote| quote.length <= 100));
        assert!(
            medium
                .iter()
                .all(|quote| (101..=300).contains(&quote.length))
        );
        assert!(long.iter().all(|quote| quote.length >= 301));
        assert_eq!(short.len() + medium.len() + long.len(), 109);
    }
}
