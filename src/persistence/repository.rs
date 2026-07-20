use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, OpenOptions},
    path::Path,
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use anyhow::{Context, Result};
use chrono::{Datelike, Local};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::adaptive::{
    MechanicSkill, NgramSkill, Observation, PersonalBaseline, ReviewState, SelectionSource,
    WordSkill, lexical_ngrams,
};
use crate::gamification::{StreakState, XpState, award};
use crate::persistence::{RawEvent, RawEventCodec};
use crate::typing::{Metrics, TestConfig, TestStatus};

pub struct Repository {
    connection: Connection,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionProvenance {
    pub seed: u64,
    pub stimuli: Vec<String>,
    pub policy_version: u16,
    pub kind: SessionKind,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RebuildReport {
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
    pub recent_tests: Vec<SessionSummary>,
    pub priority_words: Vec<PriorityWord>,
    pub priority_patterns: Vec<PriorityPattern>,
    pub total_xp: u64,
    pub level: u64,
    pub streak: u16,
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
    pub word: String,
    pub difficulty: f64,
    pub confirmed_errors: f64,
    pub corrections: f64,
    pub observations: u32,
    pub effective_exposures: f64,
    pub uncorrected_error_rate: f64,
    pub corrected_error_rate: f64,
    pub estimated_session_chance: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PriorityPattern {
    pub pattern: String,
    pub kind: &'static str,
    pub difficulty: f64,
    pub effective_exposures: f64,
    pub uncorrected_error_rate: f64,
    pub corrected_error_rate: f64,
    pub distinct_words: usize,
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
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MechanicObservationRecord {
    pub mechanic: String,
    pub confirmed_error: bool,
    pub corrected: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PersonalBaselineProfile {
    pub rates: PersonalBaseline,
    latency_samples: Vec<(u16, f64)>,
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
}

impl Repository {
    /// Valida estrutura, versão, integridade do SQLite e blobs brutos sem
    /// executar migrações nem alterar o banco.
    pub fn doctor(path: &Path) -> Result<()> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        validate_schema_version(&connection)?;
        let quick_check =
            connection.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))?;
        anyhow::ensure!(quick_check == "ok", "integridade do SQLite: {quick_check}");
        let mut statement = connection.prepare(
            "SELECT codec_version, uncompressed_size, blob FROM raw_events ORDER BY session_id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, u16>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (version, size, blob) in rows {
            let size =
                usize::try_from(size).context("tamanho negativo nos eventos brutos persistidos")?;
            RawEventCodec::decode(version, size, &blob)?;
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
        validate_schema_version(&connection)?;
        if !new_database {
            restrict_file(path)?;
        }
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&connection)?;
        Ok(Self { connection })
    }

    pub fn save_session(
        &self,
        config: &TestConfig,
        status: &TestStatus,
        metrics: Metrics,
    ) -> Result<i64> {
        self.save_session_with_observations(config, status, metrics, &[])
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
            TestStatus::Running { .. } => "restart",
            TestStatus::Completed { .. } => "completed",
            TestStatus::Failed { .. } => "failed",
        };
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO sessions (
                terminal_state, config_toml, elapsed_ms, wpm, raw_wpm, accuracy,
                correct_chars, incorrect_chars, extra_chars, missed_chars,
                metrics_version, adaptive_version, codec_version, session_kind,
                seed_hex, stimuli_json, policy_version
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, 2, ?11, ?12, ?13, ?14, ?15)",
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
                provenance.policy_version,
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
            transaction.execute(
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
                    serde_json::to_string(&record.mechanics)?,
                    record.planning_ms as i64,
                    record.fluent_ms as i64,
                    record.correction_ms as i64,
                    record.input_events,
                    record.corrective_events,
                    record.censored,
                ],
            )?;

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
            let observation = Observation {
                confirmed_error: record.confirmed_error,
                corrected: record.corrections > 0,
                fast_success: record.fast_success,
                slow: record.slow,
                latency_ratio: record.latency_ratio,
                evidence_weight: record.evidence_weight,
            };
            let mut skill = previous;
            skill.observe(observation);
            let state = postcard::to_allocvec(&skill)?;
            transaction.execute(
                "INSERT INTO word_skill (language, word, state) VALUES (?1, ?2, ?3)
                 ON CONFLICT(language, word) DO UPDATE SET state = excluded.state",
                params![record.language, record.word, state],
            )?;
            for ngram in lexical_ngrams(&record.word) {
                let mut ngram_skill = transaction
                    .query_row(
                        "SELECT state FROM ngram_skill WHERE language = ?1 AND ngram = ?2",
                        params![record.language, ngram],
                        |row| row.get::<_, Vec<u8>>(0),
                    )
                    .optional()?
                    .map(|bytes| postcard::from_bytes::<NgramSkill>(&bytes))
                    .transpose()?
                    .unwrap_or_default();
                ngram_skill.observe(&record.word, observation);
                transaction.execute(
                    "INSERT INTO ngram_skill (language, ngram, state) VALUES (?1, ?2, ?3)
                     ON CONFLICT(language, ngram) DO UPDATE SET state = excluded.state",
                    params![record.language, ngram, postcard::to_allocvec(&ngram_skill)?,],
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
                "SELECT seed_hex, stimuli_json, policy_version, session_kind
                 FROM sessions WHERE id = ?1",
                [session_id],
                |row| {
                    let seed_hex = row.get::<_, String>(0)?;
                    let stimuli_json = row.get::<_, String>(1)?;
                    Ok((
                        seed_hex,
                        stimuli_json,
                        row.get::<_, u16>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
            .map(|(seed_hex, stimuli_json, policy_version, kind)| {
                Ok(SessionProvenance {
                    seed: u64::from_str_radix(&seed_hex, 16)?,
                    stimuli: serde_json::from_str(&stimuli_json)?,
                    policy_version,
                    kind: session_kind_from_db(&kind),
                })
            })
            .transpose()
    }

    /// Recria todas as projeções adaptativas em memória e as troca numa única
    /// transação. Blobs brutos presentes são decodificados e validados antes
    /// de qualquer estado existente ser removido.
    pub fn rebuild_adaptive_projections(&self) -> Result<RebuildReport> {
        let mut raw_statement = self.connection.prepare(
            "SELECT codec_version, uncompressed_size, blob FROM raw_events ORDER BY session_id",
        )?;
        let raw_rows = raw_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, u16>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (version, size, blob) in raw_rows {
            let size =
                usize::try_from(size).context("tamanho negativo nos eventos brutos persistidos")?;
            RawEventCodec::decode(version, size, &blob)?;
        }

        #[derive(Debug)]
        struct StoredObservation {
            language: String,
            word: String,
            observation: Observation,
            corrections: u32,
            mechanics: Vec<MechanicObservationRecord>,
            session_id: i64,
            observed_at: i64,
            censored: bool,
        }

        let mut statement = self.connection.prepare(
            "SELECT wo.language, wo.word, wo.confirmed_error, wo.corrections,
                    wo.fast_success, wo.slow, wo.latency_ratio, wo.evidence_weight,
                    wo.mechanics_json, wo.session_id, unixepoch(s.created_at), wo.censored
             FROM word_observations wo
             JOIN sessions s ON s.id = wo.session_id
             ORDER BY wo.session_id, wo.id",
        )?;
        let observations = statement
            .query_map([], |row| {
                let mechanics_json = row.get::<_, String>(8)?;
                let mechanics = serde_json::from_str(&mechanics_json).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                let corrections = row.get::<_, u32>(3)?;
                Ok(StoredObservation {
                    language: row.get(0)?,
                    word: row.get(1)?,
                    observation: Observation {
                        confirmed_error: row.get(2)?,
                        corrected: corrections > 0,
                        fast_success: row.get(4)?,
                        slow: row.get(5)?,
                        latency_ratio: row.get(6)?,
                        evidence_weight: row.get(7)?,
                    },
                    corrections,
                    mechanics,
                    session_id: row.get(9)?,
                    observed_at: row.get(10)?,
                    censored: row.get(11)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut words = HashMap::<(String, String), WordSkill>::new();
        let mut ngrams = HashMap::<(String, String), NgramSkill>::new();
        let mut mechanics = HashMap::<(String, String), MechanicSkill>::new();
        let mut reviews = BTreeMap::<(i64, String, String), (bool, i64)>::new();
        for record in &observations {
            words
                .entry((record.language.clone(), record.word.clone()))
                .or_default()
                .observe(record.observation);
            for ngram in lexical_ngrams(&record.word) {
                ngrams
                    .entry((record.language.clone(), ngram))
                    .or_default()
                    .observe(&record.word, record.observation);
            }
            for mechanic in &record.mechanics {
                mechanics
                    .entry((record.language.clone(), mechanic.mechanic.clone()))
                    .or_default()
                    .observe(
                        &record.word,
                        mechanic.confirmed_error,
                        mechanic.corrected,
                        record.observation.evidence_weight,
                    );
            }
            if record.observation.evidence_weight > 0.0 && !record.censored {
                reviews
                    .entry((
                        record.session_id,
                        record.language.clone(),
                        record.word.clone(),
                    ))
                    .and_modify(|(clean, _)| {
                        *clean &= !record.observation.confirmed_error && record.corrections == 0;
                    })
                    .or_insert((
                        !record.observation.confirmed_error && record.corrections == 0,
                        record.observed_at,
                    ));
            }
        }

        let report = RebuildReport {
            observations: observations.len(),
            words: words.len(),
            ngrams: ngrams.len(),
            mechanics: mechanics.len(),
        };
        let transaction = self.connection.unchecked_transaction()?;
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
        transaction.commit()?;
        Ok(report)
    }

    /// Baseline robusto (mediana aproximada) por idioma e tamanho. Só entra em
    /// ação após haver oito amostras, preservando o início frio.
    pub fn baseline_ms_per_grapheme(&self, language: &str) -> Result<Option<f64>> {
        Ok(self.baseline_profile(language)?.latency_ms_per_grapheme(0))
    }

    pub fn baseline_profile(&self, language: &str) -> Result<PersonalBaselineProfile> {
        let mut statement = self.connection.prepare(
            "SELECT grapheme_count, active_ms * 1.0 / grapheme_count,
                    confirmed_error, corrections
             FROM word_observations
             WHERE language = ?1 AND active_ms > 0 AND grapheme_count > 0",
        )?;
        let rows = statement
            .query_map([language], |row| {
                Ok((
                    row.get::<_, u16>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, u32>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let prior = PersonalBaseline::default();
        let exposures = rows.len() as f64;
        let prior_strength = 24.0;
        let uncorrected = rows.iter().filter(|(_, _, error, _)| *error).count() as f64;
        let corrected = rows
            .iter()
            .filter(|(_, _, error, corrections)| !*error && *corrections > 0)
            .count() as f64;
        Ok(PersonalBaselineProfile {
            rates: PersonalBaseline {
                uncorrected_error_rate: (prior.uncorrected_error_rate * prior_strength
                    + uncorrected)
                    / (prior_strength + exposures),
                corrected_error_rate: (prior.corrected_error_rate * prior_strength + corrected)
                    / (prior_strength + exposures),
            },
            latency_samples: rows
                .into_iter()
                .map(|(length, latency, _, _)| (length, latency))
                .collect(),
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
    pub fn next_session_kind(&self, config: &TestConfig) -> Result<SessionKind> {
        if matches!(config.mode, crate::typing::TestMode::Quote) {
            return Ok(SessionKind::Transfer);
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
        let has_due_review = self
            .load_all_review_states()?
            .into_iter()
            .any(|(_, _, state)| state.value_at(Local::now().timestamp()) > 0.0);
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
                        encoded.len(),
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
                        encoded.len(),
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
        let assessment_count = self.connection.query_row(
            "SELECT COUNT(*) FROM sessions
             WHERE terminal_state = 'completed' AND session_kind = 'assessment'",
            [],
            |row| row.get::<_, u64>(0),
        )?;
        let assessments_only = assessment_count >= 2;
        let mut overview = self.connection.query_row(
            "SELECT
                COUNT(*),
                COALESCE(SUM(elapsed_ms), 0),
                COALESCE(AVG(CASE WHEN ?1 = 0 OR session_kind = 'assessment' THEN wpm END), 0),
                COALESCE(AVG(CASE WHEN ?1 = 0 OR session_kind = 'assessment' THEN accuracy END), 0),
                COALESCE(MAX(CASE WHEN ?1 = 0 OR session_kind = 'assessment' THEN wpm END), 0)
             FROM sessions
             WHERE terminal_state = 'completed'",
            [assessments_only],
            |row| {
                Ok(StatisticsOverview {
                    completed_tests: row.get(0)?,
                    comparable_tests: 0,
                    active_ms: row.get::<_, i64>(1)? as u64,
                    average_wpm: row.get(2)?,
                    average_accuracy: row.get(3)?,
                    best_wpm: row.get(4)?,
                    recent_tests: Vec::new(),
                    priority_words: Vec::new(),
                    priority_patterns: Vec::new(),
                    total_xp: 0,
                    level: 0,
                    streak: 0,
                })
            },
        )?;
        overview.comparable_tests = if assessments_only {
            assessment_count
        } else {
            overview.completed_tests
        };
        let mut statement = self.connection.prepare(
            "SELECT id, elapsed_ms, wpm, accuracy, raw_wpm, correct_chars,
                    incorrect_chars, extra_chars, config_toml, session_kind
             FROM sessions
             WHERE terminal_state = 'completed'
               AND (?1 = 0 OR session_kind = 'assessment')
             ORDER BY id DESC
             LIMIT 12",
        )?;
        overview.recent_tests = statement
            .query_map([assessments_only], |row| {
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
        overview.recent_tests.reverse();
        overview.priority_words = self.priority_words()?;
        overview.priority_patterns = self.priority_patterns()?;
        let (xp, streak) = self.progress()?;
        overview.total_xp = xp.total;
        overview.level = crate::gamification::level_from_total_xp(xp.total);
        overview.streak = streak.current;
        Ok(overview)
    }

    fn priority_words(&self) -> Result<Vec<PriorityWord>> {
        let policy = crate::adaptive::AdaptivePolicy::default();
        let skills = self.load_all_word_skills()?;
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
                (word, skill, difficulty)
            })
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| right.2.total_cmp(&left.2));
        Ok(scored
            .into_iter()
            .filter_map(|(word, skill, difficulty)| {
                let exposures = skill.effective_exposures;
                (difficulty > 0.0).then_some(PriorityWord {
                    word,
                    difficulty,
                    confirmed_errors: skill.confirmed_errors,
                    corrections: skill.corrections,
                    observations: skill.observations,
                    effective_exposures: exposures,
                    uncorrected_error_rate: if exposures > 0.0 {
                        skill.uncorrected_error_mass / exposures
                    } else {
                        0.0
                    },
                    corrected_error_rate: if exposures > 0.0 {
                        skill.corrected_error_mass / exposures
                    } else {
                        0.0
                    },
                    estimated_session_chance: 0.0,
                })
            })
            .take(8)
            .collect())
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
                    pattern,
                    "sequência",
                    difficulty,
                    skill.effective_exposures,
                    skill.uncorrected_error_mass,
                    skill.corrected_error_mass,
                    skill.distinct_words.len(),
                ));
            }
        }
        for (language, pattern, skill) in mechanics {
            let baseline = baselines[&language];
            let difficulty = policy.mechanic_difficulty(&skill, baseline);
            if difficulty > 0.0 {
                patterns.push(pattern_diagnostic(
                    mechanic_label(&pattern),
                    "mecânica",
                    difficulty,
                    skill.effective_exposures,
                    skill.uncorrected_error_mass,
                    skill.corrected_error_mass,
                    skill.distinct_words.len(),
                ));
            }
        }
        patterns.sort_by(|left, right| right.difficulty.total_cmp(&left.difficulty));
        patterns.truncate(8);
        Ok(patterns)
    }
}

fn pattern_diagnostic(
    pattern: String,
    kind: &'static str,
    difficulty: f64,
    exposures: f64,
    uncorrected: f64,
    corrected: f64,
    distinct_words: usize,
) -> PriorityPattern {
    PriorityPattern {
        pattern,
        kind,
        difficulty,
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

const CURRENT_SCHEMA_VERSION: i64 = 6;

fn validate_schema_version(connection: &Connection) -> Result<Option<i64>> {
    let exists = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_version')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let versions = connection
        .prepare("SELECT version FROM schema_version")?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(
        versions.len() == 1,
        "a versão do banco deve possuir exatamente um registro"
    );
    let version = versions[0];
    anyhow::ensure!(version >= 1, "versão inválida do banco: {version}");
    anyhow::ensure!(
        version <= CURRENT_SCHEMA_VERSION,
        "o banco foi criado por uma versão mais nova do tuipe ({version}); esta versão suporta até {CURRENT_SCHEMA_VERSION}"
    );
    Ok(Some(version))
}

fn migrate(connection: &Connection) -> Result<()> {
    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
         INSERT INTO schema_version (version) SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM schema_version);
         CREATE TABLE IF NOT EXISTS sessions (
           id INTEGER PRIMARY KEY, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           terminal_state TEXT NOT NULL, config_toml TEXT NOT NULL, elapsed_ms INTEGER NOT NULL,
           wpm REAL NOT NULL, raw_wpm REAL NOT NULL, accuracy REAL NOT NULL,
           correct_chars INTEGER NOT NULL, incorrect_chars INTEGER NOT NULL,
           extra_chars INTEGER NOT NULL, missed_chars INTEGER NOT NULL,
           metrics_version INTEGER NOT NULL, adaptive_version INTEGER NOT NULL, codec_version INTEGER NOT NULL,
           session_kind TEXT NOT NULL DEFAULT 'practice',
           seed_hex TEXT NOT NULL DEFAULT '0000000000000000',
           stimuli_json TEXT NOT NULL DEFAULT '[]',
           policy_version INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS word_observations (
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
         CREATE TABLE IF NOT EXISTS word_skill (language TEXT NOT NULL, word TEXT NOT NULL, state BLOB NOT NULL, PRIMARY KEY(language, word));
         CREATE TABLE IF NOT EXISTS ngram_skill (language TEXT NOT NULL, ngram TEXT NOT NULL, state BLOB NOT NULL, PRIMARY KEY(language, ngram));
         CREATE TABLE IF NOT EXISTS mechanic_skill (language TEXT NOT NULL, mechanic TEXT NOT NULL, state BLOB NOT NULL, PRIMARY KEY(language, mechanic));
         CREATE TABLE IF NOT EXISTS skill_review (
           language TEXT NOT NULL, word TEXT NOT NULL, last_seen_unix_s INTEGER NOT NULL,
           last_session_id INTEGER NOT NULL REFERENCES sessions(id),
           consecutive_clean_sessions INTEGER NOT NULL,
           PRIMARY KEY(language, word)
         );
         CREATE TABLE IF NOT EXISTS favorite_quotes (quote_id INTEGER PRIMARY KEY);
         CREATE TABLE IF NOT EXISTS xp_state (id INTEGER PRIMARY KEY CHECK(id = 1), state BLOB NOT NULL);
         CREATE TABLE IF NOT EXISTS streak_state (id INTEGER PRIMARY KEY CHECK(id = 1), state BLOB NOT NULL);
         CREATE TABLE IF NOT EXISTS raw_events (session_id INTEGER PRIMARY KEY REFERENCES sessions(id), codec_version INTEGER NOT NULL, uncompressed_size INTEGER NOT NULL, blob BLOB NOT NULL);",
    )?;
    if !table_has_column(&transaction, "word_observations", "fast_success")? {
        transaction.execute(
            "ALTER TABLE word_observations ADD COLUMN fast_success INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !table_has_column(&transaction, "word_observations", "grapheme_count")? {
        transaction.execute(
            "ALTER TABLE word_observations ADD COLUMN grapheme_count INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !table_has_column(&transaction, "word_observations", "slow")? {
        transaction.execute(
            "ALTER TABLE word_observations ADD COLUMN slow INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !table_has_column(&transaction, "word_observations", "latency_ratio")? {
        transaction.execute(
            "ALTER TABLE word_observations ADD COLUMN latency_ratio REAL",
            [],
        )?;
    }
    if !table_has_column(&transaction, "sessions", "session_kind")? {
        transaction.execute(
            "ALTER TABLE sessions ADD COLUMN session_kind TEXT NOT NULL DEFAULT 'practice'",
            [],
        )?;
    }
    if !table_has_column(&transaction, "word_observations", "evidence_weight")? {
        transaction.execute(
            "ALTER TABLE word_observations ADD COLUMN evidence_weight REAL NOT NULL DEFAULT 1",
            [],
        )?;
    }
    if !table_has_column(&transaction, "word_observations", "selection_source")? {
        transaction.execute(
            "ALTER TABLE word_observations ADD COLUMN selection_source TEXT",
            [],
        )?;
    }
    if !table_has_column(&transaction, "word_observations", "selection_propensity")? {
        transaction.execute(
            "ALTER TABLE word_observations ADD COLUMN selection_propensity REAL",
            [],
        )?;
    }
    if !table_has_column(&transaction, "word_observations", "mechanics_json")? {
        transaction.execute(
            "ALTER TABLE word_observations ADD COLUMN mechanics_json TEXT NOT NULL DEFAULT '[]'",
            [],
        )?;
    }
    for (column, definition) in [
        ("seed_hex", "TEXT NOT NULL DEFAULT '0000000000000000'"),
        ("stimuli_json", "TEXT NOT NULL DEFAULT '[]'"),
        ("policy_version", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !table_has_column(&transaction, "sessions", column)? {
            transaction.execute(
                &format!("ALTER TABLE sessions ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    for (column, definition) in [
        ("planning_ms", "INTEGER NOT NULL DEFAULT 0"),
        ("fluent_ms", "INTEGER NOT NULL DEFAULT 0"),
        ("correction_ms", "INTEGER NOT NULL DEFAULT 0"),
        ("input_events", "INTEGER NOT NULL DEFAULT 0"),
        ("corrective_events", "INTEGER NOT NULL DEFAULT 0"),
        ("censored", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !table_has_column(&transaction, "word_observations", column)? {
            transaction.execute(
                &format!("ALTER TABLE word_observations ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    transaction.execute(
        "UPDATE schema_version SET version = ?1",
        [CURRENT_SCHEMA_VERSION],
    )?;
    transaction.commit()?;
    Ok(())
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

fn table_has_column(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    Ok(statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == column))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typing::TestStatus;

    #[test]
    fn migrations_create_a_session_repository() {
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
    fn banco_de_versao_futura_e_rejeitado_sem_downgrade() {
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

        assert!(error.contains("versão mais nova"));
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
    fn sessao_congela_seed_estimulos_politica_e_tipo() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let provenance = SessionProvenance {
            seed: u64::MAX - 3,
            stimuli: vec!["ação".into(), "casa".into()],
            policy_version: 2,
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
    fn overview_uses_only_completed_tests() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let completed = Metrics {
            duration_ms: 12_000,
            wpm: 80.0,
            accuracy: 95.0,
            ..Metrics::default()
        };
        repository
            .save_session(
                &TestConfig::default(),
                &TestStatus::Completed {
                    ended_at_ms: 12_000,
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

        assert_eq!(
            repository.statistics_overview().unwrap(),
            StatisticsOverview {
                completed_tests: 1,
                comparable_tests: 1,
                active_ms: 12_000,
                average_wpm: 80.0,
                average_accuracy: 95.0,
                best_wpm: 80.0,
                recent_tests: vec![SessionSummary {
                    id: 1,
                    elapsed_ms: 12_000,
                    wpm: 80.0,
                    accuracy: 95.0,
                    raw_wpm: 0.0,
                    correct_chars: 0,
                    incorrect_chars: 0,
                    extra_chars: 0,
                    config: TestConfig::default(),
                    kind: SessionKind::Practice,
                }],
                priority_words: Vec::new(),
                priority_patterns: Vec::new(),
                total_xp: 27,
                level: 1,
                streak: 1,
            }
        );
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
                    model_version: 2,
                    effective_exposures: 1.0,
                    uncorrected_error_mass: 1.0,
                    corrected_error_mass: 0.0,
                    latency_log_residual_sum: 0.0,
                    latency_weight: 0.0,
                },
            )]
        );
        let priority = repository.statistics_overview().unwrap().priority_words;
        assert_eq!(priority.len(), 1);
        assert_eq!(priority[0].word, "difícil");
        assert_eq!(priority[0].confirmed_errors, 1.0);

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
                    }],
                )
                .unwrap();
        }
        assert_eq!(
            repository.baseline_ms_per_grapheme("portuguese").unwrap(),
            Some(200.0)
        );
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
            repository.next_session_kind(&config).unwrap(),
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
            repository.next_session_kind(&config).unwrap(),
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
            repository.next_session_kind(&config).unwrap(),
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
            repository.next_session_kind(&config).unwrap(),
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
            repository.next_session_kind(&config).unwrap(),
            SessionKind::Retention
        );
    }
}
