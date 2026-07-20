//! Configuração XDG e um pequeno repositório SQLite com migrações.

mod config;
mod raw_events;
mod repository;

pub use config::{Keymap, LoadedPreferences, Preferences, paths};
pub use raw_events::{RawEvent, RawEventCodec, RawSessionEnd};
pub use repository::{
    ActivityDay, MechanicObservationRecord, OpenedRepository, PersonalBaselineProfile,
    PriorityPattern, PriorityWord, RebuildReport, Repository, SessionDetail, SessionHistoryItem,
    SessionKind, SessionOutcome, SessionProvenance, SessionSummary, SessionWordDiagnostic,
    StatisticsOverview, WordAttemptSummary, WordDetail, WordObservationRecord, WpmBucket,
};
