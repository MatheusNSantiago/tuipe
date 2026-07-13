use serde::{Deserialize, Serialize};

/// Subconjunto deliberadamente pequeno de modos do Monkeytype suportado pelo tuipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestMode {
    Time { seconds: u16 },
    Words { count: u16 },
    Quote,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteLength {
    #[default]
    All,
    Short,
    Medium,
    Long,
}

/// Corresponde às três semânticas de dificuldade do Monkeytype.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Difficulty {
    Normal,
    Expert,
    Master,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestConfig {
    pub mode: TestMode,
    #[serde(default)]
    pub quote_length: QuoteLength,
    pub difficulty: Difficulty,
    pub punctuation: bool,
    pub numbers: bool,
    pub adaptive: bool,
    pub language: String,
    pub word_pack: String,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            mode: TestMode::Time { seconds: 30 },
            quote_length: QuoteLength::All,
            difficulty: Difficulty::Expert,
            punctuation: false,
            numbers: false,
            adaptive: true,
            language: "portuguese".into(),
            word_pack: "common".into(),
        }
    }
}
