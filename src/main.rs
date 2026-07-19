use std::{
    collections::HashMap,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
};
use rand::{SeedableRng, rngs::SmallRng, seq::IndexedRandom};
use termina::{
    escape::osc::{DynamicColorNumber, Osc},
    style::RgbColor,
};
use unicode_segmentation::UnicodeSegmentation;

use tuipe::{
    adaptive::{AdaptivePolicy, AdaptiveSampler, Observation, mechanics_for_token},
    content::{ContentCatalog, WordGenerator},
    persistence::{
        MechanicObservationRecord, Preferences, RawEventCodec, RawSessionEnd, Repository,
        SessionKind, StatisticsOverview, WordObservationRecord, paths,
    },
    typing::{
        ExternalEvent, InputEvent, KeyAction, QuoteLength, RecordedInputKind, TestEngine, TestMode,
        TestStatus,
    },
    ui,
};

fn main() -> Result<()> {
    let (config_path, database_path) = paths();
    let preferences = Preferences::load(&config_path)?;
    let catalog = ContentCatalog::bundled()?;
    let repository = Repository::open(&database_path)?;
    let mut app = App::new(preferences, catalog, config_path, &repository)?;

    ratatui::run(|terminal| {
        execute!(
            std::io::stdout(),
            EnableMouseCapture,
            SetCursorStyle::BlinkingBar
        )?;
        let _mouse_guard = scopeguard::guard((), |_| {
            let mut stdout = std::io::stdout();
            let _ = write!(
                stdout,
                "{}",
                Osc::ResetDynamicColor(DynamicColorNumber::TextCursorColor)
            );
            let _ = execute!(
                stdout,
                DisableMouseCapture,
                SetCursorStyle::DefaultUserShape
            );
        });
        run(terminal, &mut app, &repository)
    })
}

struct App {
    preferences: Preferences,
    catalog: ContentCatalog,
    engine: TestEngine,
    started: Instant,
    persisted: bool,
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
}

