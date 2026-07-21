use std::{
    collections::HashMap,
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(test)]
use std::collections::HashSet;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use anyhow::{Context, Result};
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
};
use rand::{SeedableRng, rngs::SmallRng, seq::IndexedRandom};
#[cfg(unix)]
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
use termina::{
    escape::osc::{DynamicColorNumber, Osc},
    style::RgbColor,
};
use tuipe::{
    adaptive::{AdaptivePolicy, AdaptiveSampler, CURRENT_POLICY_VERSION, Observation},
    content::{ContentCatalog, Quote, WordGenerator},
    persistence::{
        Preferences, RawEvent, RawEventCodec, RawSessionEnd, Repository, SessionDetail,
        SessionKind, SessionOutcome, SessionProvenance, StatisticsOverview, WordDetail,
        WordObservationRecord, paths, state_dir,
    },
    typing::{ExternalEvent, InputEvent, KeyAction, QuoteLength, TestEngine, TestMode, TestStatus},
    ui,
};

fn main() -> Result<()> {
    let diagnostics = state_dir();
    install_panic_reporter(diagnostics.clone());
    if let Err(error) = run_main() {
        let _ = write_diagnostic(&diagnostics, "erro fatal", &format!("{error:#}"));
        return Err(error);
    }
    Ok(())
}

fn run_main() -> Result<()> {
    if handle_cli()? {
        return Ok(());
    }
    ui::configure_terminal_color_output();
    let (config_path, database_path) = paths();
    let loaded = Preferences::load_recovering(&config_path)?;
    let mut notices = loaded
        .quarantined
        .map(|path| {
            format!(
                "configuração inválida isolada em {}; padrões restaurados",
                path.display()
            )
        })
        .into_iter()
        .collect::<Vec<_>>();
    let mut catalog = ContentCatalog::bundled()?;
    let themes_directory = config_path
        .parent()
        .expect("o caminho da configuração deve ter um diretório pai")
        .join("themes");
    notices.extend(catalog.load_user_themes(&themes_directory)?);
    let mut preferences = loaded.preferences;
    if catalog.theme(&preferences.theme).is_none() {
        notices.push(format!(
            "tema {} indisponível; arch restaurado",
            preferences.theme
        ));
        preferences.theme = "arch".into();
        preferences.save(&config_path)?;
    }
    let opened = Repository::open_recovering(&database_path)?;
    if let Some(path) = opened.quarantined {
        notices.push(format!(
            "histórico corrompido preservado em {}; um banco novo foi criado",
            path.display()
        ));
    }
    let repository = opened.repository;
    let policy_state = repository.adaptive_policy_state()?;
    if preferences.test.adaptive && policy_state.active_version != CURRENT_POLICY_VERSION {
        notices.push(
            "currículo adaptativo em modo seguro; seleções uniformes até a política ser restaurada"
                .into(),
        );
    }
    let startup_notice = (!notices.is_empty()).then(|| notices.join(" · "));
    let mut persistence = PersistenceWorker::start(database_path)?;
    let mut app = App::new(
        preferences,
        catalog,
        config_path,
        startup_notice,
        &repository,
    )?;
    let shutdown = shutdown_flag()?;
    ui::inicializar_capacidades_do_terminal();

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

fn install_panic_reporter(directory: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("panic sem mensagem");
        let location = info.location().map_or_else(
            || "local desconhecido".into(),
            |location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            },
        );
        let detail = format!(
            "{message}\nlocal: {location}\n\nbacktrace:\n{}",
            std::backtrace::Backtrace::force_capture()
        );
        let _ = write_diagnostic(&directory, "panic", &detail);
        previous(info);
    }));
}

fn write_diagnostic(directory: &Path, kind: &str, detail: &str) -> std::io::Result<PathBuf> {
    fs::create_dir_all(directory)?;
    #[cfg(unix)]
    fs::set_permissions(
        directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )?;
    let path = directory.join(format!(
        "falha-{}-{}.log",
        chrono::Utc::now().format("%Y%m%d-%H%M%S%.3f"),
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&path)?;
    writeln!(file, "tuipe {}", env!("CARGO_PKG_VERSION"))?;
    writeln!(file, "instante UTC: {}", chrono::Utc::now().to_rfc3339())?;
    writeln!(file, "tipo: {kind}")?;
    writeln!(file, "terminal: {}", env::var("TERM").unwrap_or_default())?;
    writeln!(
        file,
        "programa do terminal: {}",
        env::var("TERM_PROGRAM").unwrap_or_default()
    )?;
    writeln!(file, "\n{detail}")?;
    file.sync_all()?;
    Ok(path)
}

fn shutdown_flag() -> Result<Arc<AtomicBool>> {
    let shutdown = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        signal_hook::flag::register(SIGTERM, Arc::clone(&shutdown))?;
        signal_hook::flag::register(SIGHUP, Arc::clone(&shutdown))?;
        signal_hook::flag::register(SIGINT, Arc::clone(&shutdown))?;
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
                "tuipe — treinador de digitação adaptativo e offline\n\nUSO:\n    tuipe\n    tuipe doctor\n    tuipe backup [DESTINO]\n    tuipe rebuild\n    tuipe policy status\n    tuipe policy rollback\n    tuipe --version\n\nCOMANDOS:\n    doctor           valida configuração, banco e eventos sem alterar dados\n    backup [DESTINO] cria uma cópia SQLite consistente e privada\n    rebuild          recalcula métricas e o modelo usando os eventos brutos\n    policy status    mostra a política adaptativa ativa e a alternativa segura\n    policy rollback troca atomicamente a política ativa pela alternativa\n\nDentro do aplicativo, pressione esc para configurações e q para sair."
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
            anyhow::ensure!(
                database_path.exists(),
                "o banco ainda não existe: {}",
                database_path.display()
            );
            let repository = Repository::open(&database_path)
                .with_context(|| format!("abrir banco em {}", database_path.display()))?;
            repository.backup(&destination)?;
            println!("backup criado em {}", destination.display());
        }
        "rebuild" => {
            anyhow::ensure!(arguments.next().is_none(), "rebuild não recebe argumentos");
            let (_, database_path) = paths();
            anyhow::ensure!(
                database_path.exists(),
                "o banco ainda não existe: {}",
                database_path.display()
            );
            let repository = Repository::open(&database_path)
                .with_context(|| format!("abrir banco em {}", database_path.display()))?;
            let report = repository.rebuild_derived_data()?;
            println!(
                "reconstrução concluída\nmétricas: {}\nobservações: {}\npalavras: {}\nn-gramas: {}\nmecânicas: {}",
                report.metrics, report.observations, report.words, report.ngrams, report.mechanics
            );
        }
        "policy" => {
            let action = arguments
                .next()
                .context("policy exige status ou rollback")?;
            anyhow::ensure!(
                arguments.next().is_none(),
                "policy recebeu argumentos demais"
            );
            let (_, database_path) = paths();
            anyhow::ensure!(
                database_path.exists(),
                "o banco ainda não existe: {}",
                database_path.display()
            );
            let repository = Repository::open(&database_path)
                .with_context(|| format!("abrir banco em {}", database_path.display()))?;
            let state = match action.as_str() {
                "status" => repository.adaptive_policy_state()?,
                "rollback" => repository.rollback_adaptive_policy()?,
                _ => anyhow::bail!("ação de policy desconhecida: {action}"),
            };
            println!(
                "política ativa: {}\nalternativa: {}\nshadow: {}",
                policy_name(state.active_version),
                policy_name(state.fallback_version),
                state
                    .shadow_version
                    .map(policy_name)
                    .unwrap_or_else(|| "nenhuma candidata".into())
            );
        }
        _ => anyhow::bail!("comando desconhecido: {command}. Use tuipe --help"),
    }
    Ok(true)
}

