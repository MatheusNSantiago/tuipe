//! Configuração XDG e um pequeno repositório SQLite com migrações.

mod config;
mod observations;
mod raw_events;
mod repository;

pub use config::{Keymap, LoadedPreferences, Preferences, paths};
pub use observations::derive_word_observations;
pub use raw_events::{RawEvent, RawEventCodec, RawEventKind, RawSessionEnd};
pub use repository::{
    ActivityDay, MechanicObservationRecord, OpenedRepository, PersonalBaselineProfile,
    PriorityPattern, PriorityWord, RebuildReport, Repository, SessionDetail, SessionHistoryItem,
    SessionKind, SessionOutcome, SessionProvenance, SessionSummary, SessionWordDiagnostic,
    StatisticsOverview, WordAttemptSummary, WordDetail, WordObservationRecord, WpmBucket,
};
