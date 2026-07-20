//! Configuração XDG e um pequeno repositório SQLite com migrações.

mod config;
mod raw_events;
mod repository;

pub use config::{LoadedPreferences, Preferences, paths};
pub use raw_events::{RawEvent, RawEventCodec, RawSessionEnd};
pub use repository::{
    MechanicObservationRecord, PersonalBaselineProfile, PriorityPattern, PriorityWord,
    RebuildReport, Repository, SessionKind, SessionProvenance, SessionSummary, StatisticsOverview,
    WordObservationRecord,
};
