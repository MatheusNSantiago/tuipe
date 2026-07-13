//! Estado de entrada e métricas de resultado compatíveis com o Monkeytype.

mod config;
mod engine;
mod metrics;
mod word;

pub use config::{Difficulty, QuoteLength, TestConfig, TestMode};
pub use engine::{InputEvent, KeyAction, TestEngine, TestStatus, Transition};
pub use metrics::{CharacterStats, Metrics};
pub use word::{CommitCharacter, TargetWord, WordAttempt};
