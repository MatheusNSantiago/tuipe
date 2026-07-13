use std::{
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

use tuipe::{
    content::{ContentCatalog, WordGenerator},
    persistence::{Preferences, Repository, paths},
    typing::{InputEvent, KeyAction, QuoteLength, TestEngine, TestMode, TestStatus},
    ui,
};

fn main() -> Result<()> {
    let (config_path, database_path) = paths();
    let preferences = Preferences::load(&config_path)?;
    let catalog = ContentCatalog::bundled()?;
    let repository = Repository::open(&database_path)?;
    let mut app = App::new(preferences, catalog, config_path)?;

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
    generator: Option<WordGenerator<SmallRng>>,
    seed: u64,
}

impl App {
    fn new(
        preferences: Preferences,
        catalog: ContentCatalog,
        config_path: PathBuf,
    ) -> Result<Self> {
        let seed = rand::random();
        let (engine, generator) = new_test(&catalog, &preferences.test, seed)?;
        Ok(Self {
            preferences,
            catalog,
            engine,
            started: Instant::now(),
            persisted: false,
            config_path,
            settings_open: false,
            generator,
            seed,
        })
    }

    fn restart(&mut self) -> Result<()> {
        self.seed = rand::random();
        let (engine, generator) = new_test(&self.catalog, &self.preferences.test, self.seed)?;
        self.engine = engine;
        self.generator = generator;
        self.started = Instant::now();
        self.persisted = false;
        Ok(())
    }

    fn repeat(&mut self) -> Result<()> {
        let (engine, generator) = new_test(&self.catalog, &self.preferences.test, self.seed)?;
        self.engine = engine;
        self.generator = generator;
        self.started = Instant::now();
        self.persisted = false;
        Ok(())
    }

    fn apply_preference(&mut self, change: impl FnOnce(&mut Preferences)) -> Result<()> {
        change(&mut self.preferences);
        self.preferences.save(&self.config_path)?;
        self.restart()
    }

    fn elapsed_ms(&self) -> u64 {
        self.started
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
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
            self.engine
                .append_words((0..40).map(|_| format!("{} ", generator.next_word())));
        }
    }
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
                )
            })?;
            last_drawn_second = app.engine.elapsed_ms() / 1_000;
            needs_draw = false;
        }

        if event::poll(Duration::from_millis(16))? {
            let event_changed_view = match event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        && handle_key(app, key.code, key.modifiers)? =>
                {
                    break;
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => true,
                Event::Mouse(mouse) => handle_mouse(app, mouse, terminal.size()?)?,
                Event::Resize(width, height)
                    if width != last_size.width || height != last_size.height =>
                {
                    last_size = ratatui::layout::Size::new(width, height);
                    true
                }
                Event::Resize(_, _) => false,
                Event::FocusGained | Event::FocusLost | Event::Paste(_) | Event::Key(_) => false,
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
            repository.save_session(
                app.engine.config(),
                app.engine.status(),
                app.engine.metrics(),
            )?;
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

fn handle_mouse(app: &mut App, mouse: MouseEvent, terminal: ratatui::layout::Size) -> Result<bool> {
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
        app.apply_preference(|preferences| {
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
        app.apply_preference(|preferences| preferences.test.mode = mode)?;
    } else if (cards[2].x..cards[2].right()).contains(&x) {
        let quarter = ((x - cards[2].x) * 4 / cards[2].width).min(3) as usize;
        app.apply_preference(|preferences| {
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

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Result<bool> {
    if app.settings_open {
        return handle_settings_key(app, code);
    }
    if matches!(code, KeyCode::Esc) && !matches!(app.engine.status(), TestStatus::Running { .. }) {
        app.settings_open = true;
        return Ok(false);
    }
    if matches!(code, KeyCode::Char('q'))
        && !matches!(app.engine.status(), TestStatus::Running { .. })
    {
        return Ok(true);
    }
    if matches!(code, KeyCode::Enter) {
        app.restart()?;
        return Ok(false);
    }
    if matches!(code, KeyCode::Char('r'))
        && matches!(
            app.engine.status(),
            TestStatus::Completed { .. } | TestStatus::Failed { .. }
        )
    {
        app.repeat()?;
        return Ok(false);
    }

    let action = match code {
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
    };
    if let Some(action) = action {
        app.update(InputEvent::Key {
            action,
            at_ms: app.elapsed_ms(),
        });
    }
    Ok(false)
}

fn handle_settings_key(app: &mut App, code: KeyCode) -> Result<bool> {
    match code {
        KeyCode::Esc | KeyCode::Enter => app.settings_open = false,
        KeyCode::Char('m') => app.apply_preference(|preferences| {
            preferences.test.mode = match preferences.test.mode {
                TestMode::Time { .. } => TestMode::Words { count: 25 },
                TestMode::Words { .. } => TestMode::Quote,
                TestMode::Quote => TestMode::Time { seconds: 30 },
            };
        })?,
        KeyCode::Char('v') => app.apply_preference(|preferences| {
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
        KeyCode::Char('d') => app.apply_preference(|preferences| {
            preferences.test.difficulty = match preferences.test.difficulty {
                tuipe::typing::Difficulty::Normal => tuipe::typing::Difficulty::Expert,
                tuipe::typing::Difficulty::Expert => tuipe::typing::Difficulty::Master,
                tuipe::typing::Difficulty::Master => tuipe::typing::Difficulty::Normal,
            };
        })?,
        KeyCode::Char('p') => app.apply_preference(|preferences| {
            preferences.test.punctuation = !preferences.test.punctuation;
        })?,
        KeyCode::Char('n') => app.apply_preference(|preferences| {
            preferences.test.numbers = !preferences.test.numbers;
        })?,
        KeyCode::Char('a') => app.apply_preference(|preferences| {
            preferences.test.adaptive = !preferences.test.adaptive;
        })?,
        KeyCode::Char('l') => app.apply_preference(|preferences| {
            preferences.test.language = if preferences.test.language == "portuguese" {
                "english".into()
            } else {
                "portuguese".into()
            };
        })?,
        KeyCode::Char('k') => app.apply_preference(|preferences| {
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
            app.apply_preference(|preferences| {
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

fn new_test(
    catalog: &ContentCatalog,
    config: &tuipe::typing::TestConfig,
    seed: u64,
) -> Result<(TestEngine, Option<WordGenerator<SmallRng>>)> {
    let mut rng = SmallRng::seed_from_u64(seed);
    let (words, generator) = match config.mode {
        TestMode::Quote => {
            let quotes = catalog.quotes(&config.language, config.quote_length);
            let quote = quotes.choose(&mut rng).context("language has no quotes")?;
            let words = quote
                .text
                .split_whitespace()
                .map(|word| format!("{word} "))
                .collect();
            (without_last_commit(words), None)
        }
        TestMode::Words { count } => {
            let mut generator = word_generator(catalog, config, rng)?;
            let words = without_last_commit(generate(&mut generator, usize::from(count)));
            (words, None)
        }
        TestMode::Time { .. } => {
            let mut generator = word_generator(catalog, config, rng)?;
            let words = generate(&mut generator, 40);
            (words, Some(generator))
        }
    };
    Ok((TestEngine::new(config.clone(), words), generator))
}

fn word_generator(
    catalog: &ContentCatalog,
    config: &tuipe::typing::TestConfig,
    rng: SmallRng,
) -> Result<WordGenerator<SmallRng>> {
    let words = catalog
        .word_pack(&config.language, &config.word_pack)
        .context("configured word pack is unavailable")?;
    Ok(WordGenerator::new(
        words,
        rng,
        config.punctuation,
        config.numbers,
    ))
}

fn generate(generator: &mut WordGenerator<SmallRng>, count: usize) -> Vec<String> {
    (0..count)
        .map(|_| format!("{} ", generator.next_word()))
        .collect()
}

fn without_last_commit(mut words: Vec<String>) -> Vec<String> {
    if let Some(last) = words.last_mut() {
        last.pop();
    }
    words
}
