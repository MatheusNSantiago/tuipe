use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
    fmt,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use anyhow::{Context, Result};
use chrono::{Datelike, Duration, Local, NaiveDate};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::adaptive::{
    CURRENT_POLICY_VERSION, MechanicSkill, NgramSkill, Observation, PersonalBaseline,
    ReachObservation, ReachProfile, ReviewState, SelectionSource, UNIFORM_POLICY_VERSION,
    WordSelection, WordSkill, correction_burden, lexical_ngrams,
};
use crate::gamification::{StreakState, XpState, award};
use crate::persistence::{
    RawEvent, RawEventCodec, RawEventKind, RawSessionEnd, derive_word_observations,
};
use crate::typing::{
    ExternalEvent, InputEvent, KeyAction, Metrics, RecordedInputKind, TestConfig, TestEngine,
    TestMode, TestStatus,
};

pub struct Repository {
    connection: Connection,
}

const BASELINE_LATENCY_WINDOW: usize = 2_048;

pub struct OpenedRepository {
    pub repository: Repository,
    pub quarantined: Option<PathBuf>,
}

#[derive(Debug)]
struct CorruptDatabase(String);

impl fmt::Display for CorruptDatabase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "banco SQLite corrompido: {}", self.0)
    }
}

impl Error for CorruptDatabase {}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionProvenance {
    pub seed: u64,
    pub stimuli: Vec<String>,
    pub selections: Vec<Option<WordSelection>>,
    pub policy_version: u16,
    pub shadow_stimuli: Vec<String>,
    pub shadow_selections: Vec<Option<WordSelection>>,
    pub shadow_policy_version: Option<u16>,
    pub kind: SessionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptivePolicyState {
    pub active_version: u16,
    pub fallback_version: u16,
    pub shadow_version: Option<u16>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub metrics: usize,
    pub observations: usize,
    pub words: usize,
    pub ngrams: usize,
    pub mechanics: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionKind {
    #[default]
    Practice,
    Assessment,
    Transfer,
    Retention,
    Repeat,
}

impl SessionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Practice => "practice",
            Self::Assessment => "assessment",
            Self::Transfer => "transfer",
            Self::Retention => "retention",
            Self::Repeat => "repeat",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatisticsOverview {
    pub completed_tests: u64,
    pub comparable_tests: u64,
    pub active_ms: u64,
    pub average_wpm: f64,
    pub average_accuracy: f64,
    pub best_wpm: f64,
    pub trend_tests: Vec<SessionSummary>,
    pub history: Vec<SessionHistoryItem>,
    pub distribution: Vec<WpmBucket>,
    pub daily_activity: Vec<ActivityDay>,
    pub priority_words: Vec<PriorityWord>,
    pub priority_patterns: Vec<PriorityPattern>,
    pub total_xp: u64,
    pub level: u64,
    pub streak: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOutcome {
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionHistoryItem {
    pub id: u64,
    pub created_at_unix_s: i64,
    pub outcome: SessionOutcome,
    pub elapsed_ms: u64,
    pub wpm: f64,
    pub accuracy: f64,
    pub raw_wpm: f64,
    pub correct_chars: u32,
    pub incorrect_chars: u32,
    pub extra_chars: u32,
    pub missed_chars: u32,
    pub config: TestConfig,
    pub kind: SessionKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionWordDiagnostic {
    pub word: String,
    pub confirmed_error: bool,
    pub corrected: bool,
    pub latency_ratio: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionDetail {
    pub session: SessionHistoryItem,
    pub stimuli: Vec<String>,
    pub observed_words: u32,
    pub clean_words: u32,
    pub corrected_words: u32,
    pub failed_words: u32,
    pub slow_words: u32,
    pub challenges: Vec<SessionWordDiagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WpmBucket {
    pub start: u32,
    pub end: u32,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityDay {
    pub date: NaiveDate,
    pub tests: u32,
    pub active_ms: u64,
    pub average_wpm: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub id: u64,
    pub elapsed_ms: u64,
    pub wpm: f64,
    pub accuracy: f64,
    pub raw_wpm: f64,
    pub correct_chars: u32,
    pub incorrect_chars: u32,
    pub extra_chars: u32,
    pub config: TestConfig,
    pub kind: SessionKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PriorityWord {
    pub language: String,
    pub word: String,
    pub difficulty: f64,
    pub confirmed_errors: f64,
    pub corrections: f64,
    pub observations: u32,
    pub effective_exposures: f64,
    pub uncorrected_error_rate: f64,
    pub corrected_error_rate: f64,
    pub correction_burden: f64,
    pub corrected_graphemes: f64,
    pub corrective_events: f64,
    pub correction_ms: f64,
    pub baseline_exposure_chance: f64,
    pub adaptive_exposure_chance: f64,
    pub estimated_exposure_uplift: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WordAttemptSummary {
    pub session_id: u64,
    pub observed_at_unix_s: i64,
    pub confirmed_error: bool,
    pub corrected: bool,
    pub corrections: u32,
    pub correction_ms: u64,
    pub milliseconds_per_grapheme: Option<f64>,
    pub latency_ratio: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WordDetail {
    pub priority: PriorityWord,
    pub personal_baseline_ms_per_grapheme: Option<f64>,
    pub median_ms_per_grapheme: Option<f64>,
    pub last_seen_unix_s: Option<i64>,
    pub relevant_sequences: Vec<String>,
    pub recent_attempts: Vec<WordAttemptSummary>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PriorityPattern {
    pub language: String,
    pub pattern: String,
    pub model_pattern: String,
    pub kind: &'static str,
    pub difficulty: f64,
    pub estimated_exposure_uplift: f64,
    pub effective_exposures: f64,
    pub uncorrected_error_rate: f64,
    pub corrected_error_rate: f64,
    pub distinct_words: usize,
}

struct PatternEvidence {
    exposures: f64,
    uncorrected: f64,
    corrected: f64,
    distinct_words: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct WordEvidenceSummary {
    attempts: u32,
    failures: u32,
    corrected_attempts: u32,
    corrected_graphemes: u64,
    correction_ms: u64,
}

/// Evidência consultável de uma palavra observada durante uma sessão terminal.
#[derive(Debug, Clone, PartialEq)]
pub struct WordObservationRecord {
    pub language: String,
    pub word: String,
    pub confirmed_error: bool,
    pub corrections: u32,
    pub active_ms: u64,
    pub afk_ms: u64,
    pub planning_ms: u64,
    pub fluent_ms: u64,
    pub correction_ms: u64,
    pub input_events: u16,
    pub corrective_events: u16,
    pub censored: bool,
    pub grapheme_count: u16,
    pub fast_success: bool,
    pub slow: bool,
    pub latency_ratio: Option<f64>,
    pub evidence_weight: f64,
    pub selection_source: Option<SelectionSource>,
    pub selection_propensity: Option<f64>,
    pub mechanics: Vec<MechanicObservationRecord>,
    pub patterns: Vec<PatternObservationRecord>,
}

fn adaptive_observation(record: &WordObservationRecord) -> Observation {
    Observation {
        confirmed_error: record.confirmed_error,
        corrected: record.corrections > 0,
        corrections: record.corrections,
        corrective_events: record.corrective_events,
        correction_ms: record.correction_ms,
        correction_burden: correction_burden(
            record.corrections,
            record.corrective_events,
            record.correction_ms,
            record.fluent_ms,
            record.grapheme_count,
        ),
        fast_success: record.fast_success,
        slow: record.slow,
        latency_ratio: record.latency_ratio,
        evidence_weight: record.evidence_weight,
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MechanicObservationRecord {
    pub mechanic: String,
    pub confirmed_error: bool,
    pub corrected: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PatternObservationRecord {
    pub pattern: String,
    pub confirmed_error: bool,
    pub corrected: bool,
    pub correction_burden: f64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct SharedEvidenceRecord {
    mechanics: Vec<MechanicObservationRecord>,
    patterns: Vec<PatternObservationRecord>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PersonalBaselineProfile {
    pub rates: PersonalBaseline,
    latency_samples: Vec<(u16, f64)>,
    uncorrected_samples: u64,
    corrected_samples: u64,
}

impl PersonalBaselineProfile {
    pub fn latency_ms_per_grapheme(&self, grapheme_count: u16) -> Option<f64> {
        let mut nearby = self
            .latency_samples
            .iter()
            .filter(|(length, _)| length.abs_diff(grapheme_count) <= 1)
            .map(|(_, value)| *value)
            .collect::<Vec<_>>();
        if nearby.len() < 8 {
            nearby = self
                .latency_samples
                .iter()
                .map(|(_, value)| *value)
                .collect();
        }
        if nearby.len() < 8 {
            return None;
        }
        nearby.sort_by(f64::total_cmp);
        Some(nearby[nearby.len() / 2])
    }

    fn observe_records(&mut self, records: &[WordObservationRecord]) {
        for record in records {
            if record.censored || record.evidence_weight <= 0.0 {
                continue;
            }
            self.observe_sample(
                record.active_ms,
                record.grapheme_count,
                record.confirmed_error,
                record.corrections,
            );
        }
        self.refresh_rates();
    }

    fn observe_sample(
        &mut self,
        active_ms: u64,
        grapheme_count: u16,
        confirmed_error: bool,
        corrections: u32,
    ) {
        if active_ms == 0 || grapheme_count == 0 {
            return;
        }
        self.latency_samples
            .push((grapheme_count, active_ms as f64 / f64::from(grapheme_count)));
        self.uncorrected_samples = self
            .uncorrected_samples
            .saturating_add(u64::from(confirmed_error));
        self.corrected_samples = self
            .corrected_samples
            .saturating_add(u64::from(!confirmed_error && corrections > 0));
    }

    fn refresh_rates(&mut self) {
        let prior = PersonalBaseline::default();
        let prior_strength = 24.0;
        let exposures = self.latency_samples.len() as f64;
        self.rates = PersonalBaseline {
            uncorrected_error_rate: (prior.uncorrected_error_rate * prior_strength
                + self.uncorrected_samples as f64)
                / (prior_strength + exposures),
            corrected_error_rate: (prior.corrected_error_rate * prior_strength
                + self.corrected_samples as f64)
                / (prior_strength + exposures),
        };
    }
}

impl Repository {
    /// Valida a estrutura atual, a integridade do SQLite e os blobs brutos sem
    /// alterar o banco.
    pub fn doctor(path: &Path) -> Result<()> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        validate_schema_version(&connection)?;
        let quick_check =
            connection.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))?;
        anyhow::ensure!(quick_check == "ok", "integridade do SQLite: {quick_check}");
        let mut statement = connection.prepare(
            "SELECT codec_version, uncompressed_size, blob FROM raw_events ORDER BY session_id",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let version = row.get::<_, u16>(0)?;
            let size = row.get::<_, i64>(1)?;
            let blob = row.get::<_, Vec<u8>>(2)?;
            let size =
                usize::try_from(size).context("tamanho negativo nos eventos brutos persistidos")?;
            RawEventCodec::decode(version, size, &blob)?;
        }
        let mut statement = connection.prepare("SELECT language, word, state FROM word_skill")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let language = row.get::<_, String>(0)?;
            let word = row.get::<_, String>(1)?;
            let state = row.get::<_, Vec<u8>>(2)?;
            WordSkill::decode(&state)
                .with_context(|| format!("decodificar habilidade de {language}/{word}"))?;
        }
        drop(rows);
        drop(statement);
        let _: XpState = load_state_from(&connection, "xp_state")?;
        let _: StreakState = load_state_from(&connection, "streak_state")?;
        let mut statement = connection.prepare(
            "SELECT id, stimuli_json, selections_json, shadow_stimuli_json,
                    shadow_selections_json, shadow_policy_version
             FROM sessions ORDER BY id",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let session_id = row.get::<_, i64>(0)?;
            let stimuli = serde_json::from_str::<Vec<String>>(&row.get::<_, String>(1)?)?;
            let selections =
                serde_json::from_str::<Vec<Option<WordSelection>>>(&row.get::<_, String>(2)?)?;
            let shadow_stimuli = serde_json::from_str::<Vec<String>>(&row.get::<_, String>(3)?)?;
            let shadow_selections =
                serde_json::from_str::<Vec<Option<WordSelection>>>(&row.get::<_, String>(4)?)?;
            let shadow_version = row.get::<_, Option<u16>>(5)?;
            validate_selection_trace(session_id, "ativa", &stimuli, &selections)?;
            validate_selection_trace(session_id, "shadow", &shadow_stimuli, &shadow_selections)?;
            anyhow::ensure!(
                shadow_version.is_some() == !shadow_stimuli.is_empty(),
                "sessão #{session_id}: versão e estímulos shadow divergem"
            );
        }
        Ok(())
    }

    /// Produz uma cópia consistente mesmo quando o banco usa WAL.
    pub fn backup(&self, destination: &Path) -> Result<()> {
        anyhow::ensure!(
            !destination.exists(),
            "o destino do backup já existe: {}",
            destination.display()
        );
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        self.connection
            .execute("VACUUM INTO ?1", [destination.to_string_lossy().as_ref()])?;
        restrict_file(destination)?;
        Ok(())
    }

    pub fn is_quote_favorite(&self, quote_id: u32) -> Result<bool> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM favorite_quotes WHERE quote_id = ?1)",
                [quote_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Alterna o favorito e devolve o novo estado em uma única transação.
    pub fn toggle_quote_favorite(&self, quote_id: u32) -> Result<bool> {
        let transaction = self.connection.unchecked_transaction()?;
        let favorite = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM favorite_quotes WHERE quote_id = ?1)",
            [quote_id],
            |row| row.get::<_, bool>(0),
        )?;
        if favorite {
            transaction.execute(
                "DELETE FROM favorite_quotes WHERE quote_id = ?1",
                [quote_id],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO favorite_quotes (quote_id) VALUES (?1)",
                [quote_id],
            )?;
        }
        transaction.commit()?;
        Ok(!favorite)
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            restrict_directory(parent)?;
        }
        let new_database = !path.exists();
        if new_database {
            create_private_file(path)?;
        }
        let connection = Connection::open(path)?;
        let quick_check =
            connection.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))?;
        if quick_check != "ok" {
            return Err(CorruptDatabase(quick_check).into());
        }
        if !new_database {
            validate_schema_version(&connection)?;
        }
        if !new_database {
            restrict_file(path)?;
        }
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        if new_database {
            initialize_schema(&connection)?;
        }
        Ok(Self { connection })
    }

    /// Preserva um banco comprovadamente corrompido e reabre com armazenamento
    /// vazio. Erros de permissão, disco e schema incompatível continuam sendo
    /// devolvidos, pois substituir o arquivo nesses casos esconderia a causa.
    pub fn open_recovering(path: &Path) -> Result<OpenedRepository> {
        match Self::open(path) {
            Ok(repository) => Ok(OpenedRepository {
                repository,
                quarantined: None,
            }),
            Err(error) if is_database_corruption(&error) => {
                let quarantine = quarantine_database(path)?;
                Ok(OpenedRepository {
                    repository: Self::open(path)?,
                    quarantined: Some(quarantine),
                })
            }
            Err(error) => Err(error),
        }
    }

    pub fn save_session(
        &self,
        config: &TestConfig,
        status: &TestStatus,
        metrics: Metrics,
    ) -> Result<i64> {
        self.save_session_with_observations(config, status, metrics, &[])
    }

    /// Retorna a melhor velocidade já concluída com exatamente a mesma
    /// configuração. Falhas e sessões interrompidas nunca formam recordes.
    pub fn best_wpm_for(&self, config: &TestConfig) -> Result<Option<f64>> {
        let config_toml = toml::to_string(config)?;
        self.connection
            .query_row(
                "SELECT MAX(wpm) FROM sessions
                 WHERE terminal_state = 'completed' AND config_toml = ?1",
                [config_toml],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn adaptive_policy_state(&self) -> Result<AdaptivePolicyState> {
        self.connection
            .query_row(
                "SELECT active_version, fallback_version, shadow_version
                 FROM adaptive_policy_state WHERE id = 1",
                [],
                |row| {
                    Ok(AdaptivePolicyState {
                        active_version: row.get(0)?,
                        fallback_version: row.get(1)?,
                        shadow_version: row.get(2)?,
                    })
                },
            )
            .map_err(Into::into)
    }

    /// Recalcula projeções incompatíveis antes de abrir a interface. Como o
    /// aplicativo ainda não foi publicado, a atualização substitui o modelo
    /// derivado sem carregar formatos antigos; sessões e eventos permanecem.
    pub fn upgrade_adaptive_model_if_needed(&self) -> Result<bool> {
        let state = self.adaptive_policy_state()?;
        let obsolete = state.active_version != UNIFORM_POLICY_VERSION
            && state.active_version != CURRENT_POLICY_VERSION
            || state.fallback_version != UNIFORM_POLICY_VERSION
                && state.fallback_version != CURRENT_POLICY_VERSION;
        if !obsolete {
            return Ok(false);
        }
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        self.rebuild_adaptive_projections_in(&transaction, false)?;
        transaction.commit()?;
        self.connection.execute(
            "UPDATE adaptive_policy_state
             SET active_version = CASE WHEN active_version = 0 THEN 0 ELSE ?1 END,
                 fallback_version = CASE WHEN active_version = 0 THEN ?1 ELSE 0 END,
                 shadow_version = CASE WHEN active_version = 0 THEN ?1 ELSE NULL END,
                 changed_at = CURRENT_TIMESTAMP
             WHERE id = 1",
            [CURRENT_POLICY_VERSION],
        )?;
        Ok(true)
    }

    /// Troca atomicamente a política ativa pela última alternativa conhecida.
    /// A operação é reversível: uma segunda chamada restaura a versão anterior.
    pub fn rollback_adaptive_policy(&self) -> Result<AdaptivePolicyState> {
        let transaction = self.connection.unchecked_transaction()?;
        let current = transaction.query_row(
            "SELECT active_version, fallback_version FROM adaptive_policy_state WHERE id = 1",
            [],
            |row| Ok((row.get::<_, u16>(0)?, row.get::<_, u16>(1)?)),
        )?;
        for version in [current.0, current.1] {
            anyhow::ensure!(
                matches!(version, UNIFORM_POLICY_VERSION | CURRENT_POLICY_VERSION),
                "versão de política adaptativa não suportada: {version}"
            );
        }
        transaction.execute(
            "UPDATE adaptive_policy_state
             SET active_version = ?1, fallback_version = ?2,
                 shadow_version = CASE WHEN ?1 = 0 THEN ?2 ELSE NULL END,
                 changed_at = CURRENT_TIMESTAMP
             WHERE id = 1",
            params![current.1, current.0],
        )?;
        transaction.commit()?;
        Ok(AdaptivePolicyState {
            active_version: current.1,
            fallback_version: current.0,
            shadow_version: (current.1 == UNIFORM_POLICY_VERSION).then_some(current.0),
        })
    }

    /// Persiste sessão, evidências brutas e a projeção materializada do modelo
    /// adaptativo em uma única transação curta.
    pub fn save_session_with_observations(
        &self,
        config: &TestConfig,
        status: &TestStatus,
        metrics: Metrics,
        observations: &[WordObservationRecord],
    ) -> Result<i64> {
        self.save_session_full(config, status, metrics, observations, &[])
    }

    /// Persiste a sessão, suas projeções consultáveis e a fonte da verdade em
    /// uma única transação.
    pub fn save_session_full(
        &self,
        config: &TestConfig,
        status: &TestStatus,
        metrics: Metrics,
        observations: &[WordObservationRecord],
        raw_events: &[RawEvent],
    ) -> Result<i64> {
        self.save_session_full_kind(
            config,
            status,
            metrics,
            observations,
            raw_events,
            SessionKind::Practice,
        )
    }

    pub fn save_session_full_kind(
        &self,
        config: &TestConfig,
        status: &TestStatus,
        metrics: Metrics,
        observations: &[WordObservationRecord],
        raw_events: &[RawEvent],
        kind: SessionKind,
    ) -> Result<i64> {
        self.save_session_with_provenance(
            config,
            status,
            metrics,
            observations,
            raw_events,
            &SessionProvenance {
                kind,
                ..SessionProvenance::default()
            },
        )
    }

    pub fn save_session_with_provenance(
        &self,
        config: &TestConfig,
        status: &TestStatus,
        metrics: Metrics,
        observations: &[WordObservationRecord],
        raw_events: &[RawEvent],
        provenance: &SessionProvenance,
    ) -> Result<i64> {
        let terminal_state = match status {
            TestStatus::Ready => "ready",
            TestStatus::Running { .. } => match raw_events.last().map(|event| &event.kind) {
                Some(RawEventKind::Terminal(RawSessionEnd::Quit)) => "quit",
                _ => "restart",
            },
            TestStatus::Completed { .. } => "completed",
            TestStatus::Failed { .. } => "failed",
        };
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO sessions (
                terminal_state, config_toml, elapsed_ms, wpm, raw_wpm, accuracy,
                correct_chars, incorrect_chars, extra_chars, missed_chars,
                metrics_version, adaptive_version, codec_version, session_kind,
                seed_hex, stimuli_json, selections_json, policy_version,
                shadow_stimuli_json, shadow_selections_json, shadow_policy_version
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 2, 2, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                terminal_state,
                toml::to_string(config)?,
                metrics.duration_ms as i64,
                metrics.wpm,
                metrics.raw_wpm,
                metrics.accuracy,
                metrics.characters.correct_word,
                metrics.characters.incorrect,
                metrics.characters.extra,
                metrics.characters.missed,
                RawEventCodec::VERSION,
                provenance.kind.as_str(),
                format!("{:016x}", provenance.seed),
                serde_json::to_string(&provenance.stimuli)?,
                serde_json::to_string(&provenance.selections)?,
                provenance.policy_version,
                serde_json::to_string(&provenance.shadow_stimuli)?,
                serde_json::to_string(&provenance.shadow_selections)?,
                provenance.shadow_policy_version,
            ],
        )?;
        let session_id = transaction.last_insert_rowid();
        let mut reviewed_words = HashMap::<(String, String), bool>::new();
        if !raw_events.is_empty() {
            let (uncompressed_size, blob) = RawEventCodec::encode(raw_events)?;
            transaction.execute(
                "INSERT INTO raw_events (
                    session_id, codec_version, uncompressed_size, blob
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    session_id,
                    RawEventCodec::VERSION,
                    uncompressed_size as i64,
                    blob,
                ],
            )?;
        }
        for record in observations {
            if record.evidence_weight > 0.0 && !record.censored {
                reviewed_words
                    .entry((record.language.clone(), record.word.clone()))
                    .and_modify(|clean| {
                        *clean &= !record.confirmed_error && record.corrections == 0;
                    })
                    .or_insert(!record.confirmed_error && record.corrections == 0);
            }
            insert_word_observation(&transaction, session_id, record)?;

            let previous = transaction
                .query_row(
                    "SELECT state FROM word_skill WHERE language = ?1 AND word = ?2",
                    params![record.language, record.word],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()?
                .map(|bytes| WordSkill::decode(&bytes))
                .transpose()?
                .unwrap_or_default();
            let observation = adaptive_observation(record);
            let mut skill = previous;
            skill.observe(observation);
            let state = postcard::to_allocvec(&skill)?;
            transaction.execute(
                "INSERT INTO word_skill (language, word, state) VALUES (?1, ?2, ?3)
                 ON CONFLICT(language, word) DO UPDATE SET state = excluded.state",
                params![record.language, record.word, state],
            )?;
            for pattern in &record.patterns {
                let mut ngram_skill = transaction
                    .query_row(
                        "SELECT state FROM ngram_skill WHERE language = ?1 AND ngram = ?2",
                        params![record.language, pattern.pattern],
                        |row| row.get::<_, Vec<u8>>(0),
                    )
                    .optional()?
                    .map(|bytes| postcard::from_bytes::<NgramSkill>(&bytes))
                    .transpose()?
                    .unwrap_or_default();
                ngram_skill.observe(
                    &record.word,
                    Observation {
                        confirmed_error: pattern.confirmed_error,
                        corrected: pattern.corrected,
                        correction_burden: pattern.correction_burden,
                        ..observation
                    },
                );
                transaction.execute(
                    "INSERT INTO ngram_skill (language, ngram, state) VALUES (?1, ?2, ?3)
                     ON CONFLICT(language, ngram) DO UPDATE SET state = excluded.state",
                    params![
                        record.language,
                        pattern.pattern,
                        postcard::to_allocvec(&ngram_skill)?,
                    ],
                )?;
            }
            for mechanic in &record.mechanics {
                let mut skill = transaction
                    .query_row(
                        "SELECT state FROM mechanic_skill WHERE language = ?1 AND mechanic = ?2",
                        params![record.language, mechanic.mechanic],
                        |row| row.get::<_, Vec<u8>>(0),
                    )
                    .optional()?
                    .map(|bytes| postcard::from_bytes::<MechanicSkill>(&bytes))
                    .transpose()?
                    .unwrap_or_default();
                skill.observe(
                    &record.word,
                    mechanic.confirmed_error,
                    mechanic.corrected,
                    record.evidence_weight,
                );
                transaction.execute(
                    "INSERT INTO mechanic_skill (language, mechanic, state) VALUES (?1, ?2, ?3)
                     ON CONFLICT(language, mechanic) DO UPDATE SET state = excluded.state",
                    params![
                        record.language,
                        mechanic.mechanic,
                        postcard::to_allocvec(&skill)?,
                    ],
                )?;
            }
        }
        let observed_at = Local::now().timestamp();
        for ((language, word), clean) in reviewed_words {
            transaction.execute(
                "INSERT INTO skill_review (
                    language, word, last_seen_unix_s, last_session_id, consecutive_clean_sessions
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(language, word) DO UPDATE SET
                    last_seen_unix_s = excluded.last_seen_unix_s,
                    last_session_id = excluded.last_session_id,
                    consecutive_clean_sessions = CASE
                        WHEN excluded.consecutive_clean_sessions = 0 THEN 0
                        ELSE skill_review.consecutive_clean_sessions + 1
                    END",
                params![language, word, observed_at, session_id, i64::from(clean),],
            )?;
        }
        if matches!(status, TestStatus::Completed { .. }) {
            let mut xp = load_state_from(&transaction, "xp_state")?;
            let mut streak = load_state_from(&transaction, "streak_state")?;
            let day = Local::now().date_naive().num_days_from_ce();
            award(&mut xp, &mut streak, config, &metrics, day);
            save_state_to(&transaction, "xp_state", &xp)?;
            save_state_to(&transaction, "streak_state", &streak)?;
        }
        transaction.commit()?;
        Ok(session_id)
    }

    pub fn raw_events(&self, session_id: i64) -> Result<Option<Vec<RawEvent>>> {
        self.connection
            .query_row(
                "SELECT codec_version, uncompressed_size, blob
                 FROM raw_events WHERE session_id = ?1",
                [session_id],
                |row| {
                    Ok((
                        row.get::<_, u16>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()?
            .map(|(version, size, blob)| {
                let size = usize::try_from(size)
                    .context("tamanho negativo nos eventos brutos persistidos")?;
                RawEventCodec::decode(version, size, &blob)
            })
            .transpose()
    }

    pub fn session_provenance(&self, session_id: i64) -> Result<Option<SessionProvenance>> {
        self.connection
            .query_row(
                "SELECT seed_hex, stimuli_json, selections_json, policy_version, session_kind,
                        shadow_stimuli_json, shadow_selections_json, shadow_policy_version
                 FROM sessions WHERE id = ?1",
                [session_id],
                |row| {
                    let seed_hex = row.get::<_, String>(0)?;
                    let stimuli_json = row.get::<_, String>(1)?;
                    Ok((
                        seed_hex,
                        stimuli_json,
                        row.get::<_, String>(2)?,
                        row.get::<_, u16>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<u16>>(7)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(
                    seed_hex,
                    stimuli_json,
                    selections_json,
                    policy_version,
                    kind,
                    shadow_stimuli_json,
                    shadow_selections_json,
                    shadow_policy_version,
                )| {
                    Ok(SessionProvenance {
                        seed: u64::from_str_radix(&seed_hex, 16)?,
                        stimuli: serde_json::from_str(&stimuli_json)?,
                        selections: serde_json::from_str(&selections_json)?,
                        policy_version,
                        shadow_stimuli: serde_json::from_str(&shadow_stimuli_json)?,
                        shadow_selections: serde_json::from_str(&shadow_selections_json)?,
                        shadow_policy_version,
                        kind: session_kind_from_db(&kind),
                    })
                },
            )
            .transpose()
    }

    /// Recria todas as projeções adaptativas em memória e as troca numa única
    /// transação. Blobs brutos presentes são decodificados e validados antes
    /// de qualquer estado existente ser removido.
    pub fn rebuild_adaptive_projections(&self) -> Result<RebuildReport> {
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let report = self.rebuild_adaptive_projections_in(&transaction, true)?;
        transaction.commit()?;
        Ok(report)
    }

    fn rebuild_adaptive_projections_in(
        &self,
        transaction: &Connection,
        validate_raw_events: bool,
    ) -> Result<RebuildReport> {
        if validate_raw_events {
            let mut raw_statement = transaction.prepare(
                "SELECT codec_version, uncompressed_size, blob FROM raw_events ORDER BY session_id",
            )?;
            let mut raw_rows = raw_statement.query([])?;
            while let Some(row) = raw_rows.next()? {
                let version = row.get::<_, u16>(0)?;
                let size = row.get::<_, i64>(1)?;
                let blob = row.get::<_, Vec<u8>>(2)?;
                let size = usize::try_from(size)
                    .context("tamanho negativo nos eventos brutos persistidos")?;
                RawEventCodec::decode(version, size, &blob)?;
            }
            drop(raw_rows);
            drop(raw_statement);
        }

        let global_reset = transaction
            .query_row(
                "SELECT session_id FROM adaptive_resets WHERE scope = '*'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let word_resets = {
            let mut reset_statement = transaction
                .prepare("SELECT scope, session_id FROM adaptive_resets WHERE scope != '*'")?;
            reset_statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<rusqlite::Result<HashMap<_, _>>>()?
        };

        let mut words = HashMap::<(String, String), WordSkill>::new();
        let mut ngrams = HashMap::<(String, String), NgramSkill>::new();
        let mut mechanics = HashMap::<(String, String), MechanicSkill>::new();
        let mut reviews = BTreeMap::<(i64, String, String), (bool, i64)>::new();
        let mut observation_count = 0_usize;
        let mut statement = transaction.prepare(
            "SELECT wo.language, wo.word, wo.confirmed_error, wo.corrections,
                    wo.fast_success, wo.slow, wo.latency_ratio, wo.evidence_weight,
                    wo.mechanics_json, wo.session_id, unixepoch(s.created_at), wo.censored,
                    wo.corrective_events, wo.correction_ms, wo.fluent_ms, wo.grapheme_count
             FROM word_observations wo
             JOIN sessions s ON s.id = wo.session_id
             ORDER BY wo.session_id, wo.id",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let language = row.get::<_, String>(0)?;
            let word = row.get::<_, String>(1)?;
            let corrections = row.get::<_, u32>(3)?;
            let observation = Observation {
                confirmed_error: row.get(2)?,
                corrected: corrections > 0,
                corrections,
                corrective_events: row.get(12)?,
                correction_ms: row.get(13)?,
                correction_burden: correction_burden(
                    corrections,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                    row.get(15)?,
                ),
                fast_success: row.get(4)?,
                slow: row.get(5)?,
                latency_ratio: row.get(6)?,
                evidence_weight: row.get(7)?,
            };
            let session_id = row.get::<_, i64>(9)?;
            let scope = word_reset_scope(&language, &word);
            if session_id <= global_reset
                || session_id <= word_resets.get(&scope).copied().unwrap_or(0)
            {
                continue;
            }
            let stored_evidence = decode_shared_evidence(&row.get::<_, String>(8)?)?;
            let observed_at = row.get::<_, i64>(10)?;
            let censored = row.get::<_, bool>(11)?;
            observation_count = observation_count.saturating_add(1);

            words
                .entry((language.clone(), word.clone()))
                .or_default()
                .observe(observation);
            for pattern in &stored_evidence.patterns {
                ngrams
                    .entry((language.clone(), pattern.pattern.clone()))
                    .or_default()
                    .observe(
                        &word,
                        Observation {
                            confirmed_error: pattern.confirmed_error,
                            corrected: pattern.corrected,
                            correction_burden: pattern.correction_burden,
                            ..observation
                        },
                    );
            }
            for mechanic in &stored_evidence.mechanics {
                mechanics
                    .entry((language.clone(), mechanic.mechanic.clone()))
                    .or_default()
                    .observe(
                        &word,
                        mechanic.confirmed_error,
                        mechanic.corrected,
                        observation.evidence_weight,
                    );
            }
            if observation.evidence_weight > 0.0 && !censored {
                reviews
                    .entry((session_id, language, word))
                    .and_modify(|(clean, _)| {
                        *clean &= !observation.confirmed_error && corrections == 0;
                    })
                    .or_insert((
                        !observation.confirmed_error && corrections == 0,
                        observed_at,
                    ));
            }
        }
        drop(rows);
        drop(statement);
        let report = RebuildReport {
            metrics: 0,
            observations: observation_count,
            words: words.len(),
            ngrams: ngrams.len(),
            mechanics: mechanics.len(),
        };
        transaction.execute_batch(
            "DELETE FROM word_skill;
             DELETE FROM ngram_skill;
             DELETE FROM mechanic_skill;
             DELETE FROM skill_review;",
        )?;
        for ((language, word), skill) in words {
            transaction.execute(
                "INSERT INTO word_skill (language, word, state) VALUES (?1, ?2, ?3)",
                params![language, word, postcard::to_allocvec(&skill)?],
            )?;
        }
        for ((language, ngram), skill) in ngrams {
            transaction.execute(
                "INSERT INTO ngram_skill (language, ngram, state) VALUES (?1, ?2, ?3)",
                params![language, ngram, postcard::to_allocvec(&skill)?],
            )?;
        }
        for ((language, mechanic), skill) in mechanics {
            transaction.execute(
                "INSERT INTO mechanic_skill (language, mechanic, state) VALUES (?1, ?2, ?3)",
                params![language, mechanic, postcard::to_allocvec(&skill)?],
            )?;
        }
        let mut review_states = HashMap::<(String, String), ReviewState>::new();
        for ((session_id, language, word), (clean, observed_at)) in reviews {
            let state = review_states
                .entry((language.clone(), word.clone()))
                .or_default();
            state.last_seen_unix_s = observed_at;
            state.consecutive_clean_sessions = if clean {
                state.consecutive_clean_sessions.saturating_add(1)
            } else {
                0
            };
            transaction.execute(
                "INSERT INTO skill_review (
                    language, word, last_seen_unix_s, last_session_id, consecutive_clean_sessions
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(language, word) DO UPDATE SET
                    last_seen_unix_s = excluded.last_seen_unix_s,
                    last_session_id = excluded.last_session_id,
                    consecutive_clean_sessions = excluded.consecutive_clean_sessions",
                params![
                    language,
                    word,
                    observed_at,
                    session_id,
                    state.consecutive_clean_sessions,
                ],
            )?;
        }
        Ok(report)
    }

    /// Recalcula métricas consultáveis e projeções adaptativas usando apenas
    /// configuração, estímulos e eventos brutos persistidos.
    pub fn rebuild_derived_data(&self) -> Result<RebuildReport> {
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(
            "SELECT s.id, s.terminal_state, s.config_toml, s.stimuli_json,
                    s.selections_json, s.session_kind,
                    r.codec_version, r.uncompressed_size, r.blob
             FROM sessions s
             LEFT JOIN raw_events r ON r.session_id = s.id
             ORDER BY s.id",
        )?;
        let mut baselines = HashMap::<String, PersonalBaselineProfile>::new();
        let mut rebuilt = Vec::new();
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let id = row.get::<_, i64>(0)?;
            let terminal_state = row.get::<_, String>(1)?;
            let config = row.get::<_, String>(2)?;
            let stimuli = row.get::<_, String>(3)?;
            let selections = row.get::<_, String>(4)?;
            let session_kind = row.get::<_, String>(5)?;
            let stimuli = serde_json::from_str::<Vec<String>>(&stimuli)?;
            let raw = match (
                row.get::<_, Option<u16>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<Vec<u8>>>(8)?,
            ) {
                (Some(version), Some(size), Some(blob)) => {
                    let size = usize::try_from(size)
                        .context("tamanho negativo nos eventos brutos persistidos")?;
                    Some(RawEventCodec::decode(version, size, &blob)?)
                }
                (None, None, None) => None,
                _ => anyhow::bail!("registro incompleto de eventos brutos na sessão #{id}"),
            };
            if stimuli.is_empty() || raw.is_none() {
                observe_stored_session_baseline(&transaction, id, &mut baselines)?;
                continue;
            }
            let events = raw.expect("eventos brutos verificados como presentes");
            let config = toml::from_str::<TestConfig>(&config)?;
            let selections = serde_json::from_str::<Vec<Option<WordSelection>>>(&selections)?;
            let replay = replay_session(config.clone(), stimuli, &events, &terminal_state)
                .with_context(|| format!("reconstruir a sessão #{id}"))?;
            let baseline = baselines.entry(config.language.clone()).or_default();
            let observations = derive_word_observations(
                &replay.engine,
                baseline,
                session_kind_from_db(&session_kind) == SessionKind::Repeat,
                matches!(replay.end, RawSessionEnd::Restarted | RawSessionEnd::Quit),
                &selections,
            );
            baseline.observe_records(&observations);
            rebuilt.push((id, replay.engine.metrics(), observations));
        }
        drop(rows);
        drop(statement);

        for (id, metrics, observations) in &rebuilt {
            transaction.execute(
                "UPDATE sessions SET
                    elapsed_ms = ?2, wpm = ?3, raw_wpm = ?4, accuracy = ?5,
                    correct_chars = ?6, incorrect_chars = ?7, extra_chars = ?8,
                    missed_chars = ?9, metrics_version = 2
                 WHERE id = ?1",
                params![
                    id,
                    metrics.duration_ms as i64,
                    metrics.wpm,
                    metrics.raw_wpm,
                    metrics.accuracy,
                    metrics.characters.correct_word,
                    metrics.characters.incorrect,
                    metrics.characters.extra,
                    metrics.characters.missed,
                ],
            )?;
            transaction.execute("DELETE FROM word_observations WHERE session_id = ?1", [id])?;
            for observation in observations {
                insert_word_observation(&transaction, *id, observation)?;
            }
        }
        let observation_count = rebuilt
            .iter()
            .map(|(_, _, observations)| observations.len())
            .sum();
        let mut report = self.rebuild_adaptive_projections_in(&transaction, false)?;
        transaction.commit()?;
        report.metrics = rebuilt.len();
        report.observations = observation_count;
        Ok(report)
    }

    /// Faz o modelo esquecer uma palavra sem apagar sessões nem eventos brutos.
    /// Projeções compartilhadas são reconstruídas para remover só a contribuição
    /// anterior dessa palavra.
    pub fn reset_word_model(&self, language: &str, word: &str) -> Result<()> {
        let cutoff =
            self.connection
                .query_row("SELECT COALESCE(MAX(id), 0) FROM sessions", [], |row| {
                    row.get::<_, i64>(0)
                })?;
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO adaptive_resets (scope, session_id) VALUES (?1, ?2)
             ON CONFLICT(scope) DO UPDATE SET session_id = excluded.session_id",
            params![word_reset_scope(language, word), cutoff],
        )?;
        self.rebuild_adaptive_projections_in(&transaction, true)?;
        transaction.commit()?;
        Ok(())
    }

    /// Reinicia apenas o currículo adaptativo. Histórico, métricas, XP e streak
    /// permanecem intactos e continuam disponíveis para auditoria.
    pub fn reset_adaptive_model(&self) -> Result<()> {
        let cutoff =
            self.connection
                .query_row("SELECT COALESCE(MAX(id), 0) FROM sessions", [], |row| {
                    row.get::<_, i64>(0)
                })?;
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM adaptive_resets", [])?;
        transaction.execute(
            "INSERT INTO adaptive_resets (scope, session_id) VALUES ('*', ?1)",
            [cutoff],
        )?;
        transaction.execute_batch(
            "DELETE FROM word_skill;
             DELETE FROM ngram_skill;
             DELETE FROM mechanic_skill;
             DELETE FROM skill_review;",
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Baseline robusto (mediana aproximada) por idioma e tamanho. Só entra em
    /// ação após haver oito amostras, preservando o início frio.
    pub fn baseline_ms_per_grapheme(&self, language: &str) -> Result<Option<f64>> {
        Ok(self.baseline_profile(language)?.latency_ms_per_grapheme(0))
    }

    pub fn baseline_profile(&self, language: &str) -> Result<PersonalBaselineProfile> {
        let (exposures, uncorrected, corrected) = self.connection.query_row(
            "SELECT COALESCE(SUM(evidence_weight), 0.0),
                    COALESCE(SUM((confirmed_error != 0) * evidence_weight), 0.0),
                    COALESCE(SUM((corrections > 0) * evidence_weight), 0.0)
             FROM word_observations
             WHERE language = ?1
               AND active_ms > 0
               AND grapheme_count > 0
               AND censored = 0
               AND evidence_weight > 0",
            [language],
            |row| {
                Ok((
                    row.get::<_, f64>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, f64>(2)?,
                ))
            },
        )?;
        let latency_samples = self
            .connection
            .prepare(
                "SELECT grapheme_count, active_ms * 1.0 / grapheme_count
                 FROM word_observations
                 WHERE language = ?1
                   AND active_ms > 0
                   AND grapheme_count > 0
                   AND censored = 0
                   AND evidence_weight > 0
                 ORDER BY id DESC
                 LIMIT ?2",
            )?
            .query_map(params![language, BASELINE_LATENCY_WINDOW], |row| {
                Ok((row.get::<_, u16>(0)?, row.get::<_, f64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let prior = PersonalBaseline::default();
        let prior_strength = crate::adaptive::AdaptivePolicy::default().prior_strength;
        Ok(PersonalBaselineProfile {
            rates: PersonalBaseline {
                uncorrected_error_rate: (prior.uncorrected_error_rate * prior_strength
                    + uncorrected)
                    / (prior_strength + exposures),
                corrected_error_rate: (prior.corrected_error_rate * prior_strength + corrected)
                    / (prior_strength + exposures),
            },
            latency_samples,
            uncorrected_samples: uncorrected.round() as u64,
            corrected_samples: corrected.round() as u64,
        })
    }

    pub fn progress(&self) -> Result<(XpState, StreakState)> {
        Ok((
            self.load_state("xp_state")?,
            self.load_state("streak_state")?,
        ))
    }

    /// Avaliações aparecem automaticamente e nunca dependem de uma escolha na
    /// interface. A primeira só ocorre depois de sete sessões completas.
    pub fn next_session_kind(
        &self,
        config: &TestConfig,
        eligible_words: &[String],
    ) -> Result<SessionKind> {
        if matches!(config.mode, crate::typing::TestMode::Quote) {
            return Ok(SessionKind::Practice);
        }
        if !config.adaptive {
            return Ok(SessionKind::Practice);
        }
        let completed = self.connection.query_row(
            "SELECT COUNT(*) FROM sessions WHERE terminal_state = 'completed'",
            [],
            |row| row.get::<_, u64>(0),
        )?;
        let next = completed + 1;
        let eligible_words = eligible_words
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let has_due_review =
            self.load_all_review_states()?
                .into_iter()
                .any(|(language, word, state)| {
                    language == config.language
                        && eligible_words.contains(word.as_str())
                        && state.value_at(Local::now().timestamp()) > 0.0
                });
        Ok(if completed > 0 && next.is_multiple_of(8) {
            SessionKind::Assessment
        } else if completed > 0 && next.is_multiple_of(12) && has_due_review {
            SessionKind::Retention
        } else if completed > 0 && next.is_multiple_of(4) {
            SessionKind::Transfer
        } else {
            SessionKind::Practice
        })
    }

    fn load_state<T: serde::de::DeserializeOwned + Default>(&self, table: &str) -> Result<T> {
        load_state_from(&self.connection, table)
    }

    pub fn load_word_skills(&self, language: &str) -> Result<Vec<(String, String, WordSkill)>> {
        let mut statement = self
            .connection
            .prepare("SELECT language, word, state FROM word_skill WHERE language = ?1")?;
        Ok(statement
            .query_map([language], |row| {
                let encoded = row.get::<_, Vec<u8>>(2)?;
                let skill = WordSkill::decode(&encoded).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Blob,
                        Box::new(error),
                    )
                })?;
                Ok((row.get(0)?, row.get(1)?, skill))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn load_all_word_skills(&self) -> Result<Vec<(String, String, WordSkill)>> {
        let mut statement = self
            .connection
            .prepare("SELECT language, word, state FROM word_skill")?;
        Ok(statement
            .query_map([], |row| {
                let encoded = row.get::<_, Vec<u8>>(2)?;
                let skill = WordSkill::decode(&encoded).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Blob,
                        Box::new(error),
                    )
                })?;
                Ok((row.get(0)?, row.get(1)?, skill))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn load_all_ngram_skills(&self) -> Result<Vec<(String, String, NgramSkill)>> {
        let mut statement = self
            .connection
            .prepare("SELECT language, ngram, state FROM ngram_skill")?;
        Ok(statement
            .query_map([], |row| {
                let encoded = row.get::<_, Vec<u8>>(2)?;
                let skill = postcard::from_bytes(&encoded).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        encoded.len(),
                        rusqlite::types::Type::Blob,
                        Box::new(error),
                    )
                })?;
                Ok((row.get(0)?, row.get(1)?, skill))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn load_all_mechanic_skills(&self) -> Result<Vec<(String, String, MechanicSkill)>> {
        let mut statement = self
            .connection
            .prepare("SELECT language, mechanic, state FROM mechanic_skill")?;
        Ok(statement
            .query_map([], |row| {
                let state = row.get::<_, Vec<u8>>(2)?;
                let skill = postcard::from_bytes(&state).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Blob,
                        Box::new(error),
                    )
                })?;
                Ok((row.get(0)?, row.get(1)?, skill))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn load_all_review_states(&self) -> Result<Vec<(String, String, ReviewState)>> {
        let mut statement = self.connection.prepare(
            "SELECT language, word, last_seen_unix_s, consecutive_clean_sessions
             FROM skill_review",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    ReviewState {
                        last_seen_unix_s: row.get(2)?,
                        consecutive_clean_sessions: row.get::<_, u16>(3)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn statistics_overview(&self) -> Result<StatisticsOverview> {
        self.statistics_overview_for(&TestConfig::default())
    }

    /// Reconstrói a curva de alcance usando somente posições que receberam
    /// entrada real em sessões comparáveis. Palavras presentes apenas no
    /// buffer nunca contam como exposição.
    pub fn reach_profile_for(&self, config: &TestConfig, positions: usize) -> Result<ReachProfile> {
        if positions == 0 || matches!(config.mode, TestMode::Quote) {
            return Ok(ReachProfile::default());
        }
        let mut statement = self.connection.prepare(
            "SELECT s.id, s.terminal_state, s.config_toml, s.elapsed_ms, s.wpm,
                    s.accuracy, s.raw_wpm, s.correct_chars, s.incorrect_chars,
                    s.extra_chars, s.session_kind, re.codec_version,
                    re.uncompressed_size, re.blob
             FROM sessions s
             JOIN raw_events re ON re.session_id = s.id
             WHERE s.terminal_state IN ('completed', 'failed')
             ORDER BY s.id",
        )?;
        let mut rows = statement.query([])?;
        let mut samples = Vec::<(SessionSummary, bool, ReachObservation)>::new();
        while let Some(row) = rows.next()? {
            let sample_config = toml::from_str::<TestConfig>(&row.get::<_, String>(2)?)?;
            if !same_reach_context(config, &sample_config) {
                continue;
            }
            let terminal_state = row.get::<_, String>(1)?;
            let completed = terminal_state == "completed";
            let elapsed_ms = row.get::<_, i64>(3)? as u64;
            let size = usize::try_from(row.get::<_, i64>(12)?)
                .context("tamanho negativo nos eventos de uma sessão comparável")?;
            let events = RawEventCodec::decode(row.get(11)?, size, &row.get::<_, Vec<u8>>(13)?)?;
            let reached = events
                .iter()
                .filter_map(|event| match event.kind {
                    RawEventKind::Input { word_index, .. } => usize::try_from(word_index)
                        .ok()
                        .and_then(|index| index.checked_add(1)),
                    RawEventKind::Terminal(_) => None,
                })
                .max()
                .unwrap_or(0);
            let mut reach = normalized_reach_observation(
                config,
                &sample_config,
                completed,
                elapsed_ms,
                reached,
            );
            reach.reached = reach.reached.min(positions);
            if reach.reached == 0 {
                continue;
            }
            samples.push((
                SessionSummary {
                    id: row.get::<_, i64>(0)? as u64,
                    elapsed_ms,
                    wpm: row.get(4)?,
                    accuracy: row.get(5)?,
                    raw_wpm: row.get(6)?,
                    correct_chars: row.get::<_, i64>(7)? as u32,
                    incorrect_chars: row.get::<_, i64>(8)? as u32,
                    extra_chars: row.get::<_, i64>(9)? as u32,
                    config: sample_config,
                    kind: session_kind_from_db(&row.get::<_, String>(10)?),
                },
                completed,
                reach,
            ));
        }
        let valid_completed = valid_trend_sessions(
            samples
                .iter()
                .filter(|(_, completed, _)| *completed)
                .map(|(session, _, _)| session.clone())
                .collect(),
        )
        .into_iter()
        .map(|session| session.id)
        .collect::<HashSet<_>>();
        let reach_observations = samples
            .into_iter()
            .filter_map(|(session, completed, reach)| {
                (!completed || valid_completed.contains(&session.id)).then_some(reach)
            });
        Ok(ReachProfile::from_observations(
            reach_observations,
            positions,
        ))
    }

    /// Calcula a tendência geral com todas as sessões concluídas que tenham
    /// duração, volume e velocidade compatíveis com uma tentativa séria.
    /// Distribuição e atividade continuam disponíveis para análises locais.
    pub fn statistics_overview_for(
        &self,
        comparable_config: &TestConfig,
    ) -> Result<StatisticsOverview> {
        let config_toml = toml::to_string(comparable_config)?;
        let assessment_count = self.connection.query_row(
            "SELECT COUNT(*) FROM sessions
             WHERE terminal_state = 'completed' AND session_kind = 'assessment'
               AND config_toml = ?1",
            [&config_toml],
            |row| row.get::<_, u64>(0),
        )?;
        let assessments_only = assessment_count >= 2;
        let mut overview = self.connection.query_row(
            "SELECT
                COUNT(*),
                COALESCE(SUM(elapsed_ms), 0),
                COALESCE(AVG(CASE WHEN config_toml = ?1 AND (?2 = 0 OR session_kind = 'assessment') THEN wpm END), 0),
                COALESCE(AVG(CASE WHEN config_toml = ?1 AND (?2 = 0 OR session_kind = 'assessment') THEN accuracy END), 0),
                COALESCE(MAX(CASE WHEN config_toml = ?1 AND (?2 = 0 OR session_kind = 'assessment') THEN wpm END), 0)
             FROM sessions
             WHERE terminal_state = 'completed'",
            params![config_toml, assessments_only],
            |row| {
                Ok(StatisticsOverview {
                    completed_tests: row.get(0)?,
                    comparable_tests: 0,
                    active_ms: row.get::<_, i64>(1)? as u64,
                    average_wpm: row.get(2)?,
                    average_accuracy: row.get(3)?,
                    best_wpm: row.get(4)?,
                    trend_tests: Vec::new(),
                    history: Vec::new(),
                    distribution: Vec::new(),
                    daily_activity: Vec::new(),
                    priority_words: Vec::new(),
                    priority_patterns: Vec::new(),
                    total_xp: 0,
                    level: 0,
                    streak: 0,
                })
            },
        )?;
        overview.comparable_tests = self.connection.query_row(
            "SELECT COUNT(*) FROM sessions
             WHERE terminal_state = 'completed' AND config_toml = ?1
               AND (?2 = 0 OR session_kind = 'assessment')",
            params![config_toml, assessments_only],
            |row| row.get(0),
        )?;
        let mut statement = self.connection.prepare(
            "SELECT id, elapsed_ms, wpm, accuracy, raw_wpm, correct_chars,
                    incorrect_chars, extra_chars, config_toml, session_kind
             FROM sessions
             WHERE terminal_state = 'completed'
             ORDER BY id",
        )?;
        overview.trend_tests = statement
            .query_map([], |row| {
                Ok(SessionSummary {
                    id: row.get::<_, i64>(0)? as u64,
                    elapsed_ms: row.get::<_, i64>(1)? as u64,
                    wpm: row.get(2)?,
                    accuracy: row.get(3)?,
                    raw_wpm: row.get(4)?,
                    correct_chars: row.get::<_, i64>(5)? as u32,
                    incorrect_chars: row.get::<_, i64>(6)? as u32,
                    extra_chars: row.get::<_, i64>(7)? as u32,
                    config: toml::from_str(&row.get::<_, String>(8)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            8,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    kind: session_kind_from_db(&row.get::<_, String>(9)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        overview.trend_tests = valid_trend_sessions(overview.trend_tests);
        overview.comparable_tests = overview.trend_tests.len() as u64;
        if !overview.trend_tests.is_empty() {
            let count = overview.trend_tests.len() as f64;
            overview.average_wpm = overview
                .trend_tests
                .iter()
                .map(|session| session.wpm)
                .sum::<f64>()
                / count;
            overview.average_accuracy = overview
                .trend_tests
                .iter()
                .map(|session| session.accuracy)
                .sum::<f64>()
                / count;
            overview.best_wpm = overview
                .trend_tests
                .iter()
                .map(|session| session.wpm)
                .fold(0.0, f64::max);
        } else {
            overview.average_wpm = 0.0;
            overview.average_accuracy = 0.0;
            overview.best_wpm = 0.0;
        }
        overview.history = self.session_history(50)?;
        overview.distribution = self.wpm_distribution(&config_toml, assessments_only)?;
        overview.daily_activity = self.daily_activity(14)?;
        overview.priority_words = self.priority_words()?;
        overview.priority_patterns = self.priority_patterns()?;
        let (xp, streak) = self.progress()?;
        overview.total_xp = xp.total;
        overview.level = crate::gamification::level_from_total_xp(xp.total);
        overview.streak = streak.current;
        Ok(overview)
    }

    /// Retorna tentativas recentes que representam um resultado observável.
    /// Reinícios voluntários ficam na fonte bruta, mas não poluem o histórico.
    pub fn session_history(&self, limit: usize) -> Result<Vec<SessionHistoryItem>> {
        let mut statement = self.connection.prepare(
            "SELECT id, unixepoch(created_at), terminal_state, elapsed_ms, wpm,
                    accuracy, raw_wpm, correct_chars, incorrect_chars, extra_chars,
                    missed_chars, config_toml, session_kind
             FROM sessions
             WHERE terminal_state IN ('completed', 'failed')
             ORDER BY id DESC
             LIMIT ?1",
        )?;
        Ok(statement
            .query_map([limit as i64], session_history_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn session_detail(&self, id: u64) -> Result<Option<SessionDetail>> {
        let session = self
            .connection
            .query_row(
                "SELECT id, unixepoch(created_at), terminal_state, elapsed_ms, wpm,
                        accuracy, raw_wpm, correct_chars, incorrect_chars, extra_chars,
                        missed_chars, config_toml, session_kind
                 FROM sessions
                 WHERE id = ?1 AND terminal_state IN ('completed', 'failed')",
                [id],
                session_history_from_row,
            )
            .optional()?;
        let Some(session) = session else {
            return Ok(None);
        };
        let stimuli = self.connection.query_row(
            "SELECT stimuli_json FROM sessions WHERE id = ?1",
            [id],
            |row| {
                let json = row.get::<_, String>(0)?;
                serde_json::from_str(&json).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            },
        )?;
        let (observed_words, clean_words, corrected_words, failed_words, slow_words) =
            self.connection.query_row(
                "SELECT COUNT(*),
                        COALESCE(SUM(confirmed_error = 0 AND corrections = 0), 0),
                        COALESCE(SUM(confirmed_error = 0 AND corrections > 0), 0),
                        COALESCE(SUM(confirmed_error = 1), 0),
                        COALESCE(SUM(slow = 1), 0)
                 FROM word_observations WHERE session_id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, u32>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, u32>(2)?,
                        row.get::<_, u32>(3)?,
                        row.get::<_, u32>(4)?,
                    ))
                },
            )?;
        let mut statement = self.connection.prepare(
            "SELECT word, confirmed_error, corrections > 0, latency_ratio
             FROM word_observations
             WHERE session_id = ?1
               AND (confirmed_error = 1 OR corrections > 0 OR slow = 1)
             ORDER BY confirmed_error DESC, corrections DESC,
                      COALESCE(latency_ratio, 0) DESC, id
             LIMIT 8",
        )?;
        let challenges = statement
            .query_map([id], |row| {
                Ok(SessionWordDiagnostic {
                    word: row.get(0)?,
                    confirmed_error: row.get(1)?,
                    corrected: row.get(2)?,
                    latency_ratio: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(SessionDetail {
            session,
            stimuli,
            observed_words,
            clean_words,
            corrected_words,
            failed_words,
            slow_words,
            challenges,
        }))
    }

    fn wpm_distribution(
        &self,
        config_toml: &str,
        assessments_only: bool,
    ) -> Result<Vec<WpmBucket>> {
        let mut statement = self.connection.prepare(
            "SELECT wpm FROM sessions
             WHERE terminal_state = 'completed'
               AND config_toml = ?1
               AND (?2 = 0 OR session_kind = 'assessment')
             ORDER BY wpm",
        )?;
        let values = statement
            .query_map(params![config_toml, assessments_only], |row| {
                row.get::<_, f64>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if values.is_empty() {
            return Ok(Vec::new());
        }
        let maximum = values.iter().copied().fold(0.0, f64::max).ceil() as u32;
        let step = if maximum <= 80 { 10 } else { 20 };
        let bucket_count = (maximum / step + 1).clamp(1, 10) as usize;
        let mut buckets = (0..bucket_count)
            .map(|index| WpmBucket {
                start: index as u32 * step,
                end: (index as u32 + 1) * step,
                count: 0,
            })
            .collect::<Vec<_>>();
        for value in values {
            let index = ((value.max(0.0) as u32) / step) as usize;
            let last = buckets.len() - 1;
            buckets[index.min(last)].count += 1;
        }
        Ok(buckets)
    }

    fn daily_activity(&self, days: usize) -> Result<Vec<ActivityDay>> {
        let today = Local::now().date_naive();
        let first = today - Duration::days(days.saturating_sub(1) as i64);
        let mut statement = self.connection.prepare(
            "SELECT date(created_at, 'localtime'), COUNT(*), SUM(elapsed_ms), AVG(wpm)
             FROM sessions
             WHERE terminal_state = 'completed'
               AND date(created_at, 'localtime') >= ?1
             GROUP BY date(created_at, 'localtime')",
        )?;
        let mut observed = statement
            .query_map([first.to_string()], |row| {
                let date = NaiveDate::parse_from_str(&row.get::<_, String>(0)?, "%Y-%m-%d")
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                Ok((
                    date,
                    ActivityDay {
                        date,
                        tests: row.get(1)?,
                        active_ms: row.get::<_, i64>(2)? as u64,
                        average_wpm: row.get(3)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
        Ok((0..days)
            .map(|offset| {
                let date = first + Duration::days(offset as i64);
                observed.remove(&date).unwrap_or(ActivityDay {
                    date,
                    tests: 0,
                    active_ms: 0,
                    average_wpm: 0.0,
                })
            })
            .collect())
    }

    fn priority_words(&self) -> Result<Vec<PriorityWord>> {
        let policy = crate::adaptive::AdaptivePolicy::default();
        let skills = self.load_all_word_skills()?;
        let evidence = self.word_evidence_summaries()?;
        let mut baselines = HashMap::new();
        for (language, _, _) in &skills {
            if !baselines.contains_key(language) {
                baselines.insert(language.clone(), self.baseline_profile(language)?.rates);
            }
        }
        let mut scored = skills
            .into_iter()
            .map(|(language, word, skill)| {
                let baseline = baselines[&language];
                let difficulty = policy.difficulty_with_baseline(&skill, baseline);
                (language, word, skill, difficulty)
            })
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| right.3.total_cmp(&left.3));
        let mut counts = HashMap::<String, usize>::new();
        Ok(scored
            .into_iter()
            .filter_map(|(language, word, skill, difficulty)| {
                let exposures = skill.effective_exposures;
                if difficulty < crate::adaptive::MINIMUM_ACTIONABLE_DIFFICULTY
                    || counts.get(&language).copied().unwrap_or(0) >= 64
                {
                    return None;
                }
                *counts.entry(language.clone()).or_default() += 1;
                let summary = evidence
                    .get(&(language.clone(), word.clone()))
                    .copied()
                    .unwrap_or_default();
                let attempts = f64::from(summary.attempts);
                Some(PriorityWord {
                    language,
                    word,
                    difficulty,
                    confirmed_errors: f64::from(summary.failures),
                    corrections: f64::from(summary.corrected_attempts),
                    observations: summary.attempts,
                    effective_exposures: exposures,
                    uncorrected_error_rate: if attempts > 0.0 {
                        f64::from(summary.failures) / attempts
                    } else {
                        0.0
                    },
                    corrected_error_rate: if attempts > 0.0 {
                        f64::from(summary.corrected_attempts) / attempts
                    } else {
                        0.0
                    },
                    correction_burden: skill.correction_burden_mass,
                    corrected_graphemes: summary.corrected_graphemes as f64,
                    corrective_events: skill.corrective_events,
                    correction_ms: summary.correction_ms as f64,
                    baseline_exposure_chance: 0.0,
                    adaptive_exposure_chance: 0.0,
                    estimated_exposure_uplift: 0.0,
                })
            })
            .collect())
    }

    fn word_evidence_summaries(&self) -> Result<HashMap<(String, String), WordEvidenceSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT language, word, COUNT(*),
                    COALESCE(SUM(confirmed_error != 0), 0),
                    COALESCE(SUM(corrections > 0), 0),
                    COALESCE(SUM(corrections), 0),
                    COALESCE(SUM(correction_ms), 0)
             FROM word_observations
             WHERE censored = 0 AND evidence_weight > 0
             GROUP BY language, word",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                    WordEvidenceSummary {
                        attempts: row.get(2)?,
                        failures: row.get(3)?,
                        corrected_attempts: row.get(4)?,
                        corrected_graphemes: row.get(5)?,
                        correction_ms: row.get(6)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<HashMap<_, _>>>()
            .map_err(Into::into)
    }

    pub fn word_detail(&self, language: &str, word: &str) -> Result<Option<WordDetail>> {
        let Some(skill) = self
            .connection
            .query_row(
                "SELECT state FROM word_skill WHERE language = ?1 AND word = ?2",
                params![language, word],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .map(|bytes| WordSkill::decode(&bytes))
            .transpose()?
        else {
            return Ok(None);
        };
        let baseline = self.baseline_profile(language)?;
        let policy = crate::adaptive::AdaptivePolicy::default();
        let difficulty = policy.difficulty_with_baseline(&skill, baseline.rates);
        let exposures = skill.effective_exposures;
        let summary = self
            .word_evidence_summaries()?
            .get(&(language.to_owned(), word.to_owned()))
            .copied()
            .unwrap_or_default();
        let attempts = f64::from(summary.attempts);
        let priority = PriorityWord {
            language: language.to_owned(),
            word: word.to_owned(),
            difficulty,
            confirmed_errors: f64::from(summary.failures),
            corrections: f64::from(summary.corrected_attempts),
            observations: summary.attempts,
            effective_exposures: exposures,
            uncorrected_error_rate: rate(f64::from(summary.failures), attempts),
            corrected_error_rate: rate(f64::from(summary.corrected_attempts), attempts),
            correction_burden: skill.correction_burden_mass,
            corrected_graphemes: summary.corrected_graphemes as f64,
            corrective_events: skill.corrective_events,
            correction_ms: summary.correction_ms as f64,
            baseline_exposure_chance: 0.0,
            adaptive_exposure_chance: 0.0,
            estimated_exposure_uplift: 0.0,
        };

        let mut statement = self.connection.prepare(
            "SELECT wo.session_id, unixepoch(s.created_at), wo.confirmed_error,
                    wo.corrections > 0, wo.corrections, wo.correction_ms,
                    CASE WHEN wo.active_ms > 0 AND wo.grapheme_count > 0
                         THEN wo.active_ms * 1.0 / wo.grapheme_count END,
                    wo.latency_ratio, wo.grapheme_count
             FROM word_observations wo
             JOIN sessions s ON s.id = wo.session_id
             WHERE wo.language = ?1 AND wo.word = ?2 AND wo.censored = 0
             ORDER BY wo.session_id DESC, wo.id DESC
             LIMIT 12",
        )?;
        let rows = statement
            .query_map(params![language, word], |row| {
                Ok((
                    WordAttemptSummary {
                        session_id: row.get::<_, i64>(0)? as u64,
                        observed_at_unix_s: row.get(1)?,
                        confirmed_error: row.get(2)?,
                        corrected: row.get(3)?,
                        corrections: row.get(4)?,
                        correction_ms: row.get(5)?,
                        milliseconds_per_grapheme: row.get(6)?,
                        latency_ratio: row.get(7)?,
                    },
                    row.get::<_, u16>(8)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let grapheme_count = rows.first().map_or(0, |(_, count)| *count);
        let recent_attempts = rows
            .into_iter()
            .map(|(attempt, _)| attempt)
            .collect::<Vec<_>>();
        let mut latencies = recent_attempts
            .iter()
            .filter_map(|attempt| attempt.milliseconds_per_grapheme)
            .collect::<Vec<_>>();
        let median_ms_per_grapheme = median_f64(&mut latencies);
        let last_seen_unix_s = self
            .connection
            .query_row(
                "SELECT last_seen_unix_s FROM skill_review WHERE language = ?1 AND word = ?2",
                params![language, word],
                |row| row.get(0),
            )
            .optional()?;

        let word_ngrams = lexical_ngrams(word);
        let mut relevant_sequences = self
            .load_all_ngram_skills()?
            .into_iter()
            .filter(|(candidate_language, ngram, _)| {
                candidate_language == language && word_ngrams.contains(ngram)
            })
            .filter_map(|(_, ngram, skill)| {
                let difficulty = policy.ngram_difficulty(&skill, baseline.rates);
                (difficulty > 0.0).then_some((ngram, difficulty))
            })
            .collect::<Vec<_>>();
        relevant_sequences.sort_by(|left, right| right.1.total_cmp(&left.1));

        Ok(Some(WordDetail {
            priority,
            personal_baseline_ms_per_grapheme: baseline.latency_ms_per_grapheme(grapheme_count),
            median_ms_per_grapheme,
            last_seen_unix_s,
            relevant_sequences: relevant_sequences
                .into_iter()
                .take(4)
                .map(|(ngram, _)| ngram)
                .collect(),
            recent_attempts,
        }))
    }

    fn priority_patterns(&self) -> Result<Vec<PriorityPattern>> {
        let policy = crate::adaptive::AdaptivePolicy::default();
        let ngrams = self.load_all_ngram_skills()?;
        let mechanics = self.load_all_mechanic_skills()?;
        let mut baselines = HashMap::new();
        for language in ngrams
            .iter()
            .map(|(language, _, _)| language)
            .chain(mechanics.iter().map(|(language, _, _)| language))
        {
            if !baselines.contains_key(language) {
                baselines.insert(language.clone(), self.baseline_profile(language)?.rates);
            }
        }
        let mut patterns = Vec::new();
        for (language, pattern, skill) in ngrams {
            let baseline = baselines[&language];
            let difficulty = policy.ngram_difficulty(&skill, baseline);
            if difficulty > 0.0 {
                patterns.push(pattern_diagnostic(
                    language,
                    pattern.clone(),
                    pattern,
                    "sequência",
                    difficulty,
                    PatternEvidence {
                        exposures: skill.effective_exposures,
                        uncorrected: skill.uncorrected_error_mass,
                        corrected: skill.corrected_error_mass,
                        distinct_words: skill.distinct_words.len(),
                    },
                ));
            }
        }
        for (language, pattern, skill) in mechanics {
            let baseline = baselines[&language];
            let difficulty = policy.mechanic_difficulty(&skill, baseline);
            if difficulty > 0.0 {
                patterns.push(pattern_diagnostic(
                    language,
                    mechanic_label(&pattern),
                    pattern,
                    "mecânica",
                    difficulty,
                    PatternEvidence {
                        exposures: skill.effective_exposures,
                        uncorrected: skill.uncorrected_error_mass,
                        corrected: skill.corrected_error_mass,
                        distinct_words: skill.distinct_words.len(),
                    },
                ));
            }
        }
        patterns.sort_by(|left, right| right.difficulty.total_cmp(&left.difficulty));
        let mut counts = HashMap::<String, usize>::new();
        Ok(patterns
            .into_iter()
            .filter(|pattern| {
                let count = counts.entry(pattern.language.clone()).or_default();
                if *count >= 8 {
                    false
                } else {
                    *count += 1;
                    true
                }
            })
            .collect())
    }
}

fn insert_word_observation(
    connection: &Connection,
    session_id: i64,
    record: &WordObservationRecord,
) -> Result<()> {
    connection.execute(
        "INSERT INTO word_observations (
            session_id, language, word, confirmed_error, corrections,
            active_ms, afk_ms, fast_success, grapheme_count, slow,
            latency_ratio, evidence_weight, selection_source, selection_propensity,
            mechanics_json, planning_ms, fluent_ms, correction_ms,
            input_events, corrective_events, censored
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
        params![
            session_id,
            record.language,
            record.word,
            record.confirmed_error,
            record.corrections,
            record.active_ms as i64,
            record.afk_ms as i64,
            record.fast_success,
            record.grapheme_count,
            record.slow,
            record.latency_ratio,
            record.evidence_weight,
            record
                .selection_source
                .map(|source| format!("{source:?}").to_lowercase()),
            record.selection_propensity,
            serde_json::to_string(&SharedEvidenceRecord {
                mechanics: record.mechanics.clone(),
                patterns: record.patterns.clone(),
            })?,
            record.planning_ms as i64,
            record.fluent_ms as i64,
            record.correction_ms as i64,
            record.input_events,
            record.corrective_events,
            record.censored,
        ],
    )?;
    Ok(())
}

fn decode_shared_evidence(value: &str) -> Result<SharedEvidenceRecord> {
    if let Ok(evidence) = serde_json::from_str(value) {
        return Ok(evidence);
    }
    Ok(SharedEvidenceRecord {
        mechanics: serde_json::from_str(value)?,
        patterns: Vec::new(),
    })
}

fn observe_stored_session_baseline(
    connection: &Connection,
    session_id: i64,
    baselines: &mut HashMap<String, PersonalBaselineProfile>,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT language, active_ms, grapheme_count, confirmed_error, corrections
         FROM word_observations
         WHERE session_id = ?1 AND censored = 0 AND evidence_weight > 0
         ORDER BY id",
    )?;
    let samples = statement
        .query_map([session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, u16>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, u32>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (language, active_ms, grapheme_count, confirmed_error, corrections) in samples {
        baselines.entry(language).or_default().observe_sample(
            active_ms,
            grapheme_count,
            confirmed_error,
            corrections,
        );
    }
    for baseline in baselines.values_mut() {
        baseline.refresh_rates();
    }
    Ok(())
}

struct ReplayedSession {
    engine: TestEngine,
    end: RawSessionEnd,
}

fn replay_session(
    config: TestConfig,
    stimuli: Vec<String>,
    events: &[RawEvent],
    stored_terminal_state: &str,
) -> Result<ReplayedSession> {
    let mut engine = TestEngine::new(config, stimuli);
    let mut at_ms = 0_u64;
    let mut raw_end = None;
    for raw in events {
        at_ms = at_ms.saturating_add(u64::from(raw.delta_ms));
        match &raw.kind {
            RawEventKind::Input { word_index, event } => {
                anyhow::ensure!(
                    engine.active_word() == *word_index as usize,
                    "o índice do evento não corresponde à palavra ativa"
                );
                match event {
                    RecordedInputKind::InsertDelta { grapheme, .. } => {
                        engine.update(InputEvent::Key {
                            action: KeyAction::Text(grapheme.clone()),
                            at_ms,
                        });
                    }
                    RecordedInputKind::DeleteDelta {
                        deleted_graphemes,
                        whole_word,
                        ..
                    } => replay_delete(
                        &mut engine,
                        usize::from(*deleted_graphemes),
                        *whole_word,
                        at_ms,
                    ),
                    RecordedInputKind::Focus { gained } => {
                        engine.update(InputEvent::External {
                            event: ExternalEvent::Focus { gained: *gained },
                            at_ms,
                        });
                    }
                    RecordedInputKind::PasteRedacted { .. } => {}
                }
            }
            RawEventKind::Terminal(end) => {
                raw_end = Some(*end);
                engine.update(InputEvent::Tick { at_ms });
            }
        }
    }
    let raw_end = raw_end.context("a sessão não possui causa terminal")?;
    let expected_state = match raw_end {
        RawSessionEnd::Completed => "completed",
        RawSessionEnd::Failed => "failed",
        RawSessionEnd::Restarted => "restart",
        RawSessionEnd::Quit => "quit",
    };
    anyhow::ensure!(
        stored_terminal_state == expected_state,
        "a causa terminal bruta ({expected_state}) diverge do estado persistido ({stored_terminal_state})"
    );
    match raw_end {
        RawSessionEnd::Completed => anyhow::ensure!(
            matches!(engine.status(), TestStatus::Completed { .. }),
            "o replay não concluiu a sessão"
        ),
        RawSessionEnd::Failed => anyhow::ensure!(
            matches!(engine.status(), TestStatus::Failed { .. }),
            "o replay não reproduziu a falha"
        ),
        RawSessionEnd::Restarted | RawSessionEnd::Quit => {}
    }
    Ok(ReplayedSession {
        engine,
        end: raw_end,
    })
}

fn replay_delete(engine: &mut TestEngine, count: usize, whole_word: bool, at_ms: u64) {
    if whole_word {
        engine.update(InputEvent::Key {
            action: KeyAction::DeleteWordBackward,
            at_ms,
        });
    } else {
        for _ in 0..count {
            engine.update(InputEvent::Key {
                action: KeyAction::Backspace,
                at_ms,
            });
        }
    }
}

fn valid_trend_sessions(sessions: Vec<SessionSummary>) -> Vec<SessionSummary> {
    let mut speeds = sessions
        .iter()
        .filter_map(|session| session.wpm.is_finite().then_some(session.wpm))
        .collect::<Vec<_>>();
    speeds.sort_by(f64::total_cmp);
    let median = speeds.get(speeds.len() / 2).copied().unwrap_or(0.0);
    let minimum_wpm = (median * 0.4).max(15.0);

    sessions
        .into_iter()
        .filter(|session| {
            let enough_time = match session.config.mode {
                TestMode::Time { seconds } => {
                    session.elapsed_ms >= u64::from(seconds).saturating_mul(800)
                }
                TestMode::Words { .. } | TestMode::Quote => session.elapsed_ms >= 1_000,
            };
            enough_time
                && session.correct_chars >= 10
                && session.wpm.is_finite()
                && session.wpm >= minimum_wpm
                && session.accuracy.is_finite()
        })
        .collect()
}

fn same_reach_context(expected: &TestConfig, observed: &TestConfig) -> bool {
    matches!(
        (expected.mode, observed.mode),
        (TestMode::Time { .. }, TestMode::Time { .. })
            | (TestMode::Words { .. }, TestMode::Words { .. })
    ) && expected.difficulty == observed.difficulty
        && expected.punctuation == observed.punctuation
        && expected.numbers == observed.numbers
        && expected.language == observed.language
        && expected.word_pack == observed.word_pack
}

fn normalized_reach_observation(
    expected: &TestConfig,
    observed: &TestConfig,
    completed: bool,
    elapsed_ms: u64,
    reached: usize,
) -> ReachObservation {
    match (expected.mode, observed.mode) {
        (TestMode::Time { seconds: expected }, TestMode::Time { seconds: observed }) => {
            if completed && expected > observed {
                return ReachObservation {
                    reached,
                    terminal: false,
                };
            }
            let observed_horizon_ms = if completed {
                u64::from(observed).saturating_mul(1_000).max(1)
            } else {
                elapsed_ms.max(1)
            };
            let expected_horizon_ms = u64::from(expected).saturating_mul(1_000);
            let scale = (expected_horizon_ms as f64 / observed_horizon_ms as f64).min(1.0);
            ReachObservation {
                reached: (reached as f64 * scale).round() as usize,
                terminal: true,
            }
        }
        (TestMode::Words { count }, TestMode::Words { count: observed }) if completed => {
            ReachObservation {
                reached: usize::from(count.min(observed)),
                terminal: count <= observed,
            }
        }
        (TestMode::Words { count }, TestMode::Words { .. }) => ReachObservation {
            reached: reached.min(usize::from(count)),
            terminal: true,
        },
        _ => ReachObservation {
            reached: 0,
            terminal: false,
        },
    }
}

fn session_history_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionHistoryItem> {
    let state = row.get::<_, String>(2)?;
    let outcome = match state.as_str() {
        "completed" => SessionOutcome::Completed,
        "failed" => SessionOutcome::Failed,
        _ => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                format!("estado terminal inesperado: {state}").into(),
            ));
        }
    };
    let config = toml::from_str(&row.get::<_, String>(11)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(11, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(SessionHistoryItem {
        id: row.get::<_, i64>(0)? as u64,
        created_at_unix_s: row.get(1)?,
        outcome,
        elapsed_ms: row.get::<_, i64>(3)? as u64,
        wpm: row.get(4)?,
        accuracy: row.get(5)?,
        raw_wpm: row.get(6)?,
        correct_chars: row.get::<_, i64>(7)? as u32,
        incorrect_chars: row.get::<_, i64>(8)? as u32,
        extra_chars: row.get::<_, i64>(9)? as u32,
        missed_chars: row.get::<_, i64>(10)? as u32,
        config,
        kind: session_kind_from_db(&row.get::<_, String>(12)?),
    })
}

fn pattern_diagnostic(
    language: String,
    pattern: String,
    model_pattern: String,
    kind: &'static str,
    difficulty: f64,
    evidence: PatternEvidence,
) -> PriorityPattern {
    let PatternEvidence {
        exposures,
        uncorrected,
        corrected,
        distinct_words,
    } = evidence;
    PriorityPattern {
        language,
        pattern,
        model_pattern,
        kind,
        difficulty,
        estimated_exposure_uplift: 0.0,
        effective_exposures: exposures,
        uncorrected_error_rate: if exposures > 0.0 {
            uncorrected / exposures
        } else {
            0.0
        },
        corrected_error_rate: if exposures > 0.0 {
            corrected / exposures
        } else {
            0.0
        },
        distinct_words,
    }
}

fn rate(mass: f64, exposures: f64) -> f64 {
    if exposures > 0.0 {
        mass / exposures
    } else {
        0.0
    }
}

fn median_f64(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    })
}

fn load_state_from<T: serde::de::DeserializeOwned + Default>(
    connection: &Connection,
    table: &str,
) -> Result<T> {
    let sql = format!("SELECT state FROM {table} WHERE id = 1");
    let encoded = connection
        .query_row(&sql, [], |row| row.get::<_, Vec<u8>>(0))
        .optional()?;
    Ok(encoded
        .map(|data| postcard::from_bytes(&data))
        .transpose()?
        .unwrap_or_default())
}

fn save_state_to<T: serde::Serialize>(
    connection: &Connection,
    table: &str,
    state: &T,
) -> Result<()> {
    let sql = format!(
        "INSERT INTO {table} (id, state) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET state = excluded.state"
    );
    connection.execute(&sql, [postcard::to_allocvec(state)?])?;
    Ok(())
}

fn mechanic_label(mechanic: &str) -> String {
    match mechanic {
        "capitalizacao" => "maiúsculas".into(),
        "pontuacao_final" => "pontuação final".into(),
        "virgula" => "vírgula".into(),
        "acento_agudo" => "acento agudo".into(),
        "acento_circunflexo" => "circunflexo".into(),
        "acento_grave" => "acento grave".into(),
        "til" => "til".into(),
        "cedilha" => "cedilha".into(),
        "trema" => "trema".into(),
        other => other.replace('_', " "),
    }
}

fn word_reset_scope(language: &str, word: &str) -> String {
    format!("palavra\0{language}\0{word}")
}

const CURRENT_SCHEMA_VERSION: i64 = 10;

fn validate_schema_version(connection: &Connection) -> Result<()> {
    let exists = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_version')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    anyhow::ensure!(exists, "o banco não possui o schema atual do tuipe");
    let versions = connection
        .prepare("SELECT version FROM schema_version")?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(
        versions.len() == 1,
        "a versão do banco deve possuir exatamente um registro"
    );
    let version = versions[0];
    anyhow::ensure!(
        version == CURRENT_SCHEMA_VERSION,
        "schema incompatível: banco {version}, aplicativo {CURRENT_SCHEMA_VERSION}; apague o banco de desenvolvimento para recriá-lo"
    );
    Ok(())
}

fn initialize_schema(connection: &Connection) -> Result<()> {
    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch("CREATE TABLE schema_version (version INTEGER NOT NULL);")?;
    transaction.execute(
        "INSERT INTO schema_version (version) VALUES (?1)",
        [CURRENT_SCHEMA_VERSION],
    )?;
    transaction.execute_batch(
        "CREATE TABLE sessions (
           id INTEGER PRIMARY KEY, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           terminal_state TEXT NOT NULL, config_toml TEXT NOT NULL, elapsed_ms INTEGER NOT NULL,
           wpm REAL NOT NULL, raw_wpm REAL NOT NULL, accuracy REAL NOT NULL,
           correct_chars INTEGER NOT NULL, incorrect_chars INTEGER NOT NULL,
           extra_chars INTEGER NOT NULL, missed_chars INTEGER NOT NULL,
           metrics_version INTEGER NOT NULL, adaptive_version INTEGER NOT NULL, codec_version INTEGER NOT NULL,
           session_kind TEXT NOT NULL DEFAULT 'practice',
           seed_hex TEXT NOT NULL DEFAULT '0000000000000000',
           stimuli_json TEXT NOT NULL DEFAULT '[]',
           selections_json TEXT NOT NULL DEFAULT '[]',
           policy_version INTEGER NOT NULL DEFAULT 0,
           shadow_stimuli_json TEXT NOT NULL DEFAULT '[]',
           shadow_selections_json TEXT NOT NULL DEFAULT '[]',
           shadow_policy_version INTEGER
         );
         CREATE TABLE word_observations (
           id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL REFERENCES sessions(id),
           language TEXT NOT NULL, word TEXT NOT NULL, confirmed_error INTEGER NOT NULL,
           corrections INTEGER NOT NULL, active_ms INTEGER NOT NULL, afk_ms INTEGER NOT NULL,
           fast_success INTEGER NOT NULL DEFAULT 0, grapheme_count INTEGER NOT NULL DEFAULT 0,
           slow INTEGER NOT NULL DEFAULT 0, latency_ratio REAL,
           evidence_weight REAL NOT NULL DEFAULT 1,
           selection_source TEXT, selection_propensity REAL,
           mechanics_json TEXT NOT NULL DEFAULT '[]',
           planning_ms INTEGER NOT NULL DEFAULT 0,
           fluent_ms INTEGER NOT NULL DEFAULT 0,
           correction_ms INTEGER NOT NULL DEFAULT 0,
           input_events INTEGER NOT NULL DEFAULT 0,
           corrective_events INTEGER NOT NULL DEFAULT 0,
           censored INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE word_skill (language TEXT NOT NULL, word TEXT NOT NULL, state BLOB NOT NULL, PRIMARY KEY(language, word));
         CREATE TABLE ngram_skill (language TEXT NOT NULL, ngram TEXT NOT NULL, state BLOB NOT NULL, PRIMARY KEY(language, ngram));
         CREATE TABLE mechanic_skill (language TEXT NOT NULL, mechanic TEXT NOT NULL, state BLOB NOT NULL, PRIMARY KEY(language, mechanic));
         CREATE TABLE skill_review (
           language TEXT NOT NULL, word TEXT NOT NULL, last_seen_unix_s INTEGER NOT NULL,
           last_session_id INTEGER NOT NULL REFERENCES sessions(id),
           consecutive_clean_sessions INTEGER NOT NULL,
           PRIMARY KEY(language, word)
         );
         CREATE TABLE favorite_quotes (quote_id INTEGER PRIMARY KEY);
         CREATE TABLE xp_state (id INTEGER PRIMARY KEY CHECK(id = 1), state BLOB NOT NULL);
         CREATE TABLE streak_state (id INTEGER PRIMARY KEY CHECK(id = 1), state BLOB NOT NULL);
         CREATE TABLE adaptive_resets (scope TEXT PRIMARY KEY, session_id INTEGER NOT NULL);
         CREATE TABLE adaptive_policy_state (
           id INTEGER PRIMARY KEY CHECK(id = 1),
           active_version INTEGER NOT NULL,
           fallback_version INTEGER NOT NULL,
           shadow_version INTEGER,
           changed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         );
         INSERT INTO adaptive_policy_state (id, active_version, fallback_version)
           VALUES (1, 4, 0);
         CREATE TABLE raw_events (session_id INTEGER PRIMARY KEY REFERENCES sessions(id), codec_version INTEGER NOT NULL, uncompressed_size INTEGER NOT NULL, blob BLOB NOT NULL);",
    )?;
    transaction.execute_batch(
        "CREATE INDEX idx_word_observations_session ON word_observations(session_id);
         CREATE INDEX idx_word_observations_baseline ON word_observations(language, active_ms, grapheme_count);
         CREATE INDEX idx_sessions_history ON sessions(terminal_state, session_kind, id DESC);
         CREATE INDEX idx_sessions_comparable ON sessions(config_toml, terminal_state, session_kind, id DESC);",
    )?;
    transaction.commit()?;
    Ok(())
}

fn lexical_stimulus(value: &str) -> Option<String> {
    let lexical = value
        .trim_matches(|character: char| !character.is_alphabetic())
        .to_lowercase();
    (!lexical.is_empty()).then_some(lexical)
}

fn validate_selection_trace(
    session_id: i64,
    label: &str,
    stimuli: &[String],
    selections: &[Option<WordSelection>],
) -> Result<()> {
    anyhow::ensure!(
        selections.is_empty() || selections.len() == stimuli.len(),
        "sessão #{session_id}: seleção {label} não corresponde aos estímulos"
    );
    for (stimulus, selection) in
        stimuli
            .iter()
            .zip(selections)
            .filter_map(|(stimulus, selection)| {
                selection.as_ref().map(|selection| (stimulus, selection))
            })
    {
        anyhow::ensure!(
            !selection.word.is_empty()
                && selection.propensity.is_finite()
                && (0.0..=1.0).contains(&selection.propensity),
            "sessão #{session_id}: seleção {label} inválida"
        );
        anyhow::ensure!(
            lexical_stimulus(stimulus).as_deref() == Some(selection.word.as_str()),
            "sessão #{session_id}: seleção {label} não corresponde ao texto"
        );
    }
    Ok(())
}

fn is_database_corruption(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        if cause.is::<CorruptDatabase>() {
            return true;
        }
        let Some(error) = cause.downcast_ref::<rusqlite::Error>() else {
            return false;
        };
        matches!(
            error,
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error {
                    code: rusqlite::ffi::ErrorCode::DatabaseCorrupt
                        | rusqlite::ffi::ErrorCode::NotADatabase,
                    ..
                },
                _
            )
        )
    })
}

fn quarantine_database(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .context("o caminho do banco deve ter um diretório pai")?;
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("tuipe");
    let quarantine = parent.join(format!(
        "{stem}-corrompido-{}.db",
        chrono::Utc::now().timestamp_millis()
    ));
    fs::rename(path, &quarantine)?;
    restrict_file(&quarantine)?;
    for suffix in ["-wal", "-shm"] {
        let source = PathBuf::from(format!("{}{suffix}", path.display()));
        if source.exists() {
            let destination = PathBuf::from(format!("{}{suffix}", quarantine.display()));
            fs::rename(source, destination)?;
        }
    }
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(quarantine)
}

fn create_private_file(path: &Path) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)?;
    restrict_file(path)?;
    Ok(())
}

fn restrict_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn restrict_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn session_kind_from_db(value: &str) -> SessionKind {
    match value {
        "assessment" => SessionKind::Assessment,
        "transfer" => SessionKind::Transfer,
        "retention" => SessionKind::Retention,
        "repeat" => SessionKind::Repeat,
        _ => SessionKind::Practice,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typing::{RecordedInputEvent, TestMode, TestStatus};

    fn word_observation(word: &str, confirmed_error: bool) -> WordObservationRecord {
        WordObservationRecord {
            language: "portuguese".into(),
            word: word.into(),
            confirmed_error,
            corrections: 0,
            active_ms: 300,
            afk_ms: 0,
            planning_ms: 0,
            fluent_ms: 300,
            correction_ms: 0,
            input_events: 5,
            corrective_events: 0,
            censored: false,
            grapheme_count: 5,
            fast_success: !confirmed_error,
            slow: false,
            latency_ratio: Some(1.0),
            evidence_weight: 1.0,
            selection_source: None,
            selection_propensity: None,
            mechanics: Vec::new(),
            patterns: Vec::new(),
        }
    }

    fn eligible_words() -> Vec<String> {
        vec!["casa".into(), "tempo".into(), "ação".into()]
    }

    fn raw_reach(reached: usize, end: RawSessionEnd) -> Vec<RawEvent> {
        let events = (0..reached)
            .map(|word_index| RecordedInputEvent {
                at_ms: (word_index as u64 + 1) * 100,
                word_index,
                kind: RecordedInputKind::InsertDelta {
                    grapheme: "a".into(),
                    expected: Some("a".into()),
                    correct: true,
                },
            })
            .collect::<Vec<_>>();
        RawEventCodec::materialize(&events, reached as u64 * 100, end)
    }

    #[test]
    fn inicializacao_cria_o_repositorio_atual() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let id = repository
            .save_session(
                &TestConfig::default(),
                &TestStatus::Completed { ended_at_ms: 1 },
                Metrics::default(),
            )
            .unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn curva_de_alcance_usa_posicoes_digitadas_e_nao_o_buffer() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let config = TestConfig {
            mode: TestMode::Words { count: 4 },
            ..TestConfig::default()
        };
        let metrics = Metrics {
            duration_ms: 4_000,
            wpm: 60.0,
            raw_wpm: 60.0,
            accuracy: 100.0,
            characters: crate::typing::CharacterStats {
                correct_word: 20,
                ..Default::default()
            },
            ..Default::default()
        };
        repository
            .save_session_with_provenance(
                &config,
                &TestStatus::Completed { ended_at_ms: 400 },
                metrics.clone(),
                &[],
                &raw_reach(4, RawSessionEnd::Completed),
                &SessionProvenance::default(),
            )
            .unwrap();
        let mut uniform_config = config.clone();
        uniform_config.adaptive = false;
        repository
            .save_session_with_provenance(
                &uniform_config,
                &TestStatus::Failed {
                    word_index: 2,
                    ended_at_ms: 300,
                },
                metrics,
                &[],
                &raw_reach(3, RawSessionEnd::Failed),
                &SessionProvenance::default(),
            )
            .unwrap();

        let profile = repository.reach_profile_for(&config, 4).unwrap();

        assert_eq!(profile.probability(0), 1.0);
        assert_eq!(profile.probability(2), 1.0);
        assert_eq!(profile.probability(3), 0.5);
        assert_eq!(profile.probability(4), 0.0);
    }

    #[test]
    fn normalizacao_de_alcance_nunca_extrapola_o_que_nao_foi_observado() {
        let time_15 = TestConfig {
            mode: TestMode::Time { seconds: 15 },
            ..TestConfig::default()
        };
        let time_30 = TestConfig {
            mode: TestMode::Time { seconds: 30 },
            ..TestConfig::default()
        };
        let words_25 = TestConfig {
            mode: TestMode::Words { count: 25 },
            ..TestConfig::default()
        };
        let words_50 = TestConfig {
            mode: TestMode::Words { count: 50 },
            ..TestConfig::default()
        };

        assert_eq!(
            normalized_reach_observation(&time_15, &time_30, true, 30_000, 20),
            ReachObservation {
                reached: 10,
                terminal: true,
            }
        );
        assert_eq!(
            normalized_reach_observation(&time_30, &time_15, true, 15_000, 10),
            ReachObservation {
                reached: 10,
                terminal: false,
            }
        );
        assert_eq!(
            normalized_reach_observation(&words_50, &words_25, true, 10_000, 25),
            ReachObservation {
                reached: 25,
                terminal: false,
            }
        );
    }

    #[test]
    fn rollback_da_politica_e_atomico_persistente_e_reversivel() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("history.db");
        let repository = Repository::open(&path).unwrap();
        assert_eq!(
            repository.adaptive_policy_state().unwrap(),
            AdaptivePolicyState {
                active_version: CURRENT_POLICY_VERSION,
                fallback_version: UNIFORM_POLICY_VERSION,
                shadow_version: None,
            }
        );
        assert_eq!(
            repository.rollback_adaptive_policy().unwrap(),
            AdaptivePolicyState {
                active_version: UNIFORM_POLICY_VERSION,
                fallback_version: CURRENT_POLICY_VERSION,
                shadow_version: Some(CURRENT_POLICY_VERSION),
            }
        );
        drop(repository);

        let repository = Repository::open(&path).unwrap();
        assert_eq!(
            repository.adaptive_policy_state().unwrap().active_version,
            UNIFORM_POLICY_VERSION
        );
        assert_eq!(
            repository
                .rollback_adaptive_policy()
                .unwrap()
                .active_version,
            CURRENT_POLICY_VERSION
        );
    }

    #[test]
    fn modelo_de_desenvolvimento_antigo_e_recalculado_sem_apagar_historico() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        repository
            .connection
            .execute(
                "UPDATE adaptive_policy_state SET active_version = 3, fallback_version = 0",
                [],
            )
            .unwrap();

        assert!(repository.upgrade_adaptive_model_if_needed().unwrap());
        assert_eq!(
            repository.adaptive_policy_state().unwrap().active_version,
            CURRENT_POLICY_VERSION
        );
        assert!(!repository.upgrade_adaptive_model_if_needed().unwrap());
    }

    #[test]
    fn favorito_de_citacao_alterna_sem_duplicar() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();

        assert!(!repository.is_quote_favorite(42).unwrap());
        assert!(repository.toggle_quote_favorite(42).unwrap());
        assert!(repository.is_quote_favorite(42).unwrap());
        assert!(!repository.toggle_quote_favorite(42).unwrap());
        assert!(!repository.is_quote_favorite(42).unwrap());
    }

    #[test]
    fn schema_diferente_e_rejeitado_sem_alterar_o_arquivo() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("future.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version VALUES (999);",
            )
            .unwrap();
        drop(connection);

        let error = Repository::open(&path).err().unwrap().to_string();

        assert!(error.contains("schema incompatível"));
        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT version FROM schema_version", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            999
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn banco_corrompido_e_preservado_e_substituido_sem_perder_o_arquivo() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("history.db");
        fs::write(&path, b"nao e um banco sqlite").unwrap();

        let opened = Repository::open_recovering(&path).unwrap();
        let quarantine = opened.quarantined.unwrap();

        assert!(quarantine.exists());
        assert_eq!(fs::read(quarantine).unwrap(), b"nao e um banco sqlite");
        assert!(path.exists());
        assert_eq!(
            opened
                .repository
                .statistics_overview()
                .unwrap()
                .completed_tests,
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn banco_e_diretorio_sao_privados() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("tuipe");
        let path = directory.join("tuipe.db");

        Repository::open(&path).unwrap();

        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn falha_na_gamificacao_reverte_a_sessao_inteira() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        repository
            .connection
            .execute("INSERT INTO streak_state (id, state) VALUES (1, X'FF')", [])
            .unwrap();

        let result = repository.save_session(
            &TestConfig::default(),
            &TestStatus::Completed { ended_at_ms: 1 },
            Metrics::default(),
        );

        assert!(result.is_err());
        assert_eq!(
            repository
                .connection
                .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(
            repository
                .connection
                .query_row("SELECT COUNT(*) FROM xp_state", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn doctor_e_backup_validam_uma_copia_consistente() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source.db");
        let backup = temporary.path().join("backup.db");
        let repository = Repository::open(&source).unwrap();
        repository
            .save_session(
                &TestConfig::default(),
                &TestStatus::Completed { ended_at_ms: 1 },
                Metrics::default(),
            )
            .unwrap();

        Repository::doctor(&source).unwrap();
        repository.backup(&backup).unwrap();
        Repository::doctor(&backup).unwrap();
        assert_eq!(
            Repository::open(&backup)
                .unwrap()
                .statistics_overview()
                .unwrap()
                .completed_tests,
            1
        );
    }

    #[test]
    fn doctor_rejeita_selecao_shadow_que_nao_corresponde_ao_estimulo() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("shadow-invalido.db");
        let repository = Repository::open(&path).unwrap();
        repository
            .save_session_with_provenance(
                &TestConfig::default(),
                &TestStatus::Completed { ended_at_ms: 1 },
                Metrics::default(),
                &[],
                &[],
                &SessionProvenance {
                    shadow_stimuli: vec!["casa ".into()],
                    shadow_selections: vec![Some(WordSelection {
                        word: "tempo".into(),
                        source: SelectionSource::Targeted,
                        propensity: 0.25,
                    })],
                    shadow_policy_version: Some(CURRENT_POLICY_VERSION),
                    ..SessionProvenance::default()
                },
            )
            .unwrap();
        drop(repository);

        assert!(
            Repository::doctor(&path)
                .unwrap_err()
                .to_string()
                .contains("não corresponde ao texto")
        );
    }

    #[test]
    fn sessao_congela_seed_estimulos_politica_e_tipo() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let provenance = SessionProvenance {
            seed: u64::MAX - 3,
            stimuli: vec!["ação".into(), "casa".into()],
            selections: vec![],
            policy_version: 2,
            shadow_stimuli: vec!["tempo".into(), "ação".into()],
            shadow_selections: vec![
                Some(WordSelection {
                    word: "tempo".into(),
                    source: SelectionSource::Targeted,
                    propensity: 0.25,
                }),
                None,
            ],
            shadow_policy_version: Some(3),
            kind: SessionKind::Assessment,
        };
        let id = repository
            .save_session_with_provenance(
                &TestConfig::default(),
                &TestStatus::Completed { ended_at_ms: 1 },
                Metrics::default(),
                &[],
                &[],
                &provenance,
            )
            .unwrap();
        assert_eq!(repository.session_provenance(id).unwrap(), Some(provenance));
    }

    #[test]
    fn rebuild_reproduz_metricas_originais_a_partir_dos_eventos() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let config = TestConfig {
            mode: TestMode::Words { count: 1 },
            ..TestConfig::default()
        };
        let stimuli = vec!["casa".to_owned()];
        let mut engine = TestEngine::new(config.clone(), stimuli.clone());
        for (index, grapheme) in ["c", "a", "s", "a"].into_iter().enumerate() {
            engine.update(InputEvent::Key {
                action: KeyAction::Text(grapheme.to_owned()),
                at_ms: 100 + index as u64 * 100,
            });
        }
        assert!(matches!(engine.status(), TestStatus::Completed { .. }));
        let expected = engine.metrics();
        let raw =
            RawEventCodec::materialize(engine.recorded_events(), 400, RawSessionEnd::Completed);
        let id = repository
            .save_session_with_provenance(
                &config,
                engine.status(),
                expected.clone(),
                &[],
                &raw,
                &SessionProvenance {
                    stimuli,
                    ..SessionProvenance::default()
                },
            )
            .unwrap();
        repository
            .connection
            .execute(
                "UPDATE sessions SET elapsed_ms = 1, wpm = 1, raw_wpm = 1,
                 accuracy = 1, correct_chars = 1, incorrect_chars = 1,
                 extra_chars = 1, missed_chars = 1, metrics_version = 0
                 WHERE id = ?1",
                [id],
            )
            .unwrap();

        let report = repository.rebuild_derived_data().unwrap();
        let rebuilt = repository
            .connection
            .query_row(
                "SELECT elapsed_ms, wpm, raw_wpm, accuracy, correct_chars,
                        incorrect_chars, extra_chars, missed_chars, metrics_version
                 FROM sessions WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, f64>(1)?,
                        row.get::<_, f64>(2)?,
                        row.get::<_, f64>(3)?,
                        row.get::<_, u32>(4)?,
                        row.get::<_, u32>(5)?,
                        row.get::<_, u32>(6)?,
                        row.get::<_, u32>(7)?,
                        row.get::<_, u16>(8)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(report.metrics, 1);
        assert_eq!(report.observations, 1);
        assert_eq!(rebuilt.0, expected.duration_ms);
        assert_eq!(rebuilt.1, expected.wpm);
        assert_eq!(rebuilt.2, expected.raw_wpm);
        assert_eq!(rebuilt.3, expected.accuracy);
        assert_eq!(rebuilt.4, expected.characters.correct_word);
        assert_eq!(rebuilt.5, expected.characters.incorrect);
        assert_eq!(rebuilt.6, expected.characters.extra);
        assert_eq!(rebuilt.7, expected.characters.missed);
        assert_eq!(rebuilt.8, 2);
        assert_eq!(
            repository
                .connection
                .query_row(
                    "SELECT word FROM word_observations WHERE session_id = ?1",
                    [id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "casa"
        );
    }

    #[test]
    fn rebuild_reverte_metricas_se_a_projecao_falhar() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let config = TestConfig {
            mode: TestMode::Words { count: 1 },
            ..TestConfig::default()
        };
        let mut engine = TestEngine::new(config.clone(), ["casa".into()]);
        engine.update(InputEvent::Key {
            action: KeyAction::Text("casa".into()),
            at_ms: 400,
        });
        let raw =
            RawEventCodec::materialize(engine.recorded_events(), 400, RawSessionEnd::Completed);
        let id = repository
            .save_session_with_provenance(
                &config,
                engine.status(),
                engine.metrics(),
                &[],
                &raw,
                &SessionProvenance {
                    stimuli: vec!["casa".into()],
                    ..SessionProvenance::default()
                },
            )
            .unwrap();
        repository
            .connection
            .execute("UPDATE sessions SET wpm = 1 WHERE id = ?1", [id])
            .unwrap();
        repository
            .connection
            .execute_batch(
                "CREATE TEMP TRIGGER impedir_skill
                 BEFORE INSERT ON word_skill
                 BEGIN SELECT RAISE(ABORT, 'falha injetada'); END;",
            )
            .unwrap();

        assert!(repository.rebuild_derived_data().is_err());
        assert_eq!(
            repository
                .connection
                .query_row("SELECT wpm FROM sessions WHERE id = ?1", [id], |row| row
                    .get::<_, f64>(0))
                .unwrap(),
            1.0
        );
        assert_eq!(
            repository
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM word_observations WHERE session_id = ?1",
                    [id],
                    |row| row.get::<_, u64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn rebuild_usa_o_historico_anterior_no_baseline_cronologico() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        for index in 0..8 {
            repository
                .save_session_with_observations(
                    &TestConfig::default(),
                    &TestStatus::Completed { ended_at_ms: index },
                    Metrics::default(),
                    &[word_observation(&format!("anterior{index}"), false)],
                )
                .unwrap();
        }
        let config = TestConfig {
            mode: TestMode::Words { count: 1 },
            ..TestConfig::default()
        };
        let mut engine = TestEngine::new(config.clone(), ["casa".into()]);
        for (grapheme, at_ms) in [("c", 100), ("a", 300), ("s", 500), ("a", 700)] {
            engine.update(InputEvent::Key {
                action: KeyAction::Text(grapheme.into()),
                at_ms,
            });
        }
        let baseline = repository.baseline_profile("portuguese").unwrap();
        let expected = derive_word_observations(&engine, &baseline, false, false, &[])[0]
            .latency_ratio
            .unwrap();
        let raw =
            RawEventCodec::materialize(engine.recorded_events(), 700, RawSessionEnd::Completed);
        let id = repository
            .save_session_with_provenance(
                &config,
                engine.status(),
                engine.metrics(),
                &[],
                &raw,
                &SessionProvenance {
                    stimuli: vec!["casa".into()],
                    ..SessionProvenance::default()
                },
            )
            .unwrap();

        repository.rebuild_derived_data().unwrap();
        let rebuilt = repository
            .connection
            .query_row(
                "SELECT latency_ratio FROM word_observations WHERE session_id = ?1",
                [id],
                |row| row.get::<_, f64>(0),
            )
            .unwrap();
        assert!((rebuilt - expected).abs() < f64::EPSILON);
    }

    #[test]
    fn sessao_nova_usa_a_versao_atual_das_metricas() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let id = repository
            .save_session(
                &TestConfig::default(),
                &TestStatus::Completed { ended_at_ms: 1 },
                Metrics::default(),
            )
            .unwrap();

        assert_eq!(
            repository
                .connection
                .query_row(
                    "SELECT metrics_version FROM sessions WHERE id = ?1",
                    [id],
                    |row| row.get::<_, u16>(0),
                )
                .unwrap(),
            2
        );
    }

    #[test]
    fn rebuild_recupera_exposicao_parcial_e_distingue_saida() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let config = TestConfig {
            mode: TestMode::Words { count: 1 },
            ..TestConfig::default()
        };
        let stimuli = vec!["casa".to_owned()];
        let mut engine = TestEngine::new(config.clone(), stimuli.clone());
        engine.update(InputEvent::Key {
            action: KeyAction::Text("x".into()),
            at_ms: 100,
        });
        let raw = RawEventCodec::materialize(engine.recorded_events(), 200, RawSessionEnd::Quit);
        let id = repository
            .save_session_with_provenance(
                &config,
                engine.status(),
                engine.metrics(),
                &[],
                &raw,
                &SessionProvenance {
                    stimuli,
                    ..SessionProvenance::default()
                },
            )
            .unwrap();

        let report = repository.rebuild_derived_data().unwrap();
        let stored = repository
            .connection
            .query_row(
                "SELECT s.terminal_state, wo.confirmed_error, wo.censored
                 FROM sessions s JOIN word_observations wo ON wo.session_id = s.id
                 WHERE s.id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(report.observations, 1);
        assert_eq!(stored, ("quit".into(), true, true));
    }

    #[test]
    fn overview_uses_only_completed_tests() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let completed = Metrics {
            duration_ms: 30_000,
            wpm: 80.0,
            accuracy: 95.0,
            characters: crate::typing::CharacterStats {
                correct_word: 100,
                ..crate::typing::CharacterStats::default()
            },
            ..Metrics::default()
        };
        repository
            .save_session(
                &TestConfig::default(),
                &TestStatus::Completed {
                    ended_at_ms: 30_000,
                },
                completed,
            )
            .unwrap();
        repository
            .save_session(
                &TestConfig::default(),
                &TestStatus::Failed {
                    ended_at_ms: 1_000,
                    word_index: 0,
                },
                Metrics::default(),
            )
            .unwrap();

        let overview = repository.statistics_overview().unwrap();
        assert_eq!(overview.completed_tests, 1);
        assert_eq!(overview.comparable_tests, 1);
        assert_eq!(overview.active_ms, 30_000);
        assert_eq!(overview.average_wpm, 80.0);
        assert_eq!(overview.average_accuracy, 95.0);
        assert_eq!(overview.best_wpm, 80.0);
        assert_eq!(
            overview.trend_tests,
            vec![SessionSummary {
                id: 1,
                elapsed_ms: 30_000,
                wpm: 80.0,
                accuracy: 95.0,
                raw_wpm: 0.0,
                correct_chars: 100,
                incorrect_chars: 0,
                extra_chars: 0,
                config: TestConfig::default(),
                kind: SessionKind::Practice,
            }]
        );
        assert_eq!(overview.history.len(), 2);
        assert_eq!(overview.history[0].outcome, SessionOutcome::Failed);
        assert_eq!(overview.history[1].outcome, SessionOutcome::Completed);
        assert_eq!(overview.distribution.len(), 9);
        assert_eq!(overview.distribution[8].count, 1);
        assert_eq!(overview.daily_activity.len(), 14);
        assert_eq!(overview.priority_words, Vec::new());
        assert_eq!(overview.priority_patterns, Vec::new());
        assert_eq!(overview.total_xp, 68);
        assert_eq!(overview.level, 1);
        assert_eq!(overview.streak, 1);
    }

    #[test]
    fn tendencia_reune_testes_validos_de_configuracoes_diferentes() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let reference = TestConfig::default();
        let mut other = reference.clone();
        other.mode = crate::typing::TestMode::Words { count: 50 };
        repository
            .save_session(
                &reference,
                &TestStatus::Completed {
                    ended_at_ms: 30_000,
                },
                Metrics {
                    duration_ms: 30_000,
                    wpm: 70.0,
                    accuracy: 95.0,
                    characters: crate::typing::CharacterStats {
                        correct_word: 100,
                        ..crate::typing::CharacterStats::default()
                    },
                    ..Metrics::default()
                },
            )
            .unwrap();
        repository
            .save_session(
                &other,
                &TestStatus::Completed {
                    ended_at_ms: 30_000,
                },
                Metrics {
                    duration_ms: 30_000,
                    wpm: 140.0,
                    accuracy: 80.0,
                    characters: crate::typing::CharacterStats {
                        correct_word: 100,
                        ..crate::typing::CharacterStats::default()
                    },
                    ..Metrics::default()
                },
            )
            .unwrap();

        let overview = repository.statistics_overview_for(&reference).unwrap();
        assert_eq!(overview.completed_tests, 2);
        assert_eq!(overview.comparable_tests, 2);
        assert_eq!(overview.average_wpm, 105.0);
        assert_eq!(overview.average_accuracy, 87.5);
        assert_eq!(overview.trend_tests.len(), 2);
        assert_eq!(overview.trend_tests[0].wpm, 70.0);
        assert_eq!(overview.trend_tests[1].wpm, 140.0);
    }

    #[test]
    fn detalhe_da_sessao_resume_sinais_sem_reprocessar_eventos() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let observation = WordObservationRecord {
            language: "portuguese".into(),
            word: "através".into(),
            confirmed_error: false,
            corrections: 1,
            active_ms: 900,
            afk_ms: 0,
            planning_ms: 100,
            fluent_ms: 500,
            correction_ms: 300,
            input_events: 8,
            corrective_events: 1,
            censored: false,
            grapheme_count: 7,
            fast_success: false,
            slow: true,
            latency_ratio: Some(1.6),
            evidence_weight: 1.0,
            selection_source: None,
            selection_propensity: None,
            mechanics: Vec::new(),
            patterns: Vec::new(),
        };
        let id = repository
            .save_session_with_provenance(
                &TestConfig::default(),
                &TestStatus::Completed { ended_at_ms: 900 },
                Metrics {
                    duration_ms: 900,
                    wpm: 80.0,
                    accuracy: 96.0,
                    ..Metrics::default()
                },
                &[observation],
                &[],
                &SessionProvenance {
                    stimuli: vec!["através".into()],
                    ..SessionProvenance::default()
                },
            )
            .unwrap();

        let detail = repository.session_detail(id as u64).unwrap().unwrap();
        assert_eq!(detail.stimuli, vec!["através"]);
        assert_eq!(detail.observed_words, 1);
        assert_eq!(detail.corrected_words, 1);
        assert_eq!(detail.slow_words, 1);
        assert_eq!(detail.challenges[0].word, "através");
        assert_eq!(detail.challenges[0].latency_ratio, Some(1.6));
    }

    #[test]
    fn observacoes_atualizam_e_restauram_a_habilidade_da_palavra() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        repository
            .save_session_with_observations(
                &TestConfig::default(),
                &TestStatus::Failed {
                    ended_at_ms: 1_000,
                    word_index: 0,
                },
                Metrics::default(),
                &[WordObservationRecord {
                    language: "portuguese".into(),
                    word: "difícil".into(),
                    confirmed_error: true,
                    corrections: 2,
                    active_ms: 320,
                    afk_ms: 0,
                    planning_ms: 0,
                    fluent_ms: 320,
                    correction_ms: 0,
                    input_events: 7,
                    corrective_events: 0,
                    censored: false,
                    grapheme_count: 7,
                    fast_success: false,
                    slow: false,
                    latency_ratio: None,
                    evidence_weight: 1.0,
                    selection_source: None,
                    selection_propensity: None,
                    mechanics: Vec::new(),
                    patterns: Vec::new(),
                }],
            )
            .unwrap();

        assert_eq!(
            repository.load_word_skills("portuguese").unwrap(),
            vec![(
                "portuguese".into(),
                "difícil".into(),
                WordSkill {
                    confirmed_errors: 1.0,
                    corrections: 1.0,
                    fast_successes: 0.0,
                    slowdowns: 0.0,
                    observations: 1,
                    model_version: 3,
                    effective_exposures: 1.0,
                    uncorrected_error_mass: 1.0,
                    corrected_error_mass: 1.0,
                    correction_burden_mass: correction_burden(2, 0, 0, 320, 7),
                    corrected_graphemes: 2.0,
                    corrective_events: 0.0,
                    correction_ms: 0.0,
                    latency_log_residual_sum: 0.0,
                    latency_weight: 0.0,
                },
            )]
        );
        let priority = repository.statistics_overview().unwrap().priority_words;
        assert!(
            priority.is_empty(),
            "uma observação isolada deve continuar abaixo do limiar acionável"
        );
        let detail = repository
            .word_detail("portuguese", "difícil")
            .unwrap()
            .unwrap();
        assert_eq!(detail.recent_attempts.len(), 1);
        assert!(detail.recent_attempts[0].confirmed_error);
        assert_eq!(detail.median_ms_per_grapheme, Some(320.0 / 7.0));
        assert!(detail.last_seen_unix_s.is_some());

        let expected = repository.load_word_skills("portuguese").unwrap();
        repository
            .connection
            .execute_batch(
                "DELETE FROM word_skill;
                 DELETE FROM ngram_skill;
                 DELETE FROM mechanic_skill;
                 DELETE FROM skill_review;",
            )
            .unwrap();
        let report = repository.rebuild_adaptive_projections().unwrap();
        assert_eq!(report.observations, 1);
        assert_eq!(report.words, 1);
        assert_eq!(repository.load_word_skills("portuguese").unwrap(), expected);
    }

    #[test]
    fn reset_adaptativo_preserva_historico_e_nao_ressuscita_no_rebuild() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        for word in ["casa", "tempo"] {
            repository
                .save_session_with_observations(
                    &TestConfig::default(),
                    &TestStatus::Failed {
                        ended_at_ms: 1_000,
                        word_index: 0,
                    },
                    Metrics::default(),
                    &[word_observation(word, true)],
                )
                .unwrap();
        }

        repository.reset_word_model("portuguese", "casa").unwrap();
        assert!(
            repository
                .word_detail("portuguese", "casa")
                .unwrap()
                .is_none()
        );
        assert!(
            repository
                .word_detail("portuguese", "tempo")
                .unwrap()
                .is_some()
        );
        repository.rebuild_adaptive_projections().unwrap();
        assert!(
            repository
                .word_detail("portuguese", "casa")
                .unwrap()
                .is_none()
        );
        assert_eq!(repository.statistics_overview().unwrap().completed_tests, 0);
        assert_eq!(
            repository
                .connection
                .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            2
        );

        repository.reset_adaptive_model().unwrap();
        assert!(repository.load_all_word_skills().unwrap().is_empty());
        repository.rebuild_adaptive_projections().unwrap();
        assert!(repository.load_all_word_skills().unwrap().is_empty());
        assert_eq!(
            repository
                .connection
                .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn reset_de_palavra_reverte_por_inteiro_se_a_reconstrucao_falhar() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        for word in ["casa", "tempo"] {
            repository
                .save_session_with_observations(
                    &TestConfig::default(),
                    &TestStatus::Failed {
                        ended_at_ms: 1_000,
                        word_index: 0,
                    },
                    Metrics::default(),
                    &[word_observation(word, true)],
                )
                .unwrap();
        }
        let before = repository.load_all_word_skills().unwrap();
        repository
            .connection
            .execute_batch(
                "CREATE TEMP TRIGGER impedir_reset
                 BEFORE INSERT ON word_skill
                 BEGIN SELECT RAISE(ABORT, 'falha injetada'); END;",
            )
            .unwrap();

        assert!(repository.reset_word_model("portuguese", "casa").is_err());
        assert_eq!(repository.load_all_word_skills().unwrap(), before);
        assert_eq!(
            repository
                .connection
                .query_row("SELECT COUNT(*) FROM adaptive_resets", [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn baseline_por_idioma_so_ativa_com_amostras_suficientes() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        for index in 0..8 {
            repository
                .save_session_with_observations(
                    &TestConfig::default(),
                    &TestStatus::Failed {
                        ended_at_ms: index,
                        word_index: 0,
                    },
                    Metrics::default(),
                    &[WordObservationRecord {
                        language: "portuguese".into(),
                        word: format!("palavra{index}"),
                        confirmed_error: false,
                        corrections: 0,
                        active_ms: 800,
                        afk_ms: 0,
                        planning_ms: 0,
                        fluent_ms: 800,
                        correction_ms: 0,
                        input_events: 4,
                        corrective_events: 0,
                        censored: false,
                        grapheme_count: 4,
                        fast_success: false,
                        slow: false,
                        latency_ratio: None,
                        evidence_weight: 1.0,
                        selection_source: None,
                        selection_propensity: None,
                        mechanics: Vec::new(),
                        patterns: Vec::new(),
                    }],
                )
                .unwrap();
        }
        let mut discarded = word_observation("cancelada", true);
        discarded.active_ms = 1;
        discarded.fluent_ms = 1;
        discarded.grapheme_count = 40;
        discarded.censored = true;
        discarded.evidence_weight = 0.0;
        repository
            .save_session_with_observations(
                &TestConfig::default(),
                &TestStatus::Running { started_at_ms: 0 },
                Metrics::default(),
                &[discarded],
            )
            .unwrap();
        assert_eq!(
            repository.baseline_ms_per_grapheme("portuguese").unwrap(),
            Some(200.0)
        );
    }

    #[test]
    fn baseline_limita_memoria_sem_perder_as_taxas_historicas() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let session_id = repository
            .save_session(
                &TestConfig::default(),
                &TestStatus::Completed { ended_at_ms: 1 },
                Metrics::default(),
            )
            .unwrap();
        let transaction = repository.connection.unchecked_transaction().unwrap();
        for index in 0..(BASELINE_LATENCY_WINDOW + 100) {
            let mut observation = word_observation(&format!("palavra{index}"), index == 0);
            observation.grapheme_count = 4;
            insert_word_observation(&transaction, session_id, &observation).unwrap();
        }
        transaction.commit().unwrap();

        let baseline = repository.baseline_profile("portuguese").unwrap();
        assert_eq!(baseline.latency_samples.len(), BASELINE_LATENCY_WINDOW);
        assert_eq!(baseline.uncorrected_samples, 1);
    }

    #[test]
    fn mecanica_e_materializada_separada_da_palavra() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        repository
            .save_session_with_observations(
                &TestConfig::default(),
                &TestStatus::Completed { ended_at_ms: 900 },
                Metrics::default(),
                &[WordObservationRecord {
                    language: "portuguese".into(),
                    word: "ação".into(),
                    confirmed_error: false,
                    corrections: 1,
                    active_ms: 900,
                    afk_ms: 0,
                    planning_ms: 0,
                    fluent_ms: 700,
                    correction_ms: 200,
                    input_events: 6,
                    corrective_events: 1,
                    censored: false,
                    grapheme_count: 4,
                    fast_success: false,
                    slow: false,
                    latency_ratio: None,
                    evidence_weight: 1.0,
                    selection_source: None,
                    selection_propensity: None,
                    mechanics: vec![MechanicObservationRecord {
                        mechanic: "til".into(),
                        confirmed_error: false,
                        corrected: true,
                    }],
                    patterns: Vec::new(),
                }],
            )
            .unwrap();

        let skills = repository.load_all_mechanic_skills().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].0, "portuguese");
        assert_eq!(skills[0].1, "til");
        assert_eq!(skills[0].2.corrected_error_mass, 1.0);
        assert_eq!(skills[0].2.distinct_words, vec!["ação"]);
    }

    #[test]
    fn revisao_conta_sessoes_limpas_e_erro_reinicia_a_sequencia() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let mut record = WordObservationRecord {
            language: "portuguese".into(),
            word: "casa".into(),
            confirmed_error: false,
            corrections: 0,
            active_ms: 500,
            afk_ms: 0,
            planning_ms: 0,
            fluent_ms: 500,
            correction_ms: 0,
            input_events: 4,
            corrective_events: 0,
            censored: false,
            grapheme_count: 4,
            fast_success: false,
            slow: false,
            latency_ratio: None,
            evidence_weight: 1.0,
            selection_source: None,
            selection_propensity: None,
            mechanics: Vec::new(),
            patterns: Vec::new(),
        };
        for ended_at_ms in [500, 1_000] {
            repository
                .save_session_with_observations(
                    &TestConfig::default(),
                    &TestStatus::Completed { ended_at_ms },
                    Metrics::default(),
                    &[record.clone()],
                )
                .unwrap();
        }
        assert_eq!(
            repository.load_all_review_states().unwrap()[0]
                .2
                .consecutive_clean_sessions,
            2
        );
        record.confirmed_error = true;
        repository
            .save_session_with_observations(
                &TestConfig::default(),
                &TestStatus::Failed {
                    ended_at_ms: 1_500,
                    word_index: 0,
                },
                Metrics::default(),
                &[record],
            )
            .unwrap();
        assert_eq!(
            repository.load_all_review_states().unwrap()[0]
                .2
                .consecutive_clean_sessions,
            0
        );
    }

    #[test]
    fn avaliacao_ancora_e_agendada_sem_escolha_do_usuario() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let config = TestConfig::default();
        assert_eq!(
            repository
                .next_session_kind(&config, &eligible_words())
                .unwrap(),
            SessionKind::Practice
        );
        let quote = TestConfig {
            mode: TestMode::Quote,
            ..config.clone()
        };
        assert_eq!(
            repository
                .next_session_kind(&quote, &eligible_words())
                .unwrap(),
            SessionKind::Practice
        );
        for ended_at_ms in 1..=3 {
            repository
                .save_session(
                    &config,
                    &TestStatus::Completed { ended_at_ms },
                    Metrics::default(),
                )
                .unwrap();
        }
        assert_eq!(
            repository
                .next_session_kind(&config, &eligible_words())
                .unwrap(),
            SessionKind::Transfer
        );
        for ended_at_ms in 4..=7 {
            repository
                .save_session(
                    &config,
                    &TestStatus::Completed { ended_at_ms },
                    Metrics::default(),
                )
                .unwrap();
        }
        assert_eq!(
            repository
                .next_session_kind(&config, &eligible_words())
                .unwrap(),
            SessionKind::Assessment
        );
    }

    #[test]
    fn retencao_so_e_agendada_quando_existe_revisao_vencida() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let config = TestConfig::default();
        for ended_at_ms in 1..=11 {
            repository
                .save_session(
                    &config,
                    &TestStatus::Completed { ended_at_ms },
                    Metrics::default(),
                )
                .unwrap();
        }
        assert_eq!(
            repository
                .next_session_kind(&config, &eligible_words())
                .unwrap(),
            SessionKind::Transfer
        );
        repository
            .connection
            .execute(
                "INSERT INTO skill_review (
                    language, word, last_seen_unix_s, last_session_id, consecutive_clean_sessions
                 ) VALUES ('english', 'house', ?1, 11, 1)",
                [Local::now().timestamp() - 3 * 86_400],
            )
            .unwrap();
        assert_eq!(
            repository
                .next_session_kind(&config, &eligible_words())
                .unwrap(),
            SessionKind::Transfer
        );
        repository
            .connection
            .execute(
                "INSERT INTO skill_review (
                    language, word, last_seen_unix_s, last_session_id, consecutive_clean_sessions
                 ) VALUES ('portuguese', 'fora-do-pacote', ?1, 11, 1)",
                [Local::now().timestamp() - 3 * 86_400],
            )
            .unwrap();
        assert_eq!(
            repository
                .next_session_kind(&config, &eligible_words())
                .unwrap(),
            SessionKind::Transfer
        );
        repository
            .connection
            .execute(
                "INSERT INTO skill_review (
                    language, word, last_seen_unix_s, last_session_id, consecutive_clean_sessions
                 ) VALUES ('portuguese', 'casa', ?1, 11, 1)",
                [Local::now().timestamp() - 3 * 86_400],
            )
            .unwrap();
        assert_eq!(
            repository
                .next_session_kind(&config, &eligible_words())
                .unwrap(),
            SessionKind::Retention
        );
    }
}
