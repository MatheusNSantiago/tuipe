//! Pacotes de palavras, citações, temas e geração declarativos no estilo Monkeytype.

mod catalog;
mod generator;

pub use catalog::{ContentCatalog, Quote, Theme};
pub use generator::{UniformWordGenerator, WordGenerator};
