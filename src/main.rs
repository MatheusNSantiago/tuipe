use std::{
    collections::HashMap,
    env,
    io::Write,
    path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
};
use rand::{SeedableRng, rngs::SmallRng, seq::IndexedRandom};
#[cfg(unix)]
use signal_hook::consts::signal::{SIGHUP, SIGTERM};
use termina::{
    escape::osc::{DynamicColorNumber, Osc},
    style::RgbColor,
};
use unicode_segmentation::UnicodeSegmentation;

use tuipe::{
    adaptive::{AdaptivePolicy, AdaptiveSampler, Observation, mechanics_for_token},
    content::{ContentCatalog, WordGenerator},
    persistence::{
        MechanicObservationRecord, Preferences, RawEvent, RawEventCodec, RawSessionEnd, Repository,
        SessionKind, SessionProvenance, StatisticsOverview, WordObservationRecord, paths,
    },
    typing::{
        ExternalEvent, InputEvent, KeyAction, QuoteLength, RecordedInputKind, TestEngine, TestMode,
        TestStatus,
    },
    ui,
};

fn main() -> Result<()> {
    if handle_cli()? {
        return Ok(());
    }
    let (config_path, database_path) = paths();
    let loaded = Preferences::load_recovering(&config_path)?;
    let startup_notice = loaded.quarantined.map(|path| {
        format!(
            "configuração inválida isolada em {}; padrões restaurados",
            path.display()
        )
    });
    let catalog = ContentCatalog::bundled()?;
    let repository = Repository::open(&database_path)?;
    let mut persistence = PersistenceWorker::start(database_path)?;
    let mut app = App::new(
        loaded.preferences,
        catalog,
        config_path,
        startup_notice,
        &repository,
    )?;
    let shutdown = shutdown_flag()?;

    ratatui::run(|terminal| {
        let _mouse_guard = scopeguard::guard((), |_| {
            let mut stdout = std::io::stdout();
            let _ = write!(
                stdout,
                "{}",
                Osc::ResetDynamicColor(DynamicColorNumber::TextCursorColor)
            );
            let _ = execute!(
                stdout,
                DisableBracketedPaste,
                DisableFocusChange,
                DisableMouseCapture,
                SetCursorStyle::DefaultUserShape
            );
        });
        execute!(
            std::io::stdout(),
            EnableBracketedPaste,
            EnableFocusChange,
            EnableMouseCapture,
            SetCursorStyle::BlinkingBar
        )?;
        run(terminal, &mut app, &repository, &mut persistence, &shutdown)
    })
}

fn shutdown_flag() -> Result<Arc<AtomicBool>> {
    let shutdown = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        signal_hook::flag::register(SIGTERM, Arc::clone(&shutdown))?;
        signal_hook::flag::register(SIGHUP, Arc::clone(&shutdown))?;
    }
    Ok(shutdown)
}

fn handle_cli() -> Result<bool> {
    let mut arguments = env::args().skip(1);
    let Some(command) = arguments.next() else {
        return Ok(false);
    };
    match command.as_str() {
        "-h" | "--help" | "help" => {
            println!(
                "tuipe — treinador de digitação adaptativo e offline\n\nUSO:\n    tuipe\n    tuipe doctor\n    tuipe backup [DESTINO]\n    tuipe --version\n\nCOMANDOS:\n    doctor           valida configuração, banco e eventos sem alterar dados\n    backup [DESTINO] cria uma cópia SQLite consistente e privada\n\nDentro do aplicativo, pressione esc para configurações e q para sair."
            );
        }
        "-V" | "--version" | "version" => println!("tuipe {}", env!("CARGO_PKG_VERSION")),
        "doctor" => {
            anyhow::ensure!(arguments.next().is_none(), "doctor não recebe argumentos");
            let (config_path, database_path) = paths();
            Preferences::validate(&config_path).context("configuração inválida")?;
            anyhow::ensure!(
                database_path.exists(),
                "o banco ainda não existe: {}",
                database_path.display()
            );
            Repository::doctor(&database_path).context("banco inválido")?;
            println!("configuração: ok\nbanco e eventos: ok");
        }
        "backup" => {
            let destination = arguments.next().map(PathBuf::from).unwrap_or_else(|| {
                PathBuf::from(format!(
                    "tuipe-backup-{}.db",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ))
            });
            anyhow::ensure!(
                arguments.next().is_none(),
                "backup recebe no máximo um destino"
            );
            let (_, database_path) = paths();
            let repository = Repository::open(&database_path)
                .with_context(|| format!("abrir banco em {}", database_path.display()))?;
            repository.backup(&destination)?;
            println!("backup criado em {}", destination.display());
        }
        _ => anyhow::bail!("comando desconhecido: {command}. Use tuipe --help"),
    }
    Ok(true)
}

struct App {
    preferences: Preferences,
    catalog: ContentCatalog,
    engine: TestEngine,
    started: Instant,
    persisted: bool,
    persistence_pending: bool,
    persistence_error: Option<String>,
    config_path: PathBuf,
    settings_open: bool,
    statistics_open: bool,
    statistics: StatisticsOverview,
    generator: Option<WordGenerator<SmallRng>>,
    selections: Vec<Option<tuipe::adaptive::WordSelection>>,
    adaptive: AdaptiveSampler,
    seed: u64,
    repeated_test: bool,
    session_kind: SessionKind,
    session_baseline: tuipe::persistence::PersonalBaselineProfile,
    startup_notice: Option<String>,
}

struct PersistJob {
    config: tuipe::typing::TestConfig,
    status: TestStatus,
    metrics: tuipe::typing::Metrics,
    observations: Vec<WordObservationRecord>,
    raw_events: Vec<RawEvent>,
    provenance: SessionProvenance,
}

enum PersistResult {
    Saved(Vec<WordObservationRecord>),
    Failed(String),
}

struct PersistenceWorker {
    sender: SyncSender<PersistJob>,
    receiver: Receiver<PersistResult>,
}