fn policy_name(version: u16) -> String {
    match version {
        0 => "uniforme (modo seguro)".into(),
        CURRENT_POLICY_VERSION => format!("adaptativa v{CURRENT_POLICY_VERSION}"),
        _ => format!("desconhecida v{version}"),
    }
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
    settings_focus: usize,
    statistics_open: bool,
    statistics_page: ui::StatisticsPage,
    statistics_selected_word: usize,
    statistics_selected_session: usize,
    statistics_history_filter: ui::HistoryFilter,
    statistics_detail: Option<WordDetail>,
    statistics_session_detail: Option<SessionDetail>,
    statistics_reset: Option<StatisticsReset>,
    statistics: StatisticsOverview,
    generator: Option<WordGenerator<SmallRng>>,
    selections: Vec<Option<tuipe::adaptive::WordSelection>>,
    adaptive: AdaptiveSampler,
    policy_version: u16,
    shadow_policy_version: Option<u16>,
    shadow_generator: Option<WordGenerator<SmallRng>>,
    shadow_stimuli: Vec<String>,
    shadow_selections: Vec<Option<tuipe::adaptive::WordSelection>>,
    seed: u64,
    repeated_test: bool,
    session_kind: SessionKind,
    session_baseline: tuipe::persistence::PersonalBaselineProfile,
    startup_notice: Option<String>,
    focus_lost_at: Option<Instant>,
    current_quote: Option<Quote>,
    quote_favorite: bool,
}

