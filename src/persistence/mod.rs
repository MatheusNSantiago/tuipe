//! Configuração XDG, schema SQLite atual e persistência dos eventos brutos.

mod config;
mod observations;
mod raw_events;
mod repository;

pub use config::{Keymap, LoadedPreferences, Preferences, paths, state_dir};
pub use observations::derive_word_observations;
pub use raw_events::{RawEvent, RawEventCodec, RawEventKind, RawSessionEnd};
pub use repository::{
    ActivityDay, AdaptivePolicyState, MechanicObservationRecord, OpenedRepository,
    PatternObservationRecord, PersonalBaselineProfile, PriorityPattern, PriorityWord,
    RebuildReport, Repository, SessionDetail, SessionHistoryItem, SessionKind, SessionOutcome,
    SessionProvenance, SessionSummary, SessionWordDiagnostic, StatisticsOverview,
    WordAttemptSummary, WordDetail, WordObservationRecord, WpmBucket,
};