impl PersistenceWorker {
    fn start(database_path: PathBuf) -> Result<Self> {
        let (jobs_tx, jobs_rx) = mpsc::sync_channel::<PersistJob>(1);
        let (results_tx, results_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        thread::Builder::new()
            .name("tuipe-persistencia".into())
            .spawn(move || {
                let repository = match Repository::open(&database_path) {
                    Ok(repository) => {
                        let _ = ready_tx.send(Ok(()));
                        repository
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                };
                for job in jobs_rx {
                    let result = repository.save_session_with_provenance(
                        &job.config,
                        &job.status,
                        job.metrics,
                        &job.observations,
                        &job.raw_events,
                        &job.provenance,
                    );
                    let response = match result {
                        Ok(_) => PersistResult::Saved(job.observations),
                        Err(error) => PersistResult::Failed(error.to_string()),
                    };
                    if results_tx.send(response).is_err() {
                        break;
                    }
                }
            })?;
        ready_rx
            .recv()
            .context("worker de persistência encerrou durante a inicialização")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            sender: jobs_tx,
            receiver: results_rx,
        })
    }

    fn save(&self, job: PersistJob) -> Result<()> {
        self.sender
            .send(job)
            .context("worker de persistência não está disponível")
    }

    fn try_result(&self) -> Result<Option<PersistResult>> {
        match self.receiver.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                anyhow::bail!("worker de persistência encerrou inesperadamente")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct WordTiming {
    fluent_ms: u64,
    correction_ms: u64,
    planning_ms: u64,
    afk_ms: u64,
    input_events: u16,
    corrective_events: u16,
}

impl WordTiming {
    fn execution_ms(self) -> u64 {
        self.fluent_ms.saturating_add(self.correction_ms)
    }
}

impl App {
    fn new(
        preferences: Preferences,
        catalog: ContentCatalog,
        config_path: PathBuf,
        startup_notice: Option<String>,
        repository: &Repository,
    ) -> Result<Self> {
        let seed = rand::random();
        let mut adaptive = AdaptiveSampler::from_skills(
            AdaptivePolicy::default(),
            repository.load_all_word_skills()?,
        );
        adaptive.set_ngram_skills(repository.load_all_ngram_skills()?);
        adaptive.set_mechanic_skills(repository.load_all_mechanic_skills()?);
        adaptive.set_review_states(
            repository.load_all_review_states()?,
            chrono::Utc::now().timestamp(),
        );
        let mut session_baseline = tuipe::persistence::PersonalBaselineProfile::default();
        for language in ["portuguese", "english"] {
            let baseline = repository.baseline_profile(language)?;
            if language == preferences.test.language {
                session_baseline = baseline.clone();
            }
            adaptive.set_baseline(language, baseline.rates);
        }
        let session_kind = repository.next_session_kind(&preferences.test)?;
        let (engine, generator, selections) =
            new_test(&catalog, &preferences.test, seed, &adaptive, session_kind)?;
        Ok(Self {
            preferences,
            catalog,
            engine,
            started: Instant::now(),
            persisted: false,
            persistence_pending: false,
            persistence_error: None,
            config_path,
            settings_open: false,
            statistics_open: false,
            statistics: StatisticsOverview::default(),
            generator,
            selections,
            adaptive,
            seed,
            repeated_test: false,
            session_kind,
            session_baseline,
            startup_notice,
        })
    }

    fn persist_interrupted(&mut self, repository: &Repository, end: RawSessionEnd) -> Result<()> {
        if self.persisted || self.engine.recorded_events().is_empty() {
            return Ok(());
        }
        let raw_events =
            RawEventCodec::materialize(self.engine.recorded_events(), self.elapsed_ms(), end);
        repository.save_session_with_provenance(
            self.engine.config(),
            self.engine.status(),
            self.engine.metrics(),
            &[],
            &raw_events,
            &self.provenance(),
        )?;
        self.persisted = true;
        Ok(())
    }

    fn restart(&mut self, repository: &Repository) -> Result<()> {
        self.persist_interrupted(repository, RawSessionEnd::Restarted)?;
        self.session_baseline = repository.baseline_profile(&self.preferences.test.language)?;
        self.adaptive.set_baseline(
            self.preferences.test.language.clone(),
            self.session_baseline.rates,
        );
        self.seed = rand::random();
        let session_kind = repository.next_session_kind(&self.preferences.test)?;
        let (engine, generator, selections) = new_test(
            &self.catalog,
            &self.preferences.test,
            self.seed,
            &self.adaptive,
            session_kind,
        )?;
        self.engine = engine;
        self.generator = generator;
        self.selections = selections;
        self.started = Instant::now();
        self.persisted = false;
        self.persistence_pending = false;
        self.persistence_error = None;
        self.repeated_test = false;
        self.session_kind = session_kind;
        Ok(())
    }

    fn repeat(&mut self, repository: &Repository) -> Result<()> {
        self.persist_interrupted(repository, RawSessionEnd::Restarted)?;
        let (engine, generator, selections) = new_test(
            &self.catalog,
            &self.preferences.test,
            self.seed,
            &self.adaptive,
            SessionKind::Repeat,
        )?;
        self.engine = engine;
        self.generator = generator;
        self.selections = selections.into_iter().map(|_| None).collect();
        self.started = Instant::now();
        self.persisted = false;
        self.persistence_pending = false;
        self.persistence_error = None;
        self.repeated_test = true;
        self.session_kind = SessionKind::Repeat;
        Ok(())
    }

    fn apply_preference(
        &mut self,
        repository: &Repository,
        change: impl FnOnce(&mut Preferences),
    ) -> Result<()> {
        change(&mut self.preferences);
        self.preferences.save(&self.config_path)?;
        self.restart(repository)
    }

    fn elapsed_ms(&self) -> u64 {
        self.started
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    fn provenance(&self) -> SessionProvenance {
        SessionProvenance {
            seed: self.seed,
            stimuli: self
                .engine
                .targets()
                .iter()
                .map(|target| target.text.clone())
                .collect(),
            policy_version: 2,
            kind: self.session_kind,
        }
    }

    fn bloqueia_atalhos_do_resultado(&self) -> bool {
        let terminou_em = match self.engine.status() {
            TestStatus::Completed { ended_at_ms } | TestStatus::Failed { ended_at_ms, .. } => {
                *ended_at_ms
            }
            TestStatus::Ready | TestStatus::Running { .. } => return false,
        };
        self.elapsed_ms().saturating_sub(terminou_em) < 300
    }

    fn persistence_ui_state(&self) -> ui::PersistenceUiState {
        if self.persistence_pending {
            ui::PersistenceUiState::Saving
        } else if self.persistence_error.is_some() {
            ui::PersistenceUiState::Failed
        } else {
            ui::PersistenceUiState::Saved
        }
    }

    fn persistence_job(&self) -> PersistJob {
        let observations = self.observations(&self.session_baseline);
        let end = if matches!(self.engine.status(), TestStatus::Failed { .. }) {
            RawSessionEnd::Failed
        } else {
            RawSessionEnd::Completed
        };
        let ended_at_ms = match self.engine.status() {
            TestStatus::Completed { ended_at_ms } | TestStatus::Failed { ended_at_ms, .. } => {
                *ended_at_ms
            }
            _ => unreachable!("job só é criado para uma sessão terminada"),
        };
        PersistJob {
            config: self.engine.config().clone(),
            status: self.engine.status().clone(),
            metrics: self.engine.metrics(),
            observations,
            raw_events: RawEventCodec::materialize(self.engine.recorded_events(), ended_at_ms, end),
            provenance: self.provenance(),
        }
    }

    fn update(&mut self, event: InputEvent) {
        self.engine.update(event);
        if matches!(self.engine.status(), TestStatus::Running { .. }) {
            self.startup_notice = None;
        }
        if matches!(self.engine.config().mode, TestMode::Time { .. })
            && self
                .engine
                .targets()
                .len()
                .saturating_sub(self.engine.active_word())
                < 20
            && let Some(generator) = &mut self.generator
        {
            let generated = (0..40)
                .map(|_| generator.next_generated())
                .collect::<Vec<_>>();
            self.selections
                .extend(generated.iter().map(|word| word.selection.clone()));
            self.engine
                .append_words(generated.into_iter().map(|word| format!("{} ", word.text)));
        }
    }

    fn observations(
        &self,
        baseline: &tuipe::persistence::PersonalBaselineProfile,
    ) -> Vec<WordObservationRecord> {
        let timings = self.word_timings();
        let mut occurrences = HashMap::<String, usize>::new();
        for (word_index, (target, attempt)) in self
            .engine
            .targets()
            .iter()
            .zip(self.engine.attempts())
            .enumerate()
        {
            let terminal_failure = matches!(
                self.engine.status(),
                TestStatus::Failed { word_index: failed_index, .. } if *failed_index == word_index
            );
            let censored = !attempt.committed
                && !terminal_failure
                && matches!(self.engine.status(), TestStatus::Completed { .. })
                && !attempt.input.is_empty();
            if (attempt.committed || terminal_failure || censored)
                && let Some(word) = lexical_word(&target.text)
            {
                *occurrences.entry(word).or_default() += 1;
            }
        }
        self.engine
            .targets()
            .iter()
            .enumerate()
            .zip(self.engine.attempts())
            .filter_map(|((word_index, target), attempt)| {
                let terminal_failure = matches!(
                    self.engine.status(),
                    TestStatus::Failed { word_index: failed_index, .. } if *failed_index == word_index
                );
                let censored = !attempt.committed
                    && !terminal_failure
                    && matches!(self.engine.status(), TestStatus::Completed { .. })
                    && !attempt.input.is_empty();
                if !attempt.committed && !terminal_failure && !censored {
                    return None;
                }
                let word = lexical_word(&target.text)?;
                let timing = timings.get(word_index).copied().unwrap_or_default();
                let active_ms = timing.execution_ms();
                let grapheme_count = word.graphemes(true).count().try_into().unwrap_or(u16::MAX);
                let active_per_grapheme = active_ms as f64 / f64::from(grapheme_count.max(1));
                let latency_baseline = baseline.latency_ms_per_grapheme(grapheme_count);
                let typed = attempt.without_commit();
                let expected_prefix = target
                    .text
                    .graphemes(true)
                    .take(typed.graphemes(true).count())
                    .collect::<String>();
                let confirmed_error = terminal_failure
                    || (attempt.committed && typed != target.text)
                    || (censored && typed != expected_prefix);
                let fast_success = attempt.committed
                    && !confirmed_error
                    && attempt.corrections == 0
                    && latency_baseline
                        .is_some_and(|baseline| active_per_grapheme <= baseline * 0.8);
                let slow = latency_baseline
                    .is_some_and(|baseline| active_per_grapheme >= baseline * 1.5);
                let evidence_weight = if self.repeated_test || (censored && !confirmed_error) {
                    0.0
                } else {
                    let occurrence_weight = 1.0 / occurrences
                        .get(&word)
                        .copied()
                        .unwrap_or(1)
                        .max(1) as f64;
                    if censored {
                        let observed_fraction = typed.graphemes(true).count() as f64
                            / f64::from(grapheme_count.max(1));
                        occurrence_weight * observed_fraction.min(1.0) * 0.5
                    } else {
                        occurrence_weight
                    }
                };
                let selection = (!self.repeated_test)
                    .then(|| self.selections.get(word_index).cloned().flatten())
                    .flatten();
                let final_mechanics = mechanics_for_token(&attempt.without_commit());
                let mechanics = mechanics_for_token(&target.text)
                    .into_iter()
                    .map(|mechanic| {
                        let had_mistake = self.engine.recorded_events().iter().any(|event| {
                            event.word_index == word_index
                                && matches!(
                                    &event.kind,
                                    RecordedInputKind::Insert {
                                        expected: Some(expected),
                                        correct: false,
                                        ..
                                    }
                                    | RecordedInputKind::InsertDelta {
                                        expected: Some(expected),
                                        correct: false,
                                        ..
                                    } if mechanics_for_token(expected).contains(&mechanic)
                                )
                        });
                        let present_at_end = final_mechanics.contains(&mechanic);
                        MechanicObservationRecord {
                            mechanic,
                            confirmed_error: !present_at_end,
                            corrected: had_mistake && present_at_end,
                        }
                    })
                    .collect();
                Some(WordObservationRecord {
                    language: self.engine.config().language.clone(),
                    word,
                    confirmed_error,
                    corrections: attempt.corrections,
                    active_ms,
                    afk_ms: timing.afk_ms,
                    planning_ms: timing.planning_ms,
                    fluent_ms: timing.fluent_ms,
                    correction_ms: timing.correction_ms,
                    input_events: timing.input_events,
                    corrective_events: timing.corrective_events,
                    censored,
                    grapheme_count,
                    fast_success,
                    slow,
                    latency_ratio: latency_baseline.map(|baseline| active_per_grapheme / baseline),
                    evidence_weight,
                    selection_source: selection.as_ref().map(|selection| selection.source),
                    selection_propensity: selection.map(|selection| selection.propensity),
                    mechanics,
                })
            })
            .collect()
    }

    /// Separa execução e interrupção pela distribuição da própria sessão. Um
    /// intervalo entre palavras continua sendo latência de planejamento, não
    /// tempo motor da palavra seguinte.
    fn word_timings(&self) -> Vec<WordTiming> {
        #[derive(Clone, Copy)]
        struct Gap {
            word_index: usize,
            elapsed_ms: u64,
            interrupted: bool,
            same_word: bool,
            correction: bool,
        }

        let mut gaps = Vec::new();
        let mut previous_key = None::<(u64, usize, bool)>;
        let mut interrupted = false;
        let mut event_counts = vec![(0_u16, 0_u16); self.engine.targets().len()];
        for event in self.engine.recorded_events() {
            match &event.kind {
                RecordedInputKind::Focus { gained } => {
                    if !gained {
                        interrupted = true;
                    }
                }
                RecordedInputKind::Insert { .. }
                | RecordedInputKind::Delete { .. }
                | RecordedInputKind::InsertDelta { .. }
                | RecordedInputKind::DeleteDelta { .. } => {
                    let current_delete = matches!(
                        event.kind,
                        RecordedInputKind::Delete { .. } | RecordedInputKind::DeleteDelta { .. }
                    );
                    let counts = &mut event_counts[event.word_index];
                    counts.0 = counts.0.saturating_add(1);
                    counts.1 = counts.1.saturating_add(u16::from(current_delete));
                    if let Some((previous_at, previous_word, previous_delete)) = previous_key {
                        gaps.push(Gap {
                            word_index: event.word_index,
                            elapsed_ms: event.at_ms.saturating_sub(previous_at),
                            interrupted,
                            same_word: previous_word == event.word_index,
                            correction: current_delete || previous_delete,
                        });
                    }
                    previous_key = Some((event.at_ms, event.word_index, current_delete));
                    interrupted = false;
                }
                RecordedInputKind::Paste { .. } | RecordedInputKind::PasteRedacted { .. } => {}
            }
        }

        let mut log_intervals = gaps
            .iter()
            .filter(|gap| gap.same_word && !gap.interrupted && gap.elapsed_ms > 0)
            .map(|gap| (gap.elapsed_ms as f64).ln())
            .collect::<Vec<_>>();
        let pause_threshold = if log_intervals.len() >= 12 {
            let median_value = median(&mut log_intervals);
            let mut deviations = log_intervals
                .iter()
                .map(|value| (value - median_value).abs())
                .collect::<Vec<_>>();
            let mad = median(&mut deviations);
            (mad > f64::EPSILON).then_some(median_value + 3.5 * 1.4826 * mad)
        } else {
            None
        };

        let mut timings = vec![WordTiming::default(); self.engine.targets().len()];
        for gap in gaps {
            let is_pause = gap.interrupted
                || pause_threshold.is_some_and(|threshold| {
                    gap.elapsed_ms > 0 && (gap.elapsed_ms as f64).ln() > threshold
                });
            let timing = &mut timings[gap.word_index];
            if is_pause {
                timing.afk_ms = timing.afk_ms.saturating_add(gap.elapsed_ms);
            } else if !gap.same_word {
                timing.planning_ms = timing.planning_ms.saturating_add(gap.elapsed_ms);
            } else if gap.correction {
                timing.correction_ms = timing.correction_ms.saturating_add(gap.elapsed_ms);
            } else {
                timing.fluent_ms = timing.fluent_ms.saturating_add(gap.elapsed_ms);
            }
        }
        for (timing, (input_events, corrective_events)) in timings.iter_mut().zip(event_counts) {
            timing.input_events = input_events;
            timing.corrective_events = corrective_events;
        }
        timings
    }

    fn apply_observations(&mut self, observations: &[WordObservationRecord]) {
        let mut reviewed_words = HashMap::<(String, String), bool>::new();
        for record in observations {
            self.adaptive.observe(
                &record.language,
                &record.word,
                Observation {
                    confirmed_error: record.confirmed_error,
                    corrected: record.corrections > 0,
                    fast_success: record.fast_success,
                    slow: record.slow,
                    latency_ratio: record.latency_ratio,
                    evidence_weight: record.evidence_weight,
                },
            );
            for mechanic in &record.mechanics {
                self.adaptive.observe_mechanic(
                    &record.language,
                    &record.word,
                    &mechanic.mechanic,
                    mechanic.confirmed_error,
                    mechanic.corrected,
                    record.evidence_weight,
                );
            }
            if record.evidence_weight > 0.0 && !record.censored {
                reviewed_words
                    .entry((record.language.clone(), record.word.clone()))
                    .and_modify(|clean| {
                        *clean &= !record.confirmed_error && record.corrections == 0;
                    })
                    .or_insert(!record.confirmed_error && record.corrections == 0);
            }
        }
        let observed_at = chrono::Utc::now().timestamp();
        for ((language, word), clean) in reviewed_words {
            self.adaptive
                .record_review(&language, &word, clean, observed_at);
        }
    }

    fn load_statistics(&mut self, repository: &Repository) -> Result<()> {
        let mut statistics = repository.statistics_overview()?;
        let config = self.engine.config();
        if let Some(candidates) = self.catalog.word_pack(&config.language, &config.word_pack) {
            let draws = match config.mode {
                TestMode::Words { count } => usize::from(count),
                TestMode::Time { seconds } => {
                    ((statistics.average_wpm.max(30.0) * f64::from(seconds) / 60.0).ceil() as usize)
                        .max(1)
                }
                TestMode::Quote => 0,
            };
            let targets = statistics
                .priority_words
                .iter()
                .map(|word| word.word.clone())
                .collect::<Vec<_>>();
            let chances = self.adaptive.estimated_session_chances(
                &config.language,
                &targets,
                candidates,
                draws,
            );
            for word in &mut statistics.priority_words {
                word.estimated_session_chance = chances.get(&word.word).copied().unwrap_or(0.0);
            }
        }
        self.statistics = statistics;
        Ok(())
    }
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn lexical_word(value: &str) -> Option<String> {
    let lexical = value
        .trim_matches(|character: char| !character.is_alphabetic())
        .to_lowercase();
    (!lexical.is_empty()).then_some(lexical)
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    repository: &Repository,
    persistence: &mut PersistenceWorker,
    shutdown: &AtomicBool,
) -> Result<()> {
    let mut needs_draw = true;
    let mut last_drawn_second = 0;
    let mut last_size = terminal.size()?;
    let mut last_cursor_color = String::new();
    loop {
        if let Some(result) = persistence.try_result()? {
            app.persistence_pending = false;
            match result {
                PersistResult::Saved(observations) => {
                    app.apply_observations(&observations);
                    app.persisted = true;
                    app.persistence_error = None;
                }
                PersistResult::Failed(error) => app.persistence_error = Some(error),
            }
            needs_draw = true;
        }
        if shutdown.load(Ordering::Relaxed) {
            let terminal_session = matches!(
                app.engine.status(),
                TestStatus::Completed { .. } | TestStatus::Failed { .. }
            );
            if terminal_session && app.persisted {
                break;
            } else if terminal_session {
                if let Some(error) = &app.persistence_error {
                    anyhow::bail!("não foi possível salvar a sessão antes de sair: {error}");
                }
                if !app.persistence_pending {
                    persistence.save(app.persistence_job())?;
                    app.persistence_pending = true;
                    needs_draw = true;
                }
            } else {
                app.persist_interrupted(repository, RawSessionEnd::Quit)?;
                break;
            }
        }
        if needs_draw {
            let theme = app
                .catalog
                .theme(&app.preferences.theme)
                .context("o tema configurado não está disponível")?;
            if ui::uses_true_color() && theme.caret != last_cursor_color {
                set_cursor_color(&theme.caret)?;
                last_cursor_color.clone_from(&theme.caret);
            }
            terminal.draw(|frame| {
                ui::render(
                    frame,
                    &app.engine,
                    theme,
                    ui::RenderState {
                        settings_open: app.settings_open,
                        theme_name: &app.preferences.theme,
                        session_kind: app.session_kind,
                        persistence: app.persistence_ui_state(),
                        notice: app.startup_notice.as_deref(),
                    },
                );
                if app.statistics_open {
                    ui::render_statistics(frame, &app.statistics, theme);
                }
            })?;
            last_drawn_second = app.engine.elapsed_ms() / 1_000;
            needs_draw = false;
        }

        if !app.persisted
            && !app.persistence_pending
            && app.persistence_error.is_none()
            && matches!(
                app.engine.status(),
                TestStatus::Completed { .. } | TestStatus::Failed { .. }
            )
        {
            persistence.save(app.persistence_job())?;
            app.persistence_pending = true;
            needs_draw = true;
            continue;
        }

        if event::poll(Duration::from_millis(16))? {
            let event_changed_view = match event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        && handle_key(app, repository, key.code, key.modifiers)? =>
                {
                    break;
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => true,
                Event::Mouse(mouse) => handle_mouse(app, repository, mouse, terminal.size()?)?,
                Event::Resize(width, height)
                    if width != last_size.width || height != last_size.height =>
                {
                    last_size = ratatui::layout::Size::new(width, height);
                    true
                }
                Event::Resize(_, _) => false,
                Event::FocusGained => {
                    app.update(InputEvent::External {
                        event: ExternalEvent::Focus { gained: true },
                        at_ms: app.elapsed_ms(),
                    });
                    false
                }
                Event::FocusLost => {
                    app.update(InputEvent::External {
                        event: ExternalEvent::Focus { gained: false },
                        at_ms: app.elapsed_ms(),
                    });
                    false
                }
                Event::Paste(text) => {
                    app.update(InputEvent::External {
                        event: ExternalEvent::Paste { text },
                        at_ms: app.elapsed_ms(),
                    });
                    false
                }
                Event::Key(_) => false,
            };
            needs_draw |= event_changed_view;
        }

        let previous_status = app.engine.status().clone();
        app.update(InputEvent::Tick {
            at_ms: app.elapsed_ms(),
        });
        let current_second = app.engine.elapsed_ms() / 1_000;
        needs_draw |= app.engine.status() != &previous_status
            || (matches!(app.engine.status(), TestStatus::Running { .. })
                && current_second != last_drawn_second);
    }
    Ok(())
}

fn set_cursor_color(value: &str) -> Result<()> {
    let parsed = value
        .parse::<csscolorparser::Color>()
        .context("cor de cursor inválida no tema")?;
    let [red, green, blue, _] = parsed.to_rgba8();
    let command = Osc::ChangeDynamicColors(
        DynamicColorNumber::TextCursorColor,
        vec![RgbColor::new(red, green, blue).into()],
    );
    let mut stdout = std::io::stdout();
    write!(stdout, "{command}")?;
    stdout.flush()?;
    Ok(())
}

fn handle_mouse(
    app: &mut App,
    repository: &Repository,
    mouse: MouseEvent,
    terminal: ratatui::layout::Size,
) -> Result<bool> {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left)
        || !matches!(app.engine.status(), TestStatus::Ready)
    {
        return Ok(false);
    }

    let viewport = ratatui::layout::Rect::new(0, 0, terminal.width, terminal.height);
    let config_bar = ui::config_bar_area(viewport);
    if mouse.row != config_bar.y + 1 {
        return Ok(false);
    }

    let Some(cards) = ui::config_card_areas(viewport, &app.engine.config().mode) else {
        app.settings_open = true;
        return Ok(true);
    };
    let x = mouse.column;
    if (cards[0].x..cards[0].right()).contains(&x) {
        if matches!(app.engine.config().mode, TestMode::Quote) {
            return Ok(false);
        }
        let punctuation = x < cards[0].x + cards[0].width / 2;
        app.apply_preference(repository, |preferences| {
            if punctuation {
                preferences.test.punctuation = !preferences.test.punctuation;
            } else {
                preferences.test.numbers = !preferences.test.numbers;
            }
        })?;
    } else if (cards[1].x..cards[1].right()).contains(&x) {
        let third = ((x - cards[1].x) * 3 / cards[1].width).min(2);
        let mode = match third {
            0 => TestMode::Time { seconds: 30 },
            1 => TestMode::Words { count: 25 },
            _ => TestMode::Quote,
        };
        app.apply_preference(repository, |preferences| preferences.test.mode = mode)?;
    } else if (cards[2].x..cards[2].right()).contains(&x) {
        let quarter = ((x - cards[2].x) * 4 / cards[2].width).min(3) as usize;
        app.apply_preference(repository, |preferences| {
            preferences.test.mode = match preferences.test.mode {
                TestMode::Time { .. } => TestMode::Time {
                    seconds: [15, 30, 60, 120][quarter],
                },
                TestMode::Words { .. } => TestMode::Words {
                    count: [10, 25, 50, 100][quarter],
                },
                TestMode::Quote => {
                    preferences.test.quote_length = [
                        QuoteLength::All,
                        QuoteLength::Short,
                        QuoteLength::Medium,
                        QuoteLength::Long,
                    ][quarter];
                    TestMode::Quote
                }
            };
        })?;
    } else {
        return Ok(false);
    }
    Ok(true)
}

fn handle_key(
    app: &mut App,
    repository: &Repository,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<bool> {
    if app.statistics_open {
        if matches!(code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('s')) {
            app.statistics_open = false;
        }
        return Ok(false);
    }
    let terminal = matches!(
        app.engine.status(),
        TestStatus::Completed { .. } | TestStatus::Failed { .. }
    );
    if terminal && app.persistence_pending {
        return Ok(false);
    }
    if terminal && app.persistence_error.is_some() {
        if matches!(code, KeyCode::Char('r')) {
            app.persistence_error = None;
        }
        return Ok(false);
    }
    if app.settings_open {
        return handle_settings_key(app, repository, code);
    }
    if matches!(code, KeyCode::Esc) && !matches!(app.engine.status(), TestStatus::Running { .. }) {
        app.settings_open = true;
        return Ok(false);
    }
    if matches!(code, KeyCode::Char('c')) && modifiers.contains(KeyModifiers::CONTROL) {
        app.repeat(repository)?;
        return Ok(false);
    }
    let resultado_recente = app.bloqueia_atalhos_do_resultado();
    if matches!(code, KeyCode::Char('q'))
        && matches!(
            app.engine.status(),
            TestStatus::Completed { .. } | TestStatus::Failed { .. }
        )
        && !resultado_recente
    {
        return Ok(true);
    }
    if matches!(code, KeyCode::Enter) {
        app.restart(repository)?;
        return Ok(false);
    }
    if matches!(code, KeyCode::Char('r'))
        && matches!(
            app.engine.status(),
            TestStatus::Completed { .. } | TestStatus::Failed { .. }
        )
        && !resultado_recente
    {
        app.repeat(repository)?;
        return Ok(false);
    }
    if matches!(code, KeyCode::Char('s'))
        && matches!(
            app.engine.status(),
            TestStatus::Completed { .. } | TestStatus::Failed { .. }
        )
        && !resultado_recente
    {
        app.load_statistics(repository)?;
        app.statistics_open = true;
        return Ok(false);
    }

    let action = typing_action(code, modifiers);
    if let Some(action) = action {
        app.update(InputEvent::Key {
            action,
            at_ms: app.elapsed_ms(),
        });
    }
    Ok(false)
}

fn typing_action(code: KeyCode, modifiers: KeyModifiers) -> Option<KeyAction> {
    match code {
        KeyCode::Char(character)
            if modifiers.contains(KeyModifiers::CONTROL) && matches!(character, 'w' | 'h') =>
        {
            Some(KeyAction::DeleteWordBackward)
        }
        KeyCode::Char(character)
            if !modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            Some(KeyAction::Text(character.to_string()))
        }
        KeyCode::Backspace if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(KeyAction::DeleteWordBackward)
        }
        KeyCode::Backspace => Some(KeyAction::Backspace),
        _ => None,
    }
}

fn handle_settings_key(app: &mut App, repository: &Repository, code: KeyCode) -> Result<bool> {
    match code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Esc | KeyCode::Enter => app.settings_open = false,
        KeyCode::Char('m') => app.apply_preference(repository, |preferences| {
            preferences.test.mode = match preferences.test.mode {
                TestMode::Time { .. } => TestMode::Words { count: 25 },
                TestMode::Words { .. } => TestMode::Quote,
                TestMode::Quote => TestMode::Time { seconds: 30 },
            };
        })?,
        KeyCode::Char('v') => app.apply_preference(repository, |preferences| {
            preferences.test.mode = match preferences.test.mode {
                TestMode::Time { seconds } => TestMode::Time {
                    seconds: next(&[15, 30, 60, 120], seconds),
                },
                TestMode::Words { count } => TestMode::Words {
                    count: next(&[10, 25, 50, 100], count),
                },
                TestMode::Quote => {
                    preferences.test.quote_length = match preferences.test.quote_length {
                        QuoteLength::All => QuoteLength::Short,
                        QuoteLength::Short => QuoteLength::Medium,
                        QuoteLength::Medium => QuoteLength::Long,
                        QuoteLength::Long => QuoteLength::All,
                    };
                    TestMode::Quote
                }
            };
        })?,
        KeyCode::Char('d') => app.apply_preference(repository, |preferences| {
            preferences.test.difficulty = match preferences.test.difficulty {
                tuipe::typing::Difficulty::Normal => tuipe::typing::Difficulty::Expert,
                tuipe::typing::Difficulty::Expert => tuipe::typing::Difficulty::Master,
                tuipe::typing::Difficulty::Master => tuipe::typing::Difficulty::Normal,
            };
        })?,
        KeyCode::Char('p') if !matches!(app.engine.config().mode, TestMode::Quote) => app
            .apply_preference(repository, |preferences| {
                preferences.test.punctuation = !preferences.test.punctuation;
            })?,
        KeyCode::Char('n') if !matches!(app.engine.config().mode, TestMode::Quote) => app
            .apply_preference(repository, |preferences| {
                preferences.test.numbers = !preferences.test.numbers;
            })?,
        KeyCode::Char('a') => app.apply_preference(repository, |preferences| {
            preferences.test.adaptive = !preferences.test.adaptive;
        })?,
        KeyCode::Char('l') => app.apply_preference(repository, |preferences| {
            preferences.test.language = if preferences.test.language == "portuguese" {
                "english".into()
            } else {
                "portuguese".into()
            };
        })?,
        KeyCode::Char('k') => app.apply_preference(repository, |preferences| {
            preferences.test.word_pack = match preferences.test.word_pack.as_str() {
                "common" => "1k",
                "1k" => "5k",
                _ => "common",
            }
            .into();
        })?,
        KeyCode::Char('t') => {
            let names = app
                .catalog
                .theme_names()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let current = app.preferences.theme.clone();
            app.apply_preference(repository, |preferences| {
                let index = names.iter().position(|name| name == &current).unwrap_or(0);
                preferences.theme = names[(index + 1) % names.len()].clone();
            })?;
        }
        _ => {}
    }
    Ok(false)
}

fn next<T: Copy + PartialEq>(values: &[T], current: T) -> T {
    let index = values
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    values[(index + 1) % values.len()]
}

type GeneratedTest = (
    TestEngine,
    Option<WordGenerator<SmallRng>>,
    Vec<Option<tuipe::adaptive::WordSelection>>,
);

fn new_test(
    catalog: &ContentCatalog,
    config: &tuipe::typing::TestConfig,
    seed: u64,
    adaptive: &AdaptiveSampler,
    session_kind: SessionKind,
) -> Result<GeneratedTest> {
    let mut rng = SmallRng::seed_from_u64(seed);
    let (words, generator, selections) = match config.mode {
        TestMode::Quote => {
            let quotes = catalog.quotes(&config.language, config.quote_length);
            let quote = quotes.choose(&mut rng).context("language has no quotes")?;
            let words = quote
                .text
                .split_whitespace()
                .map(|word| format!("{word} "))
                .collect::<Vec<_>>();
            let selections = vec![None; words.len()];
            (without_last_commit(words), None, selections)
        }
        TestMode::Words { count } => {
            let mut generator = word_generator(catalog, config, rng, adaptive, session_kind)?;
            let (words, selections) = generate(&mut generator, usize::from(count));
            (without_last_commit(words), None, selections)
        }
        TestMode::Time { .. } => {
            let mut generator = word_generator(catalog, config, rng, adaptive, session_kind)?;
            // O buffer inicial precisa preencher três linhas reais também em
            // terminais ultrawide, como o gerador contínuo do Monkeytype.
            let (words, selections) = generate(&mut generator, 120);
            (words, Some(generator), selections)
        }
    };
    Ok((
        TestEngine::new(config.clone(), words),
        generator,
        selections,
    ))
}

fn word_generator(
    catalog: &ContentCatalog,
    config: &tuipe::typing::TestConfig,
    rng: SmallRng,
    adaptive: &AdaptiveSampler,
    session_kind: SessionKind,
) -> Result<WordGenerator<SmallRng>> {
    let configured_words = catalog
        .word_pack(&config.language, &config.word_pack)
        .context("configured word pack is unavailable")?;
    let partitioned = match session_kind {
        SessionKind::Transfer => configured_words
            .iter()
            .filter(|word| is_transfer_holdout(word))
            .cloned()
            .collect::<Vec<_>>(),
        SessionKind::Retention => adaptive.retention_candidates(&config.language, configured_words),
        SessionKind::Practice if config.adaptive => configured_words
            .iter()
            .filter(|word| !is_transfer_holdout(word))
            .cloned()
            .collect(),
        SessionKind::Assessment | SessionKind::Practice | SessionKind::Repeat => Vec::new(),
    };
    let words = if partitioned.len() >= 3 {
        partitioned.as_slice()
    } else {
        configured_words
    };
    let generator = WordGenerator::new(words, rng, config.punctuation, config.numbers);
    Ok(match session_kind {
        SessionKind::Assessment => generator.with_assessment(),
        SessionKind::Practice if config.adaptive => {
            generator.with_adaptive(&config.language, adaptive.clone())
        }
        SessionKind::Practice
        | SessionKind::Transfer
        | SessionKind::Retention
        | SessionKind::Repeat => generator,
    })
}

/// Partição FNV-1a estável: aproximadamente 10% do pack nunca entra na prática
/// adaptativa e fica reservado para medir transferência.
fn is_transfer_holdout(word: &str) -> bool {
    let hash = word
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    hash % 10 == 0
}

fn generate(
    generator: &mut WordGenerator<SmallRng>,
    count: usize,
) -> (Vec<String>, Vec<Option<tuipe::adaptive::WordSelection>>) {
    let generated = (0..count)
        .map(|_| generator.next_generated())
        .collect::<Vec<_>>();
    let selections = generated
        .iter()
        .map(|word| word.selection.clone())
        .collect();
    let words = generated
        .into_iter()
        .map(|word| format!("{} ", word.text))
        .collect();
    (words, selections)
}

fn without_last_commit(mut words: Vec<String>) -> Vec<String> {
    if let Some(last) = words.last_mut() {
        last.pop();
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_de_teste(config: tuipe::typing::TestConfig, words: &[&str]) -> App {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let preferences = Preferences {
            test: config.clone(),
            ..Preferences::default()
        };
        let mut app = App::new(
            preferences,
            ContentCatalog::bundled().unwrap(),
            temporary.path().join("config.toml"),
            None,
            &repository,
        )
        .unwrap();
        app.engine = TestEngine::new(config, words.iter().map(|word| (*word).to_owned()));
        app.selections = vec![None; words.len()];
        app
    }

    #[test]
    fn ctrl_w_and_ctrl_backspace_remove_the_active_word() {
        assert_eq!(
            typing_action(KeyCode::Char('w'), KeyModifiers::CONTROL),
            Some(KeyAction::DeleteWordBackward)
        );
        assert_eq!(
            typing_action(KeyCode::Backspace, KeyModifiers::CONTROL),
            Some(KeyAction::DeleteWordBackward)
        );
        assert_eq!(
            typing_action(KeyCode::Char('h'), KeyModifiers::CONTROL),
            Some(KeyAction::DeleteWordBackward)
        );
    }

    #[test]
    fn q_inicia_o_teste_em_vez_de_fechar_o_programa() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let mut app = app_de_teste(tuipe::typing::TestConfig::default(), &["que ", "tempo "]);

        let deve_sair = handle_key(
            &mut app,
            &repository,
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )
        .unwrap();

        assert!(!deve_sair);
        assert!(matches!(app.engine.status(), TestStatus::Running { .. }));
        assert_eq!(app.engine.attempts()[0].input, "q");
    }

    #[test]
    fn worker_persiste_sem_bloquear_o_estado_da_interface() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("worker.db");
        let worker = PersistenceWorker::start(path.clone()).unwrap();
        worker
            .save(PersistJob {
                config: tuipe::typing::TestConfig::default(),
                status: TestStatus::Completed { ended_at_ms: 1 },
                metrics: tuipe::typing::Metrics::default(),
                observations: Vec::new(),
                raw_events: Vec::new(),
                provenance: SessionProvenance::default(),
            })
            .unwrap();

        assert!(matches!(
            worker
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            PersistResult::Saved(_)
        ));
        let repository = Repository::open(&path).unwrap();
        assert_eq!(repository.statistics_overview().unwrap().completed_tests, 1);
    }