enum StatisticsReset {
    Word { language: String, word: String },
    Model,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MouseOutcome {
    Unchanged,
    Changed,
    Quit,
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
        let session_kind = repository.next_session_kind(
            &preferences.test,
            catalog
                .word_pack(&preferences.test.language, &preferences.test.word_pack)
                .context("o pacote de palavras configurado não está disponível")?,
        )?;
        let policy_state = repository.adaptive_policy_state()?;
        let policy_version = policy_state.active_version;
        let (engine, generator, selections, current_quote) = new_test(
            &catalog,
            &preferences.test,
            seed,
            &adaptive,
            session_kind,
            policy_version,
        )?;
        let shadow = shadow_test(
            &catalog,
            &preferences.test,
            seed,
            &adaptive,
            session_kind,
            policy_state.shadow_version,
        )?;
        let quote_favorite = current_quote
            .as_ref()
            .map(|quote| repository.is_quote_favorite(quote.id))
            .transpose()?
            .unwrap_or(false);
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
            settings_focus: 0,
            statistics_open: false,
            statistics_page: ui::StatisticsPage::Overview,
            statistics_selected_word: 0,
            statistics_selected_session: 0,
            statistics_history_filter: ui::HistoryFilter::All,
            statistics_detail: None,
            statistics_session_detail: None,
            statistics_reset: None,
            statistics: StatisticsOverview::default(),
            generator,
            selections,
            adaptive,
            policy_version,
            shadow_policy_version: policy_state.shadow_version,
            shadow_generator: shadow.generator,
            shadow_stimuli: shadow.stimuli,
            shadow_selections: shadow.selections,
            seed,
            repeated_test: false,
            session_kind,
            session_baseline,
            startup_notice,
            focus_lost_at: None,
            current_quote,
            quote_favorite,
        })
    }

    fn focus_warning_visible(&self) -> bool {
        self.focus_lost_at
            .is_some_and(|lost_at| lost_at.elapsed() >= Duration::from_secs(1))
    }

    fn persist_interrupted(&mut self, repository: &Repository, end: RawSessionEnd) -> Result<()> {
        if self.persisted || self.engine.recorded_events().is_empty() {
            return Ok(());
        }
        let raw_events =
            RawEventCodec::materialize(self.engine.recorded_events(), self.elapsed_ms(), end);
        let observations = self.observations(&self.session_baseline, true);
        repository.save_session_with_provenance(
            self.engine.config(),
            self.engine.status(),
            self.engine.metrics(),
            &observations,
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
        let session_kind = repository.next_session_kind(
            &self.preferences.test,
            self.catalog
                .word_pack(
                    &self.preferences.test.language,
                    &self.preferences.test.word_pack,
                )
                .context("o pacote de palavras configurado não está disponível")?,
        )?;
        let (engine, generator, selections, current_quote) = new_test(
            &self.catalog,
            &self.preferences.test,
            self.seed,
            &self.adaptive,
            session_kind,
            self.policy_version,
        )?;
        let shadow = shadow_test(
            &self.catalog,
            &self.preferences.test,
            self.seed,
            &self.adaptive,
            session_kind,
            self.shadow_policy_version,
        )?;
        self.engine = engine;
        self.generator = generator;
        self.selections = selections;
        self.shadow_generator = shadow.generator;
        self.shadow_stimuli = shadow.stimuli;
        self.shadow_selections = shadow.selections;
        self.quote_favorite = current_quote
            .as_ref()
            .map(|quote| repository.is_quote_favorite(quote.id))
            .transpose()?
            .unwrap_or(false);
        self.current_quote = current_quote;
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
        let (engine, generator, selections, current_quote) = new_test(
            &self.catalog,
            &self.preferences.test,
            self.seed,
            &self.adaptive,
            SessionKind::Repeat,
            self.policy_version,
        )?;
        self.engine = engine;
        self.generator = generator;
        self.selections = selections.into_iter().map(|_| None).collect();
        self.shadow_generator = None;
        self.shadow_stimuli.clear();
        self.shadow_selections.clear();
        self.quote_favorite = current_quote
            .as_ref()
            .map(|quote| repository.is_quote_favorite(quote.id))
            .transpose()?
            .unwrap_or(false);
        self.current_quote = current_quote;
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

    fn toggle_quote_favorite(&mut self, repository: &Repository) -> Result<()> {
        if let Some(quote) = &self.current_quote {
            self.quote_favorite = repository.toggle_quote_favorite(quote.id)?;
        }
        Ok(())
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
            selections: self.selections.clone(),
            policy_version: self.policy_version,
            shadow_stimuli: self.shadow_stimuli.clone(),
            shadow_selections: self.shadow_selections.clone(),
            shadow_policy_version: (!self.shadow_stimuli.is_empty())
                .then_some(self.shadow_policy_version)
                .flatten(),
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
        let observations = self.observations(&self.session_baseline, false);
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
            if let Some(generator) = &mut self.shadow_generator {
                let generated = (0..40)
                    .map(|_| generator.next_generated())
                    .collect::<Vec<_>>();
                self.shadow_stimuli
                    .extend(generated.iter().map(|word| format!("{} ", word.text)));
                self.shadow_selections
                    .extend(generated.into_iter().map(|word| word.selection));
            }
        }
    }

    fn observations(
        &self,
        baseline: &tuipe::persistence::PersonalBaselineProfile,
        interrupted: bool,
    ) -> Vec<WordObservationRecord> {
        tuipe::persistence::derive_word_observations(
            &self.engine,
            baseline,
            self.repeated_test,
            interrupted,
            &self.selections,
        )
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
        let config = self.engine.config();
        let mut statistics = repository.statistics_overview_for(config)?;
        statistics
            .priority_words
            .retain(|word| word.language == config.language);
        statistics
            .priority_patterns
            .retain(|pattern| pattern.language == config.language);
        if let Some(configured_words) = self.catalog.word_pack(&config.language, &config.word_pack)
        {
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
            let chances = if matches!(config.mode, TestMode::Quote) {
                HashMap::new()
            } else if config.adaptive && self.policy_version == CURRENT_POLICY_VERSION {
                let (candidates, _) = session_word_pool(
                    configured_words,
                    config,
                    &self.adaptive,
                    SessionKind::Practice,
                );
                self.adaptive
                    .estimated_session_uplifts_with_number_probability(
                        &config.language,
                        &targets,
                        &candidates,
                        draws,
                        if config.numbers { 0.1 } else { 0.0 },
                    )
            } else {
                HashMap::new()
            };
            for word in &mut statistics.priority_words {
                word.estimated_session_chance = chances.get(&word.word).copied().unwrap_or(0.0);
            }
        }
        self.statistics = statistics;
        self.statistics_selected_word = self
            .statistics_selected_word
            .min(self.statistics.priority_words.len().saturating_sub(1));
        self.statistics_detail = None;
        self.statistics_session_detail = None;
        self.statistics_reset = None;
        self.clamp_statistics_selection();
        Ok(())
    }

    fn reload_adaptive(&mut self, repository: &Repository) -> Result<()> {
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
        for language in ["portuguese", "english"] {
            adaptive.set_baseline(language, repository.baseline_profile(language)?.rates);
        }
        self.adaptive = adaptive;
        Ok(())
    }

    fn open_statistics_word(&mut self, repository: &Repository, index: usize) -> Result<()> {
        let Some(priority) = self.statistics.priority_words.get(index) else {
            return Ok(());
        };
        self.statistics_selected_word = index;
        self.statistics_detail = repository
            .word_detail(&priority.language, &priority.word)?
            .map(|mut detail| {
                detail.priority.estimated_session_chance = priority.estimated_session_chance;
                detail
            });
        Ok(())
    }

    fn filtered_history(&self) -> impl Iterator<Item = &tuipe::persistence::SessionHistoryItem> {
        self.statistics
            .history
            .iter()
            .filter(|session| match self.statistics_history_filter {
                ui::HistoryFilter::All => true,
                ui::HistoryFilter::Completed => session.outcome == SessionOutcome::Completed,
                ui::HistoryFilter::Failed => session.outcome == SessionOutcome::Failed,
            })
    }

    fn clamp_statistics_selection(&mut self) {
        let count = self.filtered_history().count();
        self.statistics_selected_session = self
            .statistics_selected_session
            .min(count.saturating_sub(1));
        self.statistics_selected_word = self
            .statistics_selected_word
            .min(self.statistics.priority_words.len().saturating_sub(1));
    }

    fn open_statistics_session(&mut self, repository: &Repository) -> Result<()> {
        let id = self
            .filtered_history()
            .nth(self.statistics_selected_session)
            .map(|session| session.id);
        self.statistics_session_detail = id
            .map(|id| repository.session_detail(id))
            .transpose()?
            .flatten();
        Ok(())
    }
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
    let mut last_focus_warning = app.focus_warning_visible();
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
                        settings_focus: app.settings_focus,
                        theme_name: &app.preferences.theme,
                        session_kind: app.session_kind,
                        persistence: app.persistence_ui_state(),
                        notice: app.startup_notice.as_deref(),
                        focus_warning: app.focus_warning_visible(),
                        quote: app
                            .current_quote
                            .as_ref()
                            .map(|quote| ui::QuoteRenderState {
                                source: &quote.source,
                                favorite: app.quote_favorite,
                            }),
                        keymap: &app.preferences.keymap,
                    },
                );
                if app.statistics_open {
                    ui::render_statistics(
                        frame,
                        &app.statistics,
                        ui::StatisticsRenderState {
                            page: app.statistics_page,
                            selected_word: app.statistics_selected_word,
                            selected_session: app.statistics_selected_session,
                            history_filter: app.statistics_history_filter,
                            word_detail: app.statistics_detail.as_ref(),
                            session_detail: app.statistics_session_detail.as_ref(),
                        },
                        theme,
                    );
                    if let Some(reset) = &app.statistics_reset {
                        let confirmation = match reset {
                            StatisticsReset::Word { word, .. } => ui::ResetConfirmation::Word(word),
                            StatisticsReset::Model => ui::ResetConfirmation::Model,
                        };
                        ui::render_reset_confirmation(frame, confirmation, theme);
                    }
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
                Event::Key(key) if key.kind == KeyEventKind::Repeat => {
                    handle_typing_repeat(app, key.code, key.modifiers)
                }
                Event::Mouse(mouse) => {
                    match handle_mouse(app, repository, mouse, terminal.size()?)? {
                        MouseOutcome::Unchanged => false,
                        MouseOutcome::Changed => true,
                        MouseOutcome::Quit => break,
                    }
                }
                Event::Resize(width, height)
                    if width != last_size.width || height != last_size.height =>
                {
                    last_size = ratatui::layout::Size::new(width, height);
                    true
                }
                Event::Resize(_, _) => false,
                Event::FocusGained => {
                    app.focus_lost_at = None;
                    app.update(InputEvent::External {
                        event: ExternalEvent::Focus { gained: true },
                        at_ms: app.elapsed_ms(),
                    });
                    true
                }
                Event::FocusLost => {
                    app.focus_lost_at = Some(Instant::now());
                    app.update(InputEvent::External {
                        event: ExternalEvent::Focus { gained: false },
                        at_ms: app.elapsed_ms(),
                    });
                    true
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
        let focus_warning = app.focus_warning_visible();
        needs_draw |= focus_warning != last_focus_warning;
        last_focus_warning = focus_warning;
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
) -> Result<MouseOutcome> {
    if app.startup_notice.is_some() {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return Ok(MouseOutcome::Unchanged);
        }
        app.startup_notice = None;
        return Ok(MouseOutcome::Changed);
    }

    let viewport = ratatui::layout::Rect::new(0, 0, terminal.width, terminal.height);
    if app.statistics_open {
        if matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) && app.statistics_reset.is_none()
            && app.statistics_detail.is_none()
            && app.statistics_session_detail.is_none()
        {
            let down = mouse.kind == MouseEventKind::ScrollDown;
            match app.statistics_page {
                ui::StatisticsPage::Overview => {
                    app.statistics_selected_word = if down {
                        (app.statistics_selected_word + 1)
                            .min(app.statistics.priority_words.len().saturating_sub(1))
                    } else {
                        app.statistics_selected_word.saturating_sub(1)
                    };
                }
                ui::StatisticsPage::History => {
                    let count = app.filtered_history().count();
                    app.statistics_selected_session = if down {
                        (app.statistics_selected_session + 1).min(count.saturating_sub(1))
                    } else {
                        app.statistics_selected_session.saturating_sub(1)
                    };
                }
                ui::StatisticsPage::Progress => return Ok(MouseOutcome::Unchanged),
            }
            return Ok(MouseOutcome::Changed);
        }
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return Ok(MouseOutcome::Unchanged);
        }
        let position = ratatui::layout::Position::new(mouse.column, mouse.row);
        if app.statistics_reset.is_some() {
            match ui::reset_confirmation_action_at(viewport, position) {
                Some(ui::StatisticsAction::ConfirmReset) => {
                    let reset = app.statistics_reset.take().expect("confirmação aberta");
                    match reset {
                        StatisticsReset::Word { language, word } => {
                            repository.reset_word_model(&language, &word)?;
                        }
                        StatisticsReset::Model => repository.reset_adaptive_model()?,
                    }
                    app.reload_adaptive(repository)?;
                    app.load_statistics(repository)?;
                }
                Some(ui::StatisticsAction::CancelReset) => app.statistics_reset = None,
                _ => return Ok(MouseOutcome::Unchanged),
            }
            return Ok(MouseOutcome::Changed);
        }
        if let Some(action) = ui::statistics_detail_action_at(
            viewport,
            app.statistics_detail.is_some(),
            app.statistics_session_detail.is_some(),
            position,
        ) {
            match action {
                ui::StatisticsAction::ResetWord => {
                    let detail = app.statistics_detail.as_ref().expect("detalhe disponível");
                    app.statistics_reset = Some(StatisticsReset::Word {
                        language: detail.priority.language.clone(),
                        word: detail.priority.word.clone(),
                    });
                }
                ui::StatisticsAction::Back => {
                    app.statistics_detail = None;
                    app.statistics_session_detail = None;
                }
                _ => unreachable!("ação incompatível com detalhe"),
            }
            return Ok(MouseOutcome::Changed);
        }
        if app.statistics_detail.is_none()
            && app.statistics_session_detail.is_none()
            && let Some(action) = ui::statistics_action_at(
                viewport,
                &app.statistics,
                app.statistics_page,
                app.statistics_selected_session,
                app.statistics_history_filter,
                position,
            )
        {
            match action {
                ui::StatisticsAction::Page(page) => app.statistics_page = page,
                ui::StatisticsAction::Session(index) => {
                    app.statistics_selected_session = index;
                    app.open_statistics_session(repository)?;
                }
                ui::StatisticsAction::FilterHistory => {
                    app.statistics_history_filter = match app.statistics_history_filter {
                        ui::HistoryFilter::All => ui::HistoryFilter::Completed,
                        ui::HistoryFilter::Completed => ui::HistoryFilter::Failed,
                        ui::HistoryFilter::Failed => ui::HistoryFilter::All,
                    };
                    app.statistics_selected_session = 0;
                }
                ui::StatisticsAction::ResetModel => {
                    app.statistics_reset = Some(StatisticsReset::Model)
                }
                ui::StatisticsAction::Back => app.statistics_open = false,
                ui::StatisticsAction::ResetWord
                | ui::StatisticsAction::ConfirmReset
                | ui::StatisticsAction::CancelReset => {
                    unreachable!("ação incompatível com a visão de estatísticas")
                }
            }
            return Ok(MouseOutcome::Changed);
        }
        if app.statistics_page == ui::StatisticsPage::Overview
            && app.statistics_detail.is_none()
            && app.statistics_session_detail.is_none()
            && let Some(index) = ui::statistics_word_at(
                viewport,
                &app.statistics,
                app.statistics_selected_word,
                position,
            )
        {
            app.open_statistics_word(repository, index)?;
            return Ok(MouseOutcome::Changed);
        }
        return Ok(MouseOutcome::Unchanged);
    }
    if app.settings_open {
        let position = ratatui::layout::Position::new(mouse.column, mouse.row);
        if !ui::settings_area(viewport).contains(position) {
            app.settings_open = false;
            return Ok(MouseOutcome::Changed);
        }
        let Some(action) = ui::settings_action_at(
            viewport,
            app.engine.config(),
            &app.preferences.theme,
            &app.preferences.keymap,
            app.settings_focus,
            position,
        ) else {
            return Ok(MouseOutcome::Unchanged);
        };
        return Ok(if handle_settings_mouse_action(app, repository, action)? {
            MouseOutcome::Quit
        } else {
            MouseOutcome::Changed
        });
    }
    if matches!(
        app.engine.status(),
        TestStatus::Completed { .. } | TestStatus::Failed { .. }
    ) {
        let position = ratatui::layout::Position::new(mouse.column, mouse.row);
        let Some(action) = ui::result_action_at(
            viewport,
            &app.preferences.keymap,
            app.current_quote.is_some(),
            position,
        ) else {
            return Ok(MouseOutcome::Unchanged);
        };
        if app.persistence_pending || app.persistence_error.is_some() {
            return Ok(MouseOutcome::Unchanged);
        }
        if action != ui::ResultAction::Next && app.bloqueia_atalhos_do_resultado() {
            return Ok(MouseOutcome::Unchanged);
        }
        match action {
            ui::ResultAction::Next => app.restart(repository)?,
            ui::ResultAction::Repeat => app.repeat(repository)?,
            ui::ResultAction::Statistics => {
                app.load_statistics(repository)?;
                app.statistics_open = true;
            }
            ui::ResultAction::Favorite => app.toggle_quote_favorite(repository)?,
            ui::ResultAction::Quit => return Ok(MouseOutcome::Quit),
        }
        return Ok(MouseOutcome::Changed);
    }
    if !matches!(app.engine.status(), TestStatus::Ready) {
        return Ok(MouseOutcome::Unchanged);
    }

    let config_bar = ui::config_bar_area(viewport);
    if !config_bar.contains(ratatui::layout::Position::new(mouse.column, mouse.row)) {
        return Ok(MouseOutcome::Unchanged);
    }

    let Some(cards) = ui::config_card_areas(viewport, &app.engine.config().mode) else {
        if !ui::config_compact_card_area(viewport)
            .contains(ratatui::layout::Position::new(mouse.column, mouse.row))
        {
            return Ok(MouseOutcome::Unchanged);
        }
        app.settings_open = true;
        app.settings_focus = initial_settings_focus(&app.engine.config().mode);
        return Ok(MouseOutcome::Changed);
    };
    let x = mouse.column;
    if (cards[0].x..cards[0].right()).contains(&x) {
        if matches!(app.engine.config().mode, TestMode::Quote) {
            return Ok(MouseOutcome::Unchanged);
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
        return Ok(MouseOutcome::Unchanged);
    }
    Ok(MouseOutcome::Changed)
}