impl App {
    fn new(
        preferences: Preferences,
        catalog: ContentCatalog,
        config_path: PathBuf,
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
        for language in ["portuguese", "english"] {
            adaptive.set_baseline(language, repository.baseline_profile(language)?.rates);
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
        })
    }

    fn persist_interrupted(&mut self, repository: &Repository, end: RawSessionEnd) -> Result<()> {
        if self.persisted || self.engine.recorded_events().is_empty() {
            return Ok(());
        }
        let raw_events =
            RawEventCodec::materialize(self.engine.recorded_events(), self.elapsed_ms(), end);
        repository.save_session_full_kind(
            self.engine.config(),
            self.engine.status(),
            self.engine.metrics(),
            &[],
            &raw_events,
            self.session_kind,
        )?;
        self.persisted = true;
        Ok(())
    }

    fn restart(&mut self, repository: &Repository) -> Result<()> {
        self.persist_interrupted(repository, RawSessionEnd::Restarted)?;
        self.adaptive.set_baseline(
            self.preferences.test.language.clone(),
            repository
                .baseline_profile(&self.preferences.test.language)?
                .rates,
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

    fn bloqueia_atalhos_do_resultado(&self) -> bool {
        let terminou_em = match self.engine.status() {
            TestStatus::Completed { ended_at_ms } | TestStatus::Failed { ended_at_ms, .. } => {
                *ended_at_ms
            }
            TestStatus::Ready | TestStatus::Running { .. } => return false,
        };
        self.elapsed_ms().saturating_sub(terminou_em) < 300
    }

    fn update(&mut self, event: InputEvent) {
        self.engine.update(event);
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
            if (attempt.committed || terminal_failure)
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
                if !attempt.committed && !terminal_failure {
                    return None;
                }
                let word = lexical_word(&target.text)?;
                let (active_ms, afk_ms) = timings.get(word_index).copied().unwrap_or_default();
                let grapheme_count = word.graphemes(true).count().try_into().unwrap_or(u16::MAX);
                let active_per_grapheme = active_ms as f64 / f64::from(grapheme_count.max(1));
                let latency_baseline = baseline.latency_ms_per_grapheme(grapheme_count);
                let confirmed_error = terminal_failure
                    || (attempt.committed && attempt.without_commit() != target.text);
                let fast_success = attempt.committed
                    && !confirmed_error
                    && attempt.corrections == 0
                    && latency_baseline
                        .is_some_and(|baseline| active_per_grapheme <= baseline * 0.8);
                let slow = latency_baseline
                    .is_some_and(|baseline| active_per_grapheme >= baseline * 1.5);
                let evidence_weight = if self.repeated_test {
                    0.0
                } else {
                    1.0 / occurrences
                        .get(&word)
                        .copied()
                        .unwrap_or(1)
                        .max(1) as f64
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
                    afk_ms,
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
    fn word_timings(&self) -> Vec<(u64, u64)> {
        #[derive(Clone, Copy)]
        struct Gap {
            word_index: usize,
            elapsed_ms: u64,
            interrupted: bool,
        }

        let mut gaps = Vec::new();
        let mut previous_key = None::<(u64, usize)>;
        let mut interrupted = false;
        for event in self.engine.recorded_events() {
            match &event.kind {
                RecordedInputKind::Focus { gained } => {
                    if !gained {
                        interrupted = true;
                    }
                }
                RecordedInputKind::Insert { .. } | RecordedInputKind::Delete { .. } => {
                    if let Some((previous_at, previous_word)) = previous_key
                        && previous_word == event.word_index
                    {
                        gaps.push(Gap {
                            word_index: event.word_index,
                            elapsed_ms: event.at_ms.saturating_sub(previous_at),
                            interrupted,
                        });
                    }
                    previous_key = Some((event.at_ms, event.word_index));
                    interrupted = false;
                }
                RecordedInputKind::Paste { .. } => {}
            }
        }

        let mut log_intervals = gaps
            .iter()
            .filter(|gap| !gap.interrupted && gap.elapsed_ms > 0)
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

        let mut timings = vec![(0_u64, 0_u64); self.engine.targets().len()];
        for gap in gaps {
            let is_pause = gap.interrupted
                || pause_threshold.is_some_and(|threshold| {
                    gap.elapsed_ms > 0 && (gap.elapsed_ms as f64).ln() > threshold
                });
            let timing = &mut timings[gap.word_index];
            if is_pause {
                timing.1 = timing.1.saturating_add(gap.elapsed_ms);
            } else {
                timing.0 = timing.0.saturating_add(gap.elapsed_ms);
            }
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
            if record.evidence_weight > 0.0 {
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
) -> Result<()> {
    let mut needs_draw = true;
    let mut last_drawn_second = 0;
    let mut last_size = terminal.size()?;
    let mut last_cursor_color = String::new();
    loop {
        if needs_draw {
            let theme = app
                .catalog
                .theme(&app.preferences.theme)
                .context("configured theme is unavailable")?;
            if theme.caret != last_cursor_color {
                set_cursor_color(&theme.caret)?;
                last_cursor_color.clone_from(&theme.caret);
            }
            terminal.draw(|frame| {
                ui::render(
                    frame,
                    &app.engine,
                    theme,
                    app.settings_open,
                    &app.preferences.theme,
                );
                if app.statistics_open {
                    ui::render_statistics(frame, &app.statistics, theme);
                }
            })?;
            last_drawn_second = app.engine.elapsed_ms() / 1_000;
            needs_draw = false;
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
        if !app.persisted
            && matches!(
                app.engine.status(),
                TestStatus::Completed { .. } | TestStatus::Failed { .. }
            )
        {
            let baseline = repository.baseline_profile(&app.engine.config().language)?;
            let observations = app.observations(&baseline);
            let end = if matches!(app.engine.status(), TestStatus::Failed { .. }) {
                RawSessionEnd::Failed
            } else {
                RawSessionEnd::Completed
            };
            let ended_at_ms = match app.engine.status() {
                TestStatus::Completed { ended_at_ms } | TestStatus::Failed { ended_at_ms, .. } => {
                    *ended_at_ms
                }
                _ => unreachable!("estado terminal validado acima"),
            };
            let raw_events =
                RawEventCodec::materialize(app.engine.recorded_events(), ended_at_ms, end);
            repository.save_session_full_kind(
                app.engine.config(),
                app.engine.status(),
                app.engine.metrics(),
                &observations,
                &raw_events,
                app.session_kind,
            )?;
            app.apply_observations(&observations);
            app.persisted = true;
        }
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
        && !matches!(app.engine.status(), TestStatus::Running { .. })
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
        KeyCode::Char('p') => app.apply_preference(repository, |preferences| {
            preferences.test.punctuation = !preferences.test.punctuation;
        })?,
        KeyCode::Char('n') => app.apply_preference(repository, |preferences| {
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
            let (words, selections) = generate(&mut generator, 40);
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
    let words = catalog
        .word_pack(&config.language, &config.word_pack)
        .context("configured word pack is unavailable")?;
    let generator = WordGenerator::new(words, rng, config.punctuation, config.numbers);
    Ok(
        if config.adaptive
            && !matches!(config.mode, TestMode::Quote)
            && session_kind != SessionKind::Assessment
        {
            generator.with_adaptive(&config.language, adaptive.clone())
        } else {
            generator
        },
    )
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
}