    #[test]
    fn falha_ao_salvar_exige_retry_antes_de_qualquer_atalho_do_resultado() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let mut app = app_de_teste(tuipe::typing::TestConfig::default(), &["que "]);
        app.engine.update(InputEvent::Key {
            action: KeyAction::Text("que ".into()),
            at_ms: 10,
        });
        app.engine.update(InputEvent::Tick { at_ms: 30_010 });
        app.persistence_error = Some("disco cheio".into());

        assert!(!handle_key(&mut app, &repository, KeyCode::Enter, KeyModifiers::NONE).unwrap());
        assert!(app.persistence_error.is_some());
        assert!(
            !handle_key(
                &mut app,
                &repository,
                KeyCode::Char('r'),
                KeyModifiers::NONE
            )
            .unwrap()
        );
        assert!(app.persistence_error.is_none());
    }

    #[test]
    fn particao_de_transferencia_e_estavel() {
        let first = (0..1_000)
            .filter(|index| is_transfer_holdout(&format!("palavra{index}")))
            .collect::<Vec<_>>();
        let second = (0..1_000)
            .filter(|index| is_transfer_holdout(&format!("palavra{index}")))
            .collect::<Vec<_>>();
        assert_eq!(first, second);
        assert!((70..=130).contains(&first.len()));
    }

    #[test]
    fn palavra_cortada_pelo_tempo_nao_vira_acerto() {
        let config = tuipe::typing::TestConfig {
            mode: TestMode::Time { seconds: 1 },
            difficulty: tuipe::typing::Difficulty::Normal,
            ..tuipe::typing::TestConfig::default()
        };
        let mut app = app_de_teste(config, &["casa ", "tempo "]);
        app.update(InputEvent::Key {
            action: KeyAction::Text("c".into()),
            at_ms: 0,
        });
        app.update(InputEvent::Tick { at_ms: 1_000 });
        let observations = app.observations(&Default::default());
        assert_eq!(observations.len(), 1);
        assert!(observations[0].censored);
        assert!(!observations[0].confirmed_error);
        assert_eq!(observations[0].evidence_weight, 0.0);
    }

    #[test]
    fn tempo_de_correcao_fica_separado_da_execucao_fluente() {
        let config = tuipe::typing::TestConfig {
            mode: TestMode::Time { seconds: 1 },
            difficulty: tuipe::typing::Difficulty::Normal,
            ..tuipe::typing::TestConfig::default()
        };
        let mut app = app_de_teste(config, &["casa ", "tempo "]);
        for (action, at_ms) in [
            (KeyAction::Text("c".into()), 0),
            (KeyAction::Text("x".into()), 100),
            (KeyAction::Backspace, 250),
            (KeyAction::Text("a".into()), 300),
            (KeyAction::Text("s".into()), 400),
            (KeyAction::Text("a".into()), 500),
            (KeyAction::Text(" ".into()), 600),
        ] {
            app.update(InputEvent::Key { action, at_ms });
        }
        app.update(InputEvent::Tick { at_ms: 1_000 });
        let observations = app.observations(&Default::default());
        assert_eq!(observations[0].fluent_ms, 400);
        assert_eq!(observations[0].correction_ms, 200);
        assert_eq!(observations[0].corrective_events, 1);
        assert_eq!(observations[0].input_events, 7);
    }
}