fn handle_settings_mouse_action(
    app: &mut App,
    repository: &Repository,
    action: ui::SettingsAction,
) -> Result<bool> {
    use ui::SettingsAction;

    app.settings_focus = match action {
        SettingsAction::Focus(index) => index.min(8),
        SettingsAction::Punctuation(_) => 0,
        SettingsAction::Numbers(_) => 1,
        SettingsAction::ModeTime | SettingsAction::ModeWords | SettingsAction::ModeQuote => 2,
        SettingsAction::Value(_) => 3,
        SettingsAction::Difficulty(_) => 4,
        SettingsAction::Adaptive(_) => 5,
        SettingsAction::LanguagePortuguese | SettingsAction::LanguageEnglish => 6,
        SettingsAction::PackCommon | SettingsAction::Pack1k | SettingsAction::Pack5k => 7,
        SettingsAction::NextTheme => 8,
        SettingsAction::Close | SettingsAction::Quit => app.settings_focus,
    };

    match action {
        SettingsAction::Focus(_) => {}
        SettingsAction::Close => app.settings_open = false,
        SettingsAction::Quit => return Ok(true),
        SettingsAction::NextTheme => {
            return handle_settings_key(app, repository, KeyCode::Char('t'), KeyModifiers::NONE);
        }
        SettingsAction::Punctuation(enabled) => {
            app.apply_preference(repository, |preferences| {
                preferences.test.punctuation = enabled;
            })?
        }
        SettingsAction::Numbers(enabled) => app.apply_preference(repository, |preferences| {
            preferences.test.numbers = enabled;
        })?,
        SettingsAction::ModeTime => app.apply_preference(repository, |preferences| {
            let seconds = match preferences.test.mode {
                TestMode::Time { seconds } => seconds,
                _ => 30,
            };
            preferences.test.mode = TestMode::Time { seconds };
        })?,
        SettingsAction::ModeWords => app.apply_preference(repository, |preferences| {
            let count = match preferences.test.mode {
                TestMode::Words { count } => count,
                _ => 25,
            };
            preferences.test.mode = TestMode::Words { count };
        })?,
        SettingsAction::ModeQuote => app.apply_preference(repository, |preferences| {
            preferences.test.mode = TestMode::Quote;
            preferences.test.punctuation = false;
            preferences.test.numbers = false;
        })?,
        SettingsAction::Value(index) => app.apply_preference(repository, |preferences| {
            preferences.test.mode = match preferences.test.mode {
                TestMode::Time { .. } => TestMode::Time {
                    seconds: [15, 30, 60, 120][index.min(3)],
                },
                TestMode::Words { .. } => TestMode::Words {
                    count: [10, 25, 50, 100][index.min(3)],
                },
                TestMode::Quote => {
                    preferences.test.quote_length = [
                        QuoteLength::All,
                        QuoteLength::Short,
                        QuoteLength::Medium,
                        QuoteLength::Long,
                    ][index.min(3)];
                    TestMode::Quote
                }
            };
        })?,
        SettingsAction::Difficulty(difficulty) => {
            app.apply_preference(repository, |preferences| {
                preferences.test.difficulty = difficulty;
            })?
        }
        SettingsAction::Adaptive(enabled) => app.apply_preference(repository, |preferences| {
            preferences.test.adaptive = enabled;
        })?,
        SettingsAction::LanguagePortuguese | SettingsAction::LanguageEnglish => {
            let language = if action == SettingsAction::LanguagePortuguese {
                "portuguese"
            } else {
                "english"
            };
            app.apply_preference(repository, |preferences| {
                preferences.test.language = language.into();
            })?;
        }
        SettingsAction::PackCommon | SettingsAction::Pack1k | SettingsAction::Pack5k => {
            let pack = match action {
                SettingsAction::PackCommon => "common",
                SettingsAction::Pack1k => "1k",
                _ => "5k",
            };
            app.apply_preference(repository, |preferences| {
                preferences.test.word_pack = pack.into();
            })?;
        }
    }
    Ok(false)
}

