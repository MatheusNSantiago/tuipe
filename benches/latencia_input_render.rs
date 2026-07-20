use std::{hint::black_box, time::Duration, time::Instant};

use ratatui::{Terminal, backend::TestBackend};
use tuipe::{
    adaptive::{AdaptivePolicy, AdaptiveSampler, Observation},
    content::ContentCatalog,
    persistence::{Keymap, SessionKind},
    typing::{InputEvent, KeyAction, TestConfig, TestEngine, TestMode},
    ui::{self, PersistenceUiState, RenderState},
};

const AQUECIMENTO: usize = 1_000;
const AMOSTRAS: usize = 20_000;

fn main() {
    if cfg!(debug_assertions) {
        println!("benchmark ignorado em debug; use cargo bench --bench latencia_input_render");
        return;
    }
    let catalog = ContentCatalog::bundled().expect("carregar assets embarcados");
    let theme = catalog.theme("arch").expect("tema arch disponível");
    let config = TestConfig {
        mode: TestMode::Time { seconds: 120 },
        ..TestConfig::default()
    };
    let words = (0..90).map(|_| "adaptabilidade ".to_owned());
    let mut engine = TestEngine::new(config, words);
    let backend = TestBackend::new(100, 28);
    let mut terminal = Terminal::new(backend).expect("criar terminal de medição");
    let keymap = Keymap::default();
    let render_state = RenderState {
        settings_open: false,
        settings_focus: 0,
        theme_name: "arch",
        session_kind: SessionKind::Practice,
        persistence: PersistenceUiState::Saved,
        notice: None,
        focus_warning: false,
        quote: None,
        keymap: &keymap,
    };

    for sample in 0..AQUECIMENTO {
        run_sample(sample, &mut engine, &mut terminal, theme, render_state);
    }

    let mut durations = Vec::with_capacity(AMOSTRAS);
    for sample in AQUECIMENTO..AQUECIMENTO + AMOSTRAS {
        let started = Instant::now();
        run_sample(sample, &mut engine, &mut terminal, theme, render_state);
        durations.push(started.elapsed());
    }
    durations.sort_unstable();

    let p50 = percentile(&durations, 50);
    let p95 = percentile(&durations, 95);
    let p99 = percentile(&durations, 99);
    println!(
        "tecla → estado → frame (100×28, {AMOSTRAS} amostras)\np50: {}\np95: {}\np99: {}",
        format_duration(p50),
        format_duration(p95),
        format_duration(p99),
    );
    assert!(
        p99 < Duration::from_millis(16),
        "p99 ultrapassou um frame de 60 Hz: {}",
        format_duration(p99)
    );

    let resize = measure_resize(&mut engine, &mut terminal, theme, render_state);
    println!(
        "resize → frame (50×14 ↔ 180×40, {} amostras)\np50: {}\np95: {}\np99: {}",
        resize.len(),
        format_duration(percentile(&resize, 50)),
        format_duration(percentile(&resize, 95)),
        format_duration(percentile(&resize, 99)),
    );
    assert!(
        percentile(&resize, 99) < Duration::from_millis(16),
        "p99 de resize ultrapassou um frame de 60 Hz: {}",
        format_duration(percentile(&resize, 99))
    );

    let recomputation = measure_adaptive_recomputation();
    println!(
        "observações → modelo adaptativo (100 palavras, {} amostras)\np50: {}\np95: {}\np99: {}",
        recomputation.len(),
        format_duration(percentile(&recomputation, 50)),
        format_duration(percentile(&recomputation, 95)),
        format_duration(percentile(&recomputation, 99)),
    );
    assert!(
        percentile(&recomputation, 99) < Duration::from_millis(16),
        "p99 adaptativo ultrapassou um frame de 60 Hz: {}",
        format_duration(percentile(&recomputation, 99))
    );
}

fn run_sample(
    sample: usize,
    engine: &mut TestEngine,
    terminal: &mut Terminal<TestBackend>,
    theme: &tuipe::content::Theme,
    state: RenderState<'_>,
) {
    let action = if sample.is_multiple_of(2) {
        KeyAction::Text("a".to_owned())
    } else {
        KeyAction::Backspace
    };
    black_box(engine.update(InputEvent::Key {
        action,
        at_ms: sample as u64,
    }));
    terminal
        .draw(|frame| ui::render(frame, engine, theme, state))
        .expect("renderizar amostra");
    black_box(terminal.backend().buffer());
}

fn percentile(samples: &[Duration], percentile: usize) -> Duration {
    samples[(samples.len() - 1) * percentile / 100]
}

fn measure_resize(
    engine: &mut TestEngine,
    terminal: &mut Terminal<TestBackend>,
    theme: &tuipe::content::Theme,
    state: RenderState<'_>,
) -> Vec<Duration> {
    let mut durations = Vec::with_capacity(2_000);
    for sample in 0_usize..2_000 {
        let (width, height) = if sample.is_multiple_of(2) {
            (50, 14)
        } else {
            (180, 40)
        };
        let started = Instant::now();
        terminal.backend_mut().resize(width, height);
        terminal
            .draw(|frame| ui::render(frame, engine, theme, state))
            .expect("renderizar resize");
        durations.push(started.elapsed());
    }
    durations.sort_unstable();
    durations
}

fn measure_adaptive_recomputation() -> Vec<Duration> {
    let mut durations = Vec::with_capacity(1_000);
    for sample in 0..1_000 {
        let mut sampler = AdaptiveSampler::new(AdaptivePolicy::default());
        let started = Instant::now();
        for word in 0..100 {
            let word = format!("palavra{word}");
            sampler.observe(
                "portuguese",
                &word,
                Observation {
                    confirmed_error: (sample + word.len()).is_multiple_of(17),
                    corrected: (sample + word.len()).is_multiple_of(11),
                    fast_success: false,
                    slow: false,
                    latency_ratio: Some(1.1),
                    evidence_weight: 1.0,
                },
            );
            sampler.observe_mechanic("portuguese", &word, "acento", false, false, 1.0);
        }
        black_box(sampler);
        durations.push(started.elapsed());
    }
    durations.sort_unstable();
    durations
}

fn format_duration(duration: Duration) -> String {
    format!("{:.3} ms", duration.as_secs_f64() * 1_000.0)
}
