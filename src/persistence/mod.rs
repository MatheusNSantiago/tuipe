//! Configuração XDG e um pequeno repositório SQLite com migrações.

mod config;
mod raw_events;
mod repository;

pub use config::{Preferences, paths};
pub use raw_events::{RawEvent, RawEventCodec};
pub use repository::{Repository, StatisticsOverview};