fn handle_key(
    app: &mut App,
    repository: &Repository,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<bool> {
    let pressed: crokey::KeyCombination = KeyEvent::new(code, modifiers).into();
    if app.startup_notice.is_some() {
        if matches!(code, KeyCode::Enter | KeyCode::Esc) {
            app.startup_notice = None;
        }
        return Ok(false);
    }
    if app.statistics_open {
        if let Some(reset) = app.statistics_reset.take() {
            match code {
                KeyCode::Char('y') | KeyCode::Char('s') => {
                    match reset {
                        StatisticsReset::Word { language, word } => {
                            repository.reset_word_model(&language, &word)?;
                        }
                        StatisticsReset::Model => repository.reset_adaptive_model()?,
                    }
                    app.reload_adaptive(repository)?;
                    app.load_statistics(repository)?;
                }
                KeyCode::Esc | KeyCode::Char('n') => {}
                _ => app.statistics_reset = Some(reset),
            }
        } else if app.statistics_detail.is_some() {
            match code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Backspace => {
                    app.statistics_detail = None;
                }
                KeyCode::Char('r') => {
                    let detail = app.statistics_detail.as_ref().expect("detalhe disponível");
                    app.statistics_reset = Some(StatisticsReset::Word {
                        language: detail.priority.language.clone(),
                        word: detail.priority.word.clone(),
                    });
                }
                _ => {}
            }
        } else if app.statistics_session_detail.is_some() {
            if matches!(code, KeyCode::Esc | KeyCode::Enter | KeyCode::Backspace) {
                app.statistics_session_detail = None;
            }
        } else {
            match code {
                KeyCode::Char('1') => app.statistics_page = ui::StatisticsPage::Overview,
                KeyCode::Char('2') => app.statistics_page = ui::StatisticsPage::Progress,
                KeyCode::Char('3') => app.statistics_page = ui::StatisticsPage::History,
                KeyCode::Tab | KeyCode::Right => {
                    app.statistics_page = match app.statistics_page {
                        ui::StatisticsPage::Overview => ui::StatisticsPage::Progress,
                        ui::StatisticsPage::Progress => ui::StatisticsPage::History,
                        ui::StatisticsPage::History => ui::StatisticsPage::Overview,
                    };
                }
                KeyCode::BackTab | KeyCode::Left => {
                    app.statistics_page = match app.statistics_page {
                        ui::StatisticsPage::Overview => ui::StatisticsPage::History,
                        ui::StatisticsPage::Progress => ui::StatisticsPage::Overview,
                        ui::StatisticsPage::History => ui::StatisticsPage::Progress,
                    };
                }
                _ => {}
            }
            match code {
                KeyCode::Esc | KeyCode::Backspace => app.statistics_open = false,
                KeyCode::Char('R') if app.statistics_page == ui::StatisticsPage::Overview => {
                    app.statistics_reset = Some(StatisticsReset::Model)
                }
                KeyCode::Up | KeyCode::Char('k')
                    if app.statistics_page == ui::StatisticsPage::Overview =>
                {
                    app.statistics_selected_word = app.statistics_selected_word.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j')
                    if app.statistics_page == ui::StatisticsPage::Overview =>
                {
                    app.statistics_selected_word = (app.statistics_selected_word + 1)
                        .min(app.statistics.priority_words.len().saturating_sub(1));
                }
                KeyCode::Enter
                    if app.statistics_page == ui::StatisticsPage::Overview
                        && !app.statistics.priority_words.is_empty() =>
                {
                    app.open_statistics_word(repository, app.statistics_selected_word)?;
                }
                KeyCode::Up | KeyCode::Char('k')
                    if app.statistics_page == ui::StatisticsPage::History =>
                {
                    app.statistics_selected_session =
                        app.statistics_selected_session.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j')
                    if app.statistics_page == ui::StatisticsPage::History =>
                {
                    let count = app.filtered_history().count();
                    app.statistics_selected_session =
                        (app.statistics_selected_session + 1).min(count.saturating_sub(1));
                }
                KeyCode::Char('f') if app.statistics_page == ui::StatisticsPage::History => {
                    app.statistics_history_filter = match app.statistics_history_filter {
                        ui::HistoryFilter::All => ui::HistoryFilter::Completed,
                        ui::HistoryFilter::Completed => ui::HistoryFilter::Failed,
                        ui::HistoryFilter::Failed => ui::HistoryFilter::All,
                    };
                    app.statistics_selected_session = 0;
                }
                KeyCode::Enter if app.statistics_page == ui::StatisticsPage::History => {
                    app.open_statistics_session(repository)?;
                }
                _ => {}
            }
        }
        return Ok(false);
    }
    let terminal = matches!(
        app.engine.status(),
        TestStatus::Completed { .. } | TestStatus::Failed { .. }
    );
    if pressed == app.preferences.keymap.statistics_global
        && matches!(app.engine.status(), TestStatus::Ready)
    {
        app.load_statistics(repository)?;
        app.statistics_open = true;
        return Ok(false);
    }
    if terminal && app.persistence_pending {
        return Ok(false);
    }
    if terminal && app.persistence_error.is_some() {
        if pressed == app.preferences.keymap.repeat {
            app.persistence_error = None;
        }
        return Ok(false);
    }
    if app.settings_open {
        return handle_settings_key(app, repository, code, modifiers);
    }
    if pressed == app.preferences.keymap.settings
        && !matches!(app.engine.status(), TestStatus::Running { .. })
    {
        app.settings_open = true;
        app.settings_focus = initial_settings_focus(&app.engine.config().mode);
        return Ok(false);
    }
    if pressed == app.preferences.keymap.cancel {
        if matches!(app.engine.status(), TestStatus::Running { .. }) {
            app.restart(repository)?;
            return Ok(false);
        }
        return Ok(true);
    }
    let resultado_recente = app.bloqueia_atalhos_do_resultado();
    if pressed == app.preferences.keymap.favorite && terminal && !resultado_recente {
        app.toggle_quote_favorite(repository)?;
        return Ok(false);
    }
    if pressed == app.preferences.keymap.quit
        && matches!(
            app.engine.status(),
            TestStatus::Completed { .. } | TestStatus::Failed { .. }
        )
        && !resultado_recente
    {
        return Ok(true);
    }
    if pressed == app.preferences.keymap.next {
        app.restart(repository)?;
        return Ok(false);
    }
    if pressed == app.preferences.keymap.repeat
        && matches!(
            app.engine.status(),
            TestStatus::Completed { .. } | TestStatus::Failed { .. }
        )
        && !resultado_recente
    {
        app.repeat(repository)?;
        return Ok(false);
    }
    if pressed == app.preferences.keymap.statistics
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

    let action = typing_action(
        code,
        modifiers,
        app.preferences.keymap.delete_word.contains(&pressed),
    );
    if let Some(action) = action {
        app.update(InputEvent::Key {
            action,
            at_ms: app.elapsed_ms(),
        });
    }
    Ok(false)
}

fn typing_action(code: KeyCode, modifiers: KeyModifiers, delete_word: bool) -> Option<KeyAction> {
    if delete_word {
        return Some(KeyAction::DeleteWordBackward);
    }
    let command_modifiers =
        modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
    let is_alt_gr = command_modifiers == KeyModifiers::CONTROL | KeyModifiers::ALT;
    match code {
        KeyCode::Char(character) if command_modifiers.is_empty() || is_alt_gr => {
            Some(KeyAction::Text(character.to_string()))
        }
        KeyCode::Backspace => Some(KeyAction::Backspace),
        _ => None,
    }
}

fn handle_typing_repeat(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if app.settings_open
        || app.statistics_open
        || matches!(
            app.engine.status(),
            TestStatus::Completed { .. } | TestStatus::Failed { .. }
        )
    {
        return false;
    }
    let pressed: crokey::KeyCombination = KeyEvent::new(code, modifiers).into();
    let Some(action) = typing_action(
        code,
        modifiers,
        app.preferences.keymap.delete_word.contains(&pressed),
    ) else {
        return false;
    };
    app.update(InputEvent::Key {
        action,
        at_ms: app.elapsed_ms(),
    });
    true
}

fn handle_settings_key(
    app: &mut App,
    repository: &Repository,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<bool> {
    let pressed: crokey::KeyCombination = KeyEvent::new(code, modifiers).into();
    if pressed == app.preferences.keymap.quit {
        return Ok(true);
    }
    if pressed == app.preferences.keymap.settings {
        app.settings_open = false;
        return Ok(false);
    }
    match code {
        KeyCode::Tab | KeyCode::Down => {
            app.settings_focus = (app.settings_focus + 1) % 9;
            if matches!(app.engine.config().mode, TestMode::Quote) && app.settings_focus < 2 {
                app.settings_focus = 2;
            }
            return Ok(false);
        }
        KeyCode::BackTab | KeyCode::Up => {
            app.settings_focus = app.settings_focus.checked_sub(1).unwrap_or(8);
            if matches!(app.engine.config().mode, TestMode::Quote) && app.settings_focus < 2 {
                app.settings_focus = 8;
            }
            return Ok(false);
        }
        _ => {}
    }
    if code == KeyCode::Enter {
        app.settings_open = false;
        return Ok(false);
    }
    if matches!(code, KeyCode::Left | KeyCode::Right) {
        adjust_focused_setting(app, repository, code)?;
        return Ok(false);
    }
    match code {
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

fn initial_settings_focus(mode: &TestMode) -> usize {
    if matches!(mode, TestMode::Quote) {
        2
    } else {
        0
    }
}

fn next<T: Copy + PartialEq>(values: &[T], current: T) -> T {
    cycle(values, current, false)
}

fn cycle<T: Copy + PartialEq>(values: &[T], current: T, backwards: bool) -> T {
    let index = values
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    let next = if backwards {
        index.checked_sub(1).unwrap_or(values.len() - 1)
    } else {
        (index + 1) % values.len()
    };
    values[next]
}

fn adjust_focused_setting(app: &mut App, repository: &Repository, code: KeyCode) -> Result<()> {
    let backwards = code == KeyCode::Left;
    let forwards = code == KeyCode::Right;
    match app.settings_focus {
        0 if !matches!(app.engine.config().mode, TestMode::Quote) => {
            app.apply_preference(repository, |preferences| {
                preferences.test.punctuation = forwards;
            })?;
        }
        1 if !matches!(app.engine.config().mode, TestMode::Quote) => {
            app.apply_preference(repository, |preferences| {
                preferences.test.numbers = forwards;
            })?;
        }
        2 => app.apply_preference(repository, |preferences| {
            let modes = [0_u8, 1, 2];
            let current = match preferences.test.mode {
                TestMode::Time { .. } => 0,
                TestMode::Words { .. } => 1,
                TestMode::Quote => 2,
            };
            preferences.test.mode = match cycle(&modes, current, backwards) {
                0 => TestMode::Time { seconds: 30 },
                1 => TestMode::Words { count: 25 },
                _ => {
                    preferences.test.punctuation = false;
                    preferences.test.numbers = false;
                    TestMode::Quote
                }
            };
        })?,
        3 => app.apply_preference(repository, |preferences| {
            preferences.test.mode = match preferences.test.mode {
                TestMode::Time { seconds } => TestMode::Time {
                    seconds: cycle(&[15, 30, 60, 120], seconds, backwards),
                },
                TestMode::Words { count } => TestMode::Words {
                    count: cycle(&[10, 25, 50, 100], count, backwards),
                },
                TestMode::Quote => {
                    preferences.test.quote_length = cycle(
                        &[
                            QuoteLength::All,
                            QuoteLength::Short,
                            QuoteLength::Medium,
                            QuoteLength::Long,
                        ],
                        preferences.test.quote_length,
                        backwards,
                    );
                    TestMode::Quote
                }
            };
        })?,
        4 => app.apply_preference(repository, |preferences| {
            preferences.test.difficulty = cycle(
                &[
                    tuipe::typing::Difficulty::Normal,
                    tuipe::typing::Difficulty::Expert,
                    tuipe::typing::Difficulty::Master,
                ],
                preferences.test.difficulty,
                backwards,
            );
        })?,
        5 => app.apply_preference(repository, |preferences| {
            preferences.test.adaptive = forwards;
        })?,
        6 => app.apply_preference(repository, |preferences| {
            preferences.test.language = cycle(
                &["portuguese", "english"],
                preferences.test.language.as_str(),
                backwards,
            )
            .into();
        })?,
        7 => app.apply_preference(repository, |preferences| {
            preferences.test.word_pack = cycle(
                &["common", "1k", "5k"],
                preferences.test.word_pack.as_str(),
                backwards,
            )
            .into();
        })?,
        8 => {
            let names = app
                .catalog
                .theme_names()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let current = app.preferences.theme.clone();
            app.apply_preference(repository, |preferences| {
                let index = names.iter().position(|name| name == &current).unwrap_or(0);
                let next = if backwards {
                    index.checked_sub(1).unwrap_or(names.len() - 1)
                } else {
                    (index + 1) % names.len()
                };
                preferences.theme = names[next].clone();
            })?;
        }
        _ => {}
    }
    Ok(())
}

type GeneratedTest = (
    TestEngine,
    Option<WordGenerator<SmallRng>>,
    Vec<Option<tuipe::adaptive::WordSelection>>,
    Option<Quote>,
);

struct ShadowTest {
    generator: Option<WordGenerator<SmallRng>>,
    stimuli: Vec<String>,
    selections: Vec<Option<tuipe::adaptive::WordSelection>>,
}

fn new_test(
    catalog: &ContentCatalog,
    config: &tuipe::typing::TestConfig,
    seed: u64,
    adaptive: &AdaptiveSampler,
    session_kind: SessionKind,
    policy_version: u16,
) -> Result<GeneratedTest> {
    let mut rng = SmallRng::seed_from_u64(seed);
    let (words, generator, selections, quote) = match config.mode {
        TestMode::Quote => {
            let quotes = catalog.quotes(&config.language, config.quote_length);
            let quote = (*quotes
                .choose(&mut rng)
                .context("o idioma não possui citações")?)
            .clone();
            let words = quote
                .text
                .split_whitespace()
                .map(|word| format!("{word} "))
                .collect::<Vec<_>>();
            let selections = vec![None; words.len()];
            (without_last_commit(words), None, selections, Some(quote))
        }
        TestMode::Words { count } => {
            let mut generator =
                word_generator(catalog, config, rng, adaptive, session_kind, policy_version)?;
            let (words, selections) = generate(&mut generator, usize::from(count));
            (without_last_commit(words), None, selections, None)
        }
        TestMode::Time { .. } => {
            let mut generator =
                word_generator(catalog, config, rng, adaptive, session_kind, policy_version)?;
            // O buffer inicial precisa preencher três linhas reais também em
            // terminais ultrawide, como o gerador contínuo do Monkeytype.
            let (words, selections) = generate(&mut generator, 120);
            (words, Some(generator), selections, None)
        }
    };
    Ok((
        TestEngine::new(config.clone(), words),
        generator,
        selections,
        quote,
    ))
}

fn shadow_test(
    catalog: &ContentCatalog,
    config: &tuipe::typing::TestConfig,
    seed: u64,
    adaptive: &AdaptiveSampler,
    session_kind: SessionKind,
    shadow_version: Option<u16>,
) -> Result<ShadowTest> {
    let Some(shadow_version) = shadow_version else {
        return Ok(ShadowTest {
            generator: None,
            stimuli: Vec::new(),
            selections: Vec::new(),
        });
    };
    if !config.adaptive
        || session_kind != SessionKind::Practice
        || matches!(config.mode, TestMode::Quote)
    {
        return Ok(ShadowTest {
            generator: None,
            stimuli: Vec::new(),
            selections: Vec::new(),
        });
    }
    let (engine, generator, selections, _) = new_test(
        catalog,
        config,
        seed,
        adaptive,
        session_kind,
        shadow_version,
    )?;
    let stimuli = engine
        .targets()
        .iter()
        .map(|target| target.text.clone())
        .collect();
    Ok(ShadowTest {
        generator,
        stimuli,
        selections,
    })
}

fn word_generator(
    catalog: &ContentCatalog,
    config: &tuipe::typing::TestConfig,
    rng: SmallRng,
    adaptive: &AdaptiveSampler,
    session_kind: SessionKind,
    policy_version: u16,
) -> Result<WordGenerator<SmallRng>> {
    let configured_words = catalog
        .word_pack(&config.language, &config.word_pack)
        .context("o pacote de palavras configurado não está disponível")?;
    let (words, retention_words) =
        session_word_pool(configured_words, config, adaptive, session_kind);
    let generator = WordGenerator::new(&words, rng, config.punctuation, config.numbers);
    Ok(match session_kind {
        SessionKind::Assessment => generator.with_assessment(),
        SessionKind::Practice if config.adaptive && policy_version == CURRENT_POLICY_VERSION => {
            generator.with_adaptive(&config.language, adaptive.clone())
        }
        SessionKind::Retention => generator.with_forced_words(retention_words),
        SessionKind::Practice | SessionKind::Transfer | SessionKind::Repeat => generator,
    })
}

fn session_word_pool(
    configured_words: &[String],
    config: &tuipe::typing::TestConfig,
    adaptive: &AdaptiveSampler,
    session_kind: SessionKind,
) -> (Vec<String>, Vec<String>) {
    let retention_words = if session_kind == SessionKind::Retention {
        adaptive.retention_candidates(&config.language, configured_words)
    } else {
        Vec::new()
    };
    let partitioned = match session_kind {
        SessionKind::Transfer => configured_words
            .iter()
            .filter(|word| is_transfer_holdout(word))
            .cloned()
            .collect::<Vec<_>>(),
        SessionKind::Retention => {
            let mut pool = retention_words.clone();
            for word in configured_words {
                if pool.len() >= 3 {
                    break;
                }
                if !pool.contains(word) {
                    pool.push(word.clone());
                }
            }
            pool
        }
        SessionKind::Practice if config.adaptive => configured_words
            .iter()
            .filter(|word| !is_transfer_holdout(word))
            .cloned()
            .collect(),
        SessionKind::Assessment | SessionKind::Practice | SessionKind::Repeat => Vec::new(),
    };
    let words = if partitioned.is_empty() {
        configured_words.to_vec()
    } else {
        partitioned
    };
    (words, retention_words)
}

#[cfg(test)]
fn estimated_generator_chances(
    catalog: &ContentCatalog,
    config: &tuipe::typing::TestConfig,
    adaptive: &AdaptiveSampler,
    session_kind: SessionKind,
    targets: &[String],
    draws: usize,
    policy_version: u16,
) -> Result<HashMap<String, f64>> {
    if targets.is_empty() || draws == 0 {
        return Ok(HashMap::new());
    }
    const TRIALS: usize = 128;
    let targets = targets.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut counts = HashMap::<String, usize>::new();
    for trial in 0..TRIALS {
        let seed = 0x7475_6970_652d_7374_u64.wrapping_add(trial as u64);
        let mut generator = word_generator(
            catalog,
            config,
            SmallRng::seed_from_u64(seed),
            adaptive,
            session_kind,
            policy_version,
        )?;
        let mut seen = HashSet::<String>::new();
        for _ in 0..draws {
            let generated = generator.next_generated();
            if let Some(selection) = generated.selection
                && targets.contains(selection.word.as_str())
            {
                seen.insert(selection.word);
            }
        }
        for word in seen {
            *counts.entry(word).or_default() += 1;
        }
    }
    Ok(targets
        .into_iter()
        .map(|word| {
            (
                word.to_owned(),
                counts.get(word).copied().unwrap_or(0) as f64 / TRIALS as f64,
            )
        })
        .collect())
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

    #[test]
    fn diagnostico_de_falha_e_privado_e_acionavel() {
        let temporary = tempfile::tempdir().unwrap();
        let path = write_diagnostic(temporary.path(), "teste", "causa encadeada").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("tipo: teste"));
        assert!(content.contains("causa encadeada"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

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
    fn modo_seguro_desativa_o_sorteio_adaptativo_sem_mudar_a_preferencia() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        repository.rollback_adaptive_policy().unwrap();
        let preferences = Preferences::default();
        assert!(preferences.test.adaptive);

        let app = App::new(
            preferences,
            ContentCatalog::bundled().unwrap(),
            temporary.path().join("config.toml"),
            None,
            &repository,
        )
        .unwrap();
        assert_eq!(app.policy_version, 0);
        assert!(app.selections.iter().flatten().all(|selection| {
            selection.source == tuipe::adaptive::SelectionSource::Representative
        }));
        assert_eq!(app.provenance().policy_version, 0);
        assert_eq!(app.shadow_policy_version, Some(CURRENT_POLICY_VERSION));
        assert_eq!(app.shadow_stimuli.len(), app.engine.targets().len());
        assert_eq!(app.shadow_selections.len(), app.selections.len());
        assert_eq!(
            app.provenance().shadow_policy_version,
            Some(CURRENT_POLICY_VERSION)
        );
    }

    #[test]
    fn shadow_acompanha_extensao_continua_e_e_persistido() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        repository.rollback_adaptive_policy().unwrap();
        let mut app = App::new(
            Preferences::default(),
            ContentCatalog::bundled().unwrap(),
            temporary.path().join("config.toml"),
            None,
            &repository,
        )
        .unwrap();
        let initial_len = app.engine.targets().len();

        for _ in 0..initial_len {
            if app.engine.targets().len() > initial_len {
                break;
            }
            let text = app.engine.targets()[app.engine.active_word()].with_commit();
            app.update(InputEvent::Key {
                action: KeyAction::Text(text),
                at_ms: app.engine.recorded_events().len() as u64 + 1,
            });
        }

        let provenance = app.provenance();
        assert_eq!(app.engine.targets().len(), initial_len + 40);
        assert_eq!(provenance.stimuli.len(), provenance.selections.len());
        assert_eq!(provenance.shadow_stimuli.len(), initial_len + 40);
        assert_eq!(
            provenance.shadow_stimuli.len(),
            provenance.shadow_selections.len()
        );
        let id = repository
            .save_session_with_provenance(
                app.engine.config(),
                app.engine.status(),
                app.engine.metrics(),
                &[],
                &[],
                &provenance,
            )
            .unwrap();
        assert_eq!(repository.session_provenance(id).unwrap(), Some(provenance));
        Repository::doctor(&temporary.path().join("history.db")).unwrap();
    }

    #[test]
    fn ctrl_w_and_ctrl_backspace_remove_the_active_word() {
        assert_eq!(
            typing_action(KeyCode::Char('w'), KeyModifiers::CONTROL, true),
            Some(KeyAction::DeleteWordBackward)
        );
        assert_eq!(
            typing_action(KeyCode::Backspace, KeyModifiers::CONTROL, true),
            Some(KeyAction::DeleteWordBackward)
        );
        assert_eq!(
            typing_action(KeyCode::Char('h'), KeyModifiers::CONTROL, false),
            None
        );
    }

    #[test]
    fn alt_gr_entregue_como_ctrl_alt_continua_digitavel() {
        assert_eq!(
            typing_action(
                KeyCode::Char('€'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
                false
            ),
            Some(KeyAction::Text("€".into()))
        );
    }

    #[test]
    fn ctrl_c_cancela_a_execucao_sem_marca_la_como_repeticao() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let mut app = app_de_teste(tuipe::typing::TestConfig::default(), &["casa "]);
        app.update(InputEvent::Key {
            action: KeyAction::Text("c".into()),
            at_ms: 10,
        });
        assert!(matches!(app.engine.status(), TestStatus::Running { .. }));

        handle_key(
            &mut app,
            &repository,
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )
        .unwrap();

        assert!(matches!(app.engine.status(), TestStatus::Ready));
        assert!(!app.repeated_test);
        assert_ne!(app.session_kind, SessionKind::Repeat);
        assert!(
            handle_key(
                &mut app,
                &repository,
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            )
            .unwrap()
        );
    }

    #[test]
    fn repeticao_do_terminal_so_repete_entrada_de_digitacao() {
        let mut app = app_de_teste(tuipe::typing::TestConfig::default(), &["casa "]);

        assert!(handle_typing_repeat(
            &mut app,
            KeyCode::Char('c'),
            KeyModifiers::NONE
        ));
        assert_eq!(app.engine.attempts()[0].input, "c");
        assert!(handle_typing_repeat(
            &mut app,
            KeyCode::Backspace,
            KeyModifiers::NONE
        ));
        assert!(app.engine.attempts()[0].input.is_empty());

        app.settings_open = true;
        assert!(!handle_typing_repeat(
            &mut app,
            KeyCode::Char('q'),
            KeyModifiers::NONE
        ));
        assert!(app.engine.attempts()[0].input.is_empty());
    }

    #[test]
    fn configuracoes_explicam_e_respeitam_a_direcao_das_setas() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let mut app = app_de_teste(tuipe::typing::TestConfig::default(), &["casa "]);
        app.settings_open = true;

        handle_settings_key(&mut app, &repository, KeyCode::Tab, KeyModifiers::NONE).unwrap();
        assert_eq!(app.settings_focus, 1);
        handle_settings_key(&mut app, &repository, KeyCode::Right, KeyModifiers::NONE).unwrap();
        assert!(app.preferences.test.numbers);

        handle_settings_key(&mut app, &repository, KeyCode::Down, KeyModifiers::NONE).unwrap();
        assert_eq!(app.settings_focus, 2);
        handle_settings_key(&mut app, &repository, KeyCode::Right, KeyModifiers::NONE).unwrap();
        assert!(matches!(
            app.preferences.test.mode,
            TestMode::Words { count: 25 }
        ));
        handle_settings_key(&mut app, &repository, KeyCode::Left, KeyModifiers::NONE).unwrap();
        assert!(matches!(
            app.preferences.test.mode,
            TestMode::Time { seconds: 30 }
        ));

        handle_settings_key(&mut app, &repository, KeyCode::Up, KeyModifiers::NONE).unwrap();
        assert_eq!(app.settings_focus, 1);
        let numbers = app.preferences.test.numbers;
        handle_settings_key(&mut app, &repository, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert_eq!(app.preferences.test.numbers, numbers);
        assert!(!app.settings_open);

        let quote_config = tuipe::typing::TestConfig {
            mode: TestMode::Quote,
            ..tuipe::typing::TestConfig::default()
        };
        let mut quote = app_de_teste(quote_config, &["casa "]);
        handle_key(&mut quote, &repository, KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(quote.settings_open);
        assert_eq!(quote.settings_focus, 2);
        handle_settings_key(&mut quote, &repository, KeyCode::Up, KeyModifiers::NONE).unwrap();
        assert_eq!(quote.settings_focus, 8);
    }

    #[test]
    fn atalho_personalizado_substitui_o_padrao_no_resultado() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let mut app = app_de_teste(tuipe::typing::TestConfig::default(), &["que "]);
        app.preferences.keymap.repeat = crokey::parse("ctrl-r").unwrap();
        app.update(InputEvent::Key {
            action: KeyAction::Text("que ".into()),
            at_ms: 10,
        });
        app.update(InputEvent::Tick { at_ms: 30_100 });
        app.started = Instant::now() - Duration::from_secs(31);

        handle_key(
            &mut app,
            &repository,
            KeyCode::Char('r'),
            KeyModifiers::NONE,
        )
        .unwrap();
        assert!(matches!(app.engine.status(), TestStatus::Completed { .. }));

        handle_key(
            &mut app,
            &repository,
            KeyCode::Char('r'),
            KeyModifiers::CONTROL,
        )
        .unwrap();
        assert!(matches!(app.engine.status(), TestStatus::Ready));
    }

    #[test]
    fn somente_o_controle_de_sair_devolve_ordem_de_encerramento() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = Repository::open(&temporary.path().join("history.db")).unwrap();
        let mut app = app_de_teste(tuipe::typing::TestConfig::default(), &["que "]);
        app.update(InputEvent::Key {
            action: KeyAction::Text("que ".into()),
            at_ms: 10,
        });
        app.update(InputEvent::Tick { at_ms: 30_100 });
        app.started = Instant::now() - Duration::from_secs(31);
        app.persisted = true;

        let vazio = handle_mouse(
            &mut app,
            &repository,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 95,
                row: 27,
                modifiers: KeyModifiers::NONE,
            },
            ratatui::layout::Size::new(100, 28),
        )
        .unwrap();
        assert_eq!(vazio, MouseOutcome::Unchanged);

        let quit_x = (0..100)
            .find(|x| {
                ui::result_action_at(
                    ratatui::layout::Rect::new(0, 0, 100, 28),
                    &app.preferences.keymap,
                    false,
                    ratatui::layout::Position::new(*x, 27),
                ) == Some(ui::ResultAction::Quit)
            })
            .expect("controle de sair visível");
        let outcome = handle_mouse(
            &mut app,
            &repository,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: quit_x,
                row: 27,
                modifiers: KeyModifiers::NONE,
            },
            ratatui::layout::Size::new(100, 28),
        )
        .unwrap();

        assert_eq!(outcome, MouseOutcome::Quit);
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

        let words = (0..100)
            .map(|index| format!("palavra{index}"))
            .collect::<Vec<_>>();
        let config = tuipe::typing::TestConfig::default();
        let adaptive = AdaptiveSampler::new(AdaptivePolicy::default());
        let (practice, _) = session_word_pool(&words, &config, &adaptive, SessionKind::Practice);
        let (transfer, _) = session_word_pool(&words, &config, &adaptive, SessionKind::Transfer);
        assert!(practice.iter().all(|word| !is_transfer_holdout(word)));
        assert!(transfer.iter().all(|word| is_transfer_holdout(word)));
    }

    #[test]
    fn chance_de_retencao_respeita_o_limite_real_do_teste() {
        let catalog = ContentCatalog::bundled().unwrap();
        let config = tuipe::typing::TestConfig::default();
        let mut adaptive = AdaptiveSampler::default();
        adaptive.set_review_states(
            [
                (
                    "portuguese".into(),
                    "casa".into(),
                    tuipe::adaptive::ReviewState {
                        last_seen_unix_s: 1,
                        consecutive_clean_sessions: 1,
                    },
                ),
                (
                    "portuguese".into(),
                    "tempo".into(),
                    tuipe::adaptive::ReviewState {
                        last_seen_unix_s: 2,
                        consecutive_clean_sessions: 1,
                    },
                ),
            ],
            4 * 86_400,
        );
        let chances = estimated_generator_chances(
            &catalog,
            &config,
            &adaptive,
            SessionKind::Retention,
            &["casa".into(), "tempo".into()],
            1,
            CURRENT_POLICY_VERSION,
        )
        .unwrap();

        let values = [chances["casa"], chances["tempo"]];
        assert_eq!(values.iter().filter(|chance| **chance == 1.0).count(), 1);
        assert_eq!(values.iter().filter(|chance| **chance == 0.0).count(), 1);
    }

    #[test]
    fn chance_adaptativa_considera_os_draws_substituidos_por_numeros() {
        let catalog = ContentCatalog::bundled().unwrap();
        let adaptive = AdaptiveSampler::default();
        let config = tuipe::typing::TestConfig::default();
        let targets = catalog
            .word_pack(&config.language, &config.word_pack)
            .unwrap()
            .to_vec();
        let plain_chances = adaptive.estimated_session_chances_with_number_probability(
            &config.language,
            &targets,
            &targets,
            30,
            0.0,
        );
        let numbered_chances = adaptive.estimated_session_chances_with_number_probability(
            &config.language,
            &targets,
            &targets,
            30,
            0.1,
        );

        assert!(numbered_chances.values().sum::<f64>() < plain_chances.values().sum::<f64>());
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
        let observations = app.observations(&Default::default(), false);
        assert_eq!(observations.len(), 1);
        assert!(observations[0].censored);
        assert!(!observations[0].confirmed_error);
        assert_eq!(observations[0].evidence_weight, 0.0);
    }

    #[test]
    fn repeticao_manual_nao_reforca_o_modelo_adaptativo() {
        let config = tuipe::typing::TestConfig {
            mode: TestMode::Words { count: 1 },
            difficulty: tuipe::typing::Difficulty::Normal,
            ..tuipe::typing::TestConfig::default()
        };
        let mut app = app_de_teste(config, &["casa"]);
        app.repeated_test = true;
        app.update(InputEvent::Key {
            action: KeyAction::Text("casa".into()),
            at_ms: 500,
        });

        let observations = app.observations(&Default::default(), false);
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].evidence_weight, 0.0);
        assert!(observations[0].selection_source.is_none());
        assert!(observations[0].selection_propensity.is_none());
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
        let observations = app.observations(&Default::default(), false);
        assert_eq!(observations[0].fluent_ms, 400);
        assert_eq!(observations[0].correction_ms, 200);
        assert_eq!(observations[0].corrective_events, 1);
        assert_eq!(observations[0].input_events, 7);
    }
}
