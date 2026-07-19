//! Configuração XDG e um pequeno repositório SQLite com migrações.

mod config;
mod raw_events;
mod repository;

pub use config::{Preferences, paths};
pub use raw_events::{RawEvent, RawEventCodec, RawSessionEnd};
pub use repository::{
    MechanicObservationRecord, PersonalBaselineProfile, PriorityWord, Repository, SessionKind,
    SessionSummary, StatisticsOverview, WordObservationRecord,
};
