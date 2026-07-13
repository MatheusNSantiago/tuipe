use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{Axis, Block, BorderType, Borders, Chart, Clear, Dataset, GraphType, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    content::Theme,
    typing::{Difficulty, Metrics, QuoteLength, TestEngine, TestMode, TestStatus},
};

const MIN_PAGE_PADDING: u16 = 2;
const WORD_GAP: usize = 2;
const CONFIG_MIN_WIDTH: u16 = 84;

pub fn render(
    frame: &mut Frame,
    engine: &TestEngine,
    theme: &Theme,
    settings_open: bool,
    theme_name: &str,
) {
    let viewport = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(color(&theme.bg))),
        viewport,
    );

    let content = page_content(viewport);
    let ready = matches!(engine.status(), TestStatus::Ready);

    // O Monkeytype mantém a geometria da página enquanto o chrome desaparece
    // ao ganhar foco. Reservar as linhas impede que as palavras saltem quando
    // o primeiro caractere inicia o teste.
    if ready {
        render_header(
            frame,
            Rect::new(content.x, viewport.y + 2, content.width, 2),
            theme,
        );
        render_config_bar(frame, viewport, engine, theme);
    }

    let compact = viewport.height < 18;
    let test_top = if compact {
        viewport.y + 6
    } else {
        viewport.y + viewport.height.saturating_mul(39) / 100
    };
    let bottom_reserve = if compact { 2 } else { 5 };
    let test_area = Rect::new(
        content.x,
        test_top,
        content.width,
        viewport
            .bottom()
            .saturating_sub(test_top)
            .saturating_sub(bottom_reserve),
    );

    match engine.status() {
        TestStatus::Completed { .. } | TestStatus::Failed { .. } => {
            render_result(frame, test_area, engine, theme)
        }
        TestStatus::Ready | TestStatus::Running { .. } => {
            render_test(frame, test_area, engine, theme)
        }
    }
    if ready
        || matches!(
            engine.status(),
            TestStatus::Completed { .. } | TestStatus::Failed { .. }
        )
    {
        render_footer(
            frame,
            Rect::new(
                content.x,
                viewport.bottom().saturating_sub(2),
                content.width,
                2,
            ),
            engine,
            theme,
        );
    }
    if settings_open {
        render_settings(frame, viewport, engine, theme, theme_name);
    }
}

fn render_header(frame: &mut Frame, area: Rect, theme: &Theme) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("⌨ ", Style::default().fg(color(&theme.main))),
            Span::styled(
                "tuipe",
                Style::default()
                    .fg(color(&theme.text))
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        area,
    );
}

fn render_settings(
    frame: &mut Frame,
    viewport: Rect,
    engine: &TestEngine,
    theme: &Theme,
    theme_name: &str,
) {
    for y in viewport.y..viewport.bottom() {
        for x in viewport.x..viewport.right() {
            let style = frame.buffer_mut()[(x, y)].style();
            frame.buffer_mut()[(x, y)].set_style(style.add_modifier(Modifier::DIM));
        }
    }
    let area = centered_width(centered_height(viewport, 21), 58);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(color(&theme.sub_alt)))
            .style(Style::default().bg(color(&theme.bg))),
        area,
    );
    let inner = Rect {
        x: area.x + 2,
        y: area.y + 2,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(4),
    };
    let config = engine.config();
    let lines = vec![
        Line::styled("test settings", Style::default().fg(color(&theme.sub))),
        Line::from(""),
        button_group(
            &[
                ("p punctuation", config.punctuation),
                ("n numbers", config.numbers),
            ],
            theme,
        ),
        Line::from(""),
        button_group(
            &[
                ("m time", matches!(config.mode, TestMode::Time { .. })),
                ("m words", matches!(config.mode, TestMode::Words { .. })),
                ("m quote", matches!(config.mode, TestMode::Quote)),
            ],
            theme,
        ),
        match config.mode {
            TestMode::Time { seconds } => button_group(
                &[
                    ("v 15", seconds == 15),
                    ("v 30", seconds == 30),
                    ("v 60", seconds == 60),
                    ("v 120", seconds == 120),
                ],
                theme,
            ),
            TestMode::Words { count } => button_group(
                &[
                    ("v 10", count == 10),
                    ("v 25", count == 25),
                    ("v 50", count == 50),
                    ("v 100", count == 100),
                ],
                theme,
            ),
            TestMode::Quote => button_group(
                &[
                    ("all", config.quote_length == QuoteLength::All),
                    ("short", config.quote_length == QuoteLength::Short),
                    ("medium", config.quote_length == QuoteLength::Medium),
                    ("long", config.quote_length == QuoteLength::Long),
                ],
                theme,
            ),
        },
        Line::from(""),
        button_group(
            &[
                ("d normal", config.difficulty == Difficulty::Normal),
                ("d expert", config.difficulty == Difficulty::Expert),
                ("d master", config.difficulty == Difficulty::Master),
            ],
            theme,
        ),
        button_group(&[("a adaptive", config.adaptive)], theme),
        Line::from(""),
        button_group(
            &[
                ("l portuguese", config.language == "portuguese"),
                ("l english", config.language == "english"),
            ],
            theme,
        ),
        button_group(
            &[
                ("k common", config.word_pack == "common"),
                ("k 1k", config.word_pack == "1k"),
                ("k 5k", config.word_pack == "5k"),
            ],
            theme,
        ),
        Line::from(vec![
            Span::styled("t theme  ", Style::default().fg(color(&theme.sub))),
            chip(theme_name.to_owned(), true, theme),
        ]),
        Line::from(""),
        Line::styled("esc close", Style::default().fg(color(&theme.sub))),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_config_bar(frame: &mut Frame, viewport: Rect, engine: &TestEngine, theme: &Theme) {
    let area = config_bar_area(viewport);
    let Some(cards) = config_card_areas(viewport) else {
        let card = centered_width(area, 21.min(area.width));
        render_card(
            frame,
            card,
            Line::styled("⚙  test settings", Style::default().fg(color(&theme.sub))),
            theme,
        );
        return;
    };

    let config = engine.config();
    let active = Style::default()
        .fg(color(&theme.main))
        .add_modifier(Modifier::BOLD);
    let idle = Style::default().fg(color(&theme.sub));

    let modifiers = Line::from(vec![
        selector("@ punctuation", config.punctuation, active, idle),
        Span::raw("  "),
        selector("# numbers", config.numbers, active, idle),
    ]);
    render_card(frame, cards[0], modifiers, theme);

    let modes = Line::from(vec![
        selector(
            "◷ time",
            matches!(config.mode, TestMode::Time { .. }),
            active,
            idle,
        ),
        Span::raw("  "),
        selector(
            "A words",
            matches!(config.mode, TestMode::Words { .. }),
            active,
            idle,
        ),
        Span::raw("  "),
        selector(
            "❝ quote",
            matches!(config.mode, TestMode::Quote),
            active,
            idle,
        ),
    ]);
    render_card(frame, cards[1], modes, theme);

    let values = match config.mode {
        TestMode::Time { seconds } => choices(&[15, 30, 60, 120], seconds, active, idle),
        TestMode::Words { count } => choices(&[10, 25, 50, 100], count, active, idle),
        TestMode::Quote => choice_names(
            &["all", "short", "medium", "long"],
            match config.quote_length {
                QuoteLength::All => 0,
                QuoteLength::Short => 1,
                QuoteLength::Medium => 2,
                QuoteLength::Long => 3,
            },
            active,
            idle,
        ),
    };
    render_card(frame, cards[2], values, theme);
}

pub fn config_bar_area(viewport: Rect) -> Rect {
    let content = page_content(viewport);
    let y = if viewport.height < 18 {
        viewport.y + 4
    } else {
        viewport.y + (viewport.height / 9).max(5)
    };
    Rect::new(content.x, y, content.width, 3)
}

pub fn config_card_areas(viewport: Rect) -> Option<[Rect; 3]> {
    let area = config_bar_area(viewport);
    if area.width < CONFIG_MIN_WIDTH {
        return None;
    }
    let row = centered_width(area, CONFIG_MIN_WIDTH);
    let layout = Layout::horizontal([
        Constraint::Length(28),
        Constraint::Length(2),
        Constraint::Length(28),
        Constraint::Length(2),
        Constraint::Length(24),
    ])
    .split(row);
    Some([layout[0], layout[2], layout[4]])
}

fn render_card(frame: &mut Frame, area: Rect, line: Line<'static>, theme: &Theme) {
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(color(&theme.sub_alt)))
            .style(Style::default().bg(color(&theme.sub_alt))),
        area,
    );
    let text = Rect::new(
        area.x.saturating_add(1),
        area.y + 1,
        area.width.saturating_sub(2),
        1,
    );
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Center), text);
}

fn render_test(frame: &mut Frame, area: Rect, engine: &TestEngine, theme: &Theme) {
    let text_width = area.width;
    if text_width < 20 || area.height < 4 {
        frame.render_widget(
            Paragraph::new("terminal too small")
                .alignment(Alignment::Center)
                .style(Style::default().fg(color(&theme.error))),
            area,
        );
        return;
    }

    let wrapped = wrap_words(styled_words(engine, theme), usize::from(text_width));
    let active_line = wrapped.iter().position(|line| line.active).unwrap_or(0);
    // O lineJump upstream só remove uma linha quando a palavra ativa entra na
    // quarta linha visual, mantendo a linha ativa no fim da janela de três linhas.
    let first_line = first_visible_line(active_line);
    let visible = wrapped
        .iter()
        .skip(first_line)
        .take(3)
        .map(|line| line.content.clone())
        .collect::<Vec<_>>();

    let word_step = if area.height >= 7 { 2 } else { 1 };
    let first_word_y = area.y + 2;

    frame.render_widget(
        Paragraph::new(test_descriptor(engine))
            .style(Style::default().fg(color(&theme.sub)))
            .alignment(Alignment::Center),
        Rect::new(area.x, area.y, area.width, 1),
    );
    if matches!(engine.status(), TestStatus::Running { .. }) {
        frame.render_widget(
            Paragraph::new(mini_progress(engine)).style(Style::default().fg(color(&theme.main))),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }
    for (index, line) in visible.into_iter().enumerate() {
        frame.render_widget(
            Paragraph::new(line).style(Style::default().fg(color(&theme.sub))),
            Rect::new(
                area.x,
                first_word_y + u16::try_from(index).unwrap_or(0) * word_step,
                area.width,
                1,
            ),
        );
    }

    if let Some(cursor_col) = wrapped.get(active_line).and_then(|line| line.cursor_col) {
        let visible_line = active_line.saturating_sub(first_line) as u16;
        frame.set_cursor_position((
            area.x + cursor_col.min(area.width.saturating_sub(1)),
            first_word_y + visible_line * word_step,
        ));
    }
}

const fn first_visible_line(active_line: usize) -> usize {
    active_line.saturating_sub(2)
}

fn render_result(frame: &mut Frame, area: Rect, engine: &TestEngine, theme: &Theme) {
    let metrics = engine.metrics();
    let body = centered_height(area, 13.min(area.height));
    let top_height = body.height.saturating_sub(5).max(5);
    let top = Rect::new(body.x, body.y, body.width, top_height);
    let columns = Layout::horizontal([Constraint::Length(16), Constraint::Min(24)]).split(top);

    render_primary_result(frame, columns[0], &metrics, theme);
    render_result_chart(frame, columns[1], &metrics, theme);

    let details_top = top.bottom().saturating_add(1);
    render_result_details(
        frame,
        Rect::new(
            body.x,
            details_top,
            body.width,
            body.bottom().saturating_sub(details_top),
        ),
        engine,
        &metrics,
        theme,
    );
}

fn render_primary_result(frame: &mut Frame, area: Rect, metrics: &Metrics, theme: &Theme) {
    let primary = vec![
        Line::styled("wpm", Style::default().fg(color(&theme.sub))),
        Line::styled(
            format!("{:.0}", metrics.wpm),
            Style::default()
                .fg(color(&theme.main))
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(""),
        Line::styled("acc", Style::default().fg(color(&theme.sub))),
        Line::styled(
            format!("{:.0}%", metrics.accuracy),
            Style::default()
                .fg(color(&theme.main))
                .add_modifier(Modifier::BOLD),
        ),
    ];
    frame.render_widget(Paragraph::new(primary), area);
}

fn render_result_chart(frame: &mut Frame, area: Rect, metrics: &Metrics, theme: &Theme) {
    let wpm_points = metrics
        .burst_history
        .iter()
        .enumerate()
        .map(|(index, value)| (index as f64 + 1.0, *value))
        .collect::<Vec<_>>();
    let error_points = metrics
        .error_history
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        .map(|(index, _)| (index as f64 + 1.0, metrics.burst_history[index]))
        .collect::<Vec<_>>();
    let seconds = (metrics.duration_ms as f64 / 1_000.0)
        .ceil()
        .max(wpm_points.last().map_or(1.0, |point| point.0));
    let peak_wpm = wpm_points
        .iter()
        .map(|point| point.1)
        .fold(metrics.raw_wpm.max(metrics.wpm), f64::max);
    let chart_ceiling = ((peak_wpm.max(20.0) / 20.0).ceil() * 20.0).max(20.0);
    let datasets = vec![
        Dataset::default()
            .data(&wpm_points)
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(color(&theme.main))),
        Dataset::default()
            .data(&error_points)
            .marker(Marker::Dot)
            .graph_type(GraphType::Scatter)
            .style(Style::default().fg(color(&theme.error))),
    ];
    frame.render_widget(
        Chart::new(datasets)
            .style(Style::default().fg(color(&theme.sub_alt)))
            .x_axis(
                Axis::default()
                    .bounds([0.0, seconds])
                    .labels(["0".to_owned(), format!("{seconds:.0}s")])
                    .style(Style::default().fg(color(&theme.sub))),
            )
            .y_axis(
                Axis::default()
                    .bounds([0.0, chart_ceiling])
                    .labels(["0".to_owned(), format!("{chart_ceiling:.0}")])
                    .style(Style::default().fg(color(&theme.sub))),
            ),
        area,
    );
}

fn render_result_details(
    frame: &mut Frame,
    area: Rect,
    engine: &TestEngine,
    metrics: &Metrics,
    theme: &Theme,
) {
    let stats = metrics.characters;
    let failed_word = match engine.status() {
        TestStatus::Failed { word_index, .. } => Some(*word_index),
        TestStatus::Ready | TestStatus::Running { .. } | TestStatus::Completed { .. } => None,
    };
    let mut details = vec![result_group_lines(
        "test type",
        result_descriptor(engine),
        theme,
    )];
    if let Some(word_index) = failed_word {
        details.push(failure_group_lines(engine, word_index, theme));
    }
    details.extend([
        result_group_lines("raw", format!("{:.0}", metrics.raw_wpm), theme),
        result_group_lines(
            "characters",
            format!(
                "{}/{}/{}/{}",
                stats.correct_word, stats.incorrect, stats.extra, stats.missed
            ),
            theme,
        ),
        result_group_lines("consistency", format!("{:.0}%", metrics.consistency), theme),
        result_group_lines(
            "time",
            format!("{:.1}s", metrics.duration_ms as f64 / 1_000.0),
            theme,
        ),
    ]);

    let groups = Layout::horizontal(result_detail_constraints(failed_word.is_some()))
        .spacing(2)
        .split(area);
    for (group_area, lines) in groups.iter().zip(details) {
        frame.render_widget(Paragraph::new(lines), *group_area);
    }
}

fn result_detail_constraints(failed: bool) -> Vec<Constraint> {
    if failed {
        vec![
            Constraint::Percentage(22),
            Constraint::Percentage(24),
            Constraint::Percentage(9),
            Constraint::Percentage(19),
            Constraint::Percentage(16),
            Constraint::Percentage(10),
        ]
    } else {
        vec![
            Constraint::Percentage(28),
            Constraint::Percentage(13),
            Constraint::Percentage(23),
            Constraint::Percentage(19),
            Constraint::Percentage(17),
        ]
    }
}

fn result_group_lines(name: &str, result: String, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(
        name.to_owned(),
        Style::default().fg(color(&theme.sub)),
    )];
    lines.extend(
        result
            .lines()
            .map(|line| Line::styled(line.to_owned(), Style::default().fg(color(&theme.main)))),
    );
    lines
}

fn failure_group_lines(
    engine: &TestEngine,
    word_index: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let target = engine.targets()[word_index].with_commit();
    let input = &engine.attempts()[word_index].input;
    vec![
        Line::styled("other", Style::default().fg(color(&theme.sub))),
        Line::styled(
            "failed (difficulty)",
            Style::default().fg(color(&theme.error)),
        ),
        failed_word_line(input, &target, theme),
    ]
}

fn failed_word_line(input: &str, target: &str, theme: &Theme) -> Line<'static> {
    let typed = input.graphemes(true).collect::<Vec<_>>();
    let expected = target.graphemes(true).collect::<Vec<_>>();
    let mut spans = Vec::new();
    for (index, grapheme) in typed.iter().enumerate() {
        let correct = expected.get(index).is_some_and(|value| value == grapheme);
        spans.push(Span::styled(
            visible_grapheme(grapheme),
            Style::default().fg(color(if correct { &theme.text } else { &theme.error })),
        ));
    }
    spans.push(label("  →  ", theme));
    spans.push(value(
        expected
            .iter()
            .map(|grapheme| visible_grapheme(grapheme))
            .collect::<String>(),
        theme,
    ));
    Line::from(spans)
}

fn visible_grapheme(grapheme: &str) -> String {
    match grapheme {
        " " => "·".to_owned(),
        "\n" => "↵".to_owned(),
        _ => grapheme.to_owned(),
    }
}

fn render_footer(frame: &mut Frame, area: Rect, engine: &TestEngine, theme: &Theme) {
    let line = match engine.status() {
        TestStatus::Ready => key_hints(
            &[("enter", "restart"), ("esc", "settings"), ("q", "quit")],
            theme,
        ),
        TestStatus::Running { .. } => return,
        TestStatus::Completed { .. } | TestStatus::Failed { .. } => key_hints(
            &[
                ("enter", "next"),
                ("r", "repeat"),
                ("h", "words"),
                ("s", "stats"),
                ("q", "quit"),
            ],
            theme,
        ),
    };
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Center), area);
}

struct StyledWord {
    spans: Vec<Span<'static>>,
    width: usize,
    cursor_col: Option<usize>,
    active: bool,
}

struct WrappedLine {
    content: Line<'static>,
    cursor_col: Option<u16>,
    active: bool,
}

fn styled_words(engine: &TestEngine, theme: &Theme) -> Vec<StyledWord> {
    engine
        .targets()
        .iter()
        .enumerate()
        .map(|(word_index, target)| {
            let attempt = &engine.attempts()[word_index];
            let input = attempt.without_commit();
            let target_text = target.text.clone();
            let target_graphemes = target_text.graphemes(true).collect::<Vec<_>>();
            let typed_graphemes = input.graphemes(true).collect::<Vec<_>>();
            let active = word_index == engine.active_word();
            let mut spans = Vec::new();
            for index in 0..target_graphemes.len().max(typed_graphemes.len()) {
                let typed = typed_graphemes.get(index).copied();
                let expected = target_graphemes.get(index).copied();
                let mut style = match (typed, expected) {
                    (Some(actual), Some(expected)) if actual == expected => {
                        Style::default().fg(color(&theme.text))
                    }
                    (Some(_), Some(_)) => Style::default().fg(color(&theme.error)),
                    (Some(_), None) => Style::default().fg(color(&theme.error_extra)),
                    (None, Some(_)) => Style::default().fg(color(&theme.sub)),
                    (None, None) => unreachable!(),
                };
                if attempt.committed && input != target_text {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                spans.push(Span::styled(
                    typed.or(expected).unwrap_or_default().to_owned(),
                    style,
                ));
            }
            let width = spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                .sum();
            let cursor_col = active.then(|| UnicodeWidthStr::width(input.as_str()).min(width));
            StyledWord {
                spans,
                width,
                cursor_col,
                active,
            }
        })
        .collect()
}

fn wrap_words(words: Vec<StyledWord>, width: usize) -> Vec<WrappedLine> {
    let mut lines = vec![(Vec::new(), false, None)];
    let mut line_width = 0;
    for word in words {
        let required = if line_width == 0 {
            word.width
        } else {
            WORD_GAP + word.width
        };
        if line_width > 0 && line_width + required > width {
            lines.push((Vec::new(), false, None));
            line_width = 0;
        }
        let line = lines.last_mut().expect("at least one line");
        if line_width > 0 {
            line.0.push(Span::raw(" ".repeat(WORD_GAP)));
            line_width += WORD_GAP;
        }
        if let Some(cursor_col) = word.cursor_col {
            line.2 = Some((line_width + cursor_col).try_into().unwrap_or(u16::MAX));
        }
        line.0.extend(word.spans);
        line.1 |= word.active;
        line_width += word.width;
    }
    lines
        .into_iter()
        .map(|(spans, active, cursor_col)| WrappedLine {
            content: Line::from(spans),
            cursor_col,
            active,
        })
        .collect()
}

fn mini_progress(engine: &TestEngine) -> String {
    match engine.config().mode {
        TestMode::Time { seconds } => {
            let elapsed = engine.elapsed_ms() / 1_000;
            u64::from(seconds).saturating_sub(elapsed).to_string()
        }
        TestMode::Words { count } => format!("{}/{}", engine.active_word() + 1, count),
        TestMode::Quote => format!("{}/{}", engine.active_word() + 1, engine.targets().len()),
    }
}

fn test_descriptor(engine: &TestEngine) -> String {
    let config = engine.config();
    let mut modifiers = vec![difficulty_name(config.difficulty)];
    if config.punctuation {
        modifiers.push("punctuation");
    }
    if config.numbers {
        modifiers.push("numbers");
    }
    if config.adaptive && !matches!(config.mode, TestMode::Quote) {
        modifiers.push("adaptive");
    }
    format!(
        "{} {} · {}",
        config.language,
        config.word_pack,
        modifiers.join(" · ")
    )
}

fn result_descriptor(engine: &TestEngine) -> String {
    let config = engine.config();
    let mode = match config.mode {
        TestMode::Time { seconds } => format!("time {seconds}"),
        TestMode::Words { count } => format!("words {count}"),
        TestMode::Quote => "quote".into(),
    };
    let mut modifiers = vec![difficulty_name(config.difficulty)];
    if config.punctuation {
        modifiers.push("punctuation");
    }
    if config.numbers {
        modifiers.push("numbers");
    }
    if config.adaptive && !matches!(config.mode, TestMode::Quote) {
        modifiers.push("adaptive");
    }
    format!(
        "{mode}\n{} {}\n{}",
        config.language,
        config.word_pack,
        modifiers.join(" · ")
    )
}

fn key_hints(hints: &[(&str, &str)], theme: &Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (key, action)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("    "));
        }
        spans.push(Span::styled(
            (*key).to_owned(),
            Style::default().fg(color(&theme.text)),
        ));
        spans.push(Span::styled(
            format!(" {action}"),
            Style::default().fg(color(&theme.sub)),
        ));
    }
    Line::from(spans)
}

fn button_group(buttons: &[(&str, bool)], theme: &Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (text, active)) in buttons.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(chip((*text).to_owned(), *active, theme));
    }
    Line::from(spans)
}

fn chip(text: String, active: bool, theme: &Theme) -> Span<'static> {
    let style = if active {
        Style::default().fg(color(&theme.bg)).bg(color(&theme.main))
    } else {
        Style::default()
            .fg(color(&theme.sub))
            .bg(color(&theme.sub_alt))
    };
    Span::styled(format!(" {text} "), style)
}

fn selector<'a>(text: &'a str, selected: bool, active: Style, idle: Style) -> Span<'a> {
    Span::styled(text, if selected { active } else { idle })
}

fn choices<T: std::fmt::Display + Copy + PartialEq>(
    values: &[T],
    selected: T,
    active: Style,
    idle: Style,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(
            value.to_string(),
            if *value == selected { active } else { idle },
        ));
    }
    Line::from(spans)
}

fn choice_names(values: &[&str], selected: usize, active: Style, idle: Style) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            (*value).to_owned(),
            if index == selected { active } else { idle },
        ));
    }
    Line::from(spans)
}

fn label(text: impl Into<String>, theme: &Theme) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(color(&theme.sub)))
}

fn value(text: impl Into<String>, theme: &Theme) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(color(&theme.text)))
}

fn difficulty_name(difficulty: Difficulty) -> &'static str {
    match difficulty {
        Difficulty::Normal => "normal",
        Difficulty::Expert => "expert",
        Difficulty::Master => "master",
    }
}

fn centered_width(area: Rect, maximum: u16) -> Rect {
    let width = area.width.min(maximum);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        width,
        ..area
    }
}

fn page_content(area: Rect) -> Rect {
    let padding = (area.width / 20).max(MIN_PAGE_PADDING);
    Rect {
        x: area.x + padding,
        width: area.width.saturating_sub(padding * 2),
        ..area
    }
}

fn centered_height(area: Rect, height: u16) -> Rect {
    let height = height.min(area.height);
    Rect {
        y: area.y + area.height.saturating_sub(height) / 2,
        height,
        ..area
    }
}

fn color(value: &str) -> Color {
    value
        .parse::<csscolorparser::Color>()
        .map(|parsed| {
            let [red, green, blue, _] = parsed.to_rgba8();
            Color::Rgb(red, green, blue)
        })
        .unwrap_or(Color::Reset)
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::{
        content::ContentCatalog,
        typing::{InputEvent, KeyAction, TestConfig},
    };

    fn render_at(width: u16, height: u16) -> String {
        let engine = TestEngine::new(
            TestConfig::default(),
            ["olá ".into(), "mundo ".into(), "prática ".into()],
        );
        render_engine_at(width, height, &engine)
    }

    fn render_engine_at(width: u16, height: u16, engine: &TestEngine) -> String {
        render_engine_with_settings(width, height, engine, false)
    }

    fn render_engine_with_settings(
        width: u16,
        height: u16,
        engine: &TestEngine,
        settings_open: bool,
    ) -> String {
        let catalog = ContentCatalog::bundled().unwrap();
        let theme = catalog.theme("arch").unwrap();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, engine, theme, settings_open, "arch"))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn test_screen_renders_at_small_medium_and_ultrawide_sizes() {
        for (width, height) in [(50, 14), (100, 28), (180, 40)] {
            let rendered = render_at(width, height);
            assert!(rendered.contains(if width < 80 { "test settings" } else { "time" }));
            assert!(rendered.contains("olá"));
            if width >= 100 {
                assert!(rendered.contains("adaptive"));
            }
            insta::assert_snapshot!(format!("test_{width}x{height}"), rendered);
        }
    }

    #[test]
    fn running_and_result_hierarchy_stay_stable() {
        let mut engine = TestEngine::new(
            TestConfig::default(),
            ["olá ".into(), "mundo ".into(), "prática ".into()],
        );
        engine.update(InputEvent::Key {
            action: KeyAction::Text("ol".into()),
            at_ms: 100,
        });
        insta::assert_snapshot!("test_running_100x28", render_engine_at(100, 28, &engine));

        engine.update(InputEvent::Key {
            action: KeyAction::Text("á mundo prática ".into()),
            at_ms: 10_000,
        });
        engine.update(InputEvent::Tick { at_ms: 30_100 });
        insta::assert_snapshot!("test_result_100x28", render_engine_at(100, 28, &engine));
    }

    #[test]
    fn failed_result_keeps_the_result_hierarchy_and_exposes_the_cause() {
        let config = TestConfig {
            mode: TestMode::Words { count: 3 },
            ..TestConfig::default()
        };
        let mut engine =
            TestEngine::new(config, ["filho ".into(), "mundo ".into(), "prática".into()]);
        engine.update(InputEvent::Key {
            action: KeyAction::Text("fil".into()),
            at_ms: 100,
        });
        engine.update(InputEvent::Key {
            action: KeyAction::Text("ha ".into()),
            at_ms: 1_100,
        });

        assert!(matches!(
            engine.status(),
            TestStatus::Failed { word_index: 0, .. }
        ));
        insta::assert_snapshot!("test_failed_100x28", render_engine_at(100, 28, &engine));
        insta::assert_snapshot!("test_failed_180x40", render_engine_at(180, 40, &engine));
    }

    #[test]
    fn settings_overlay_stays_compact() {
        let engine = TestEngine::new(
            TestConfig::default(),
            ["olá ".into(), "mundo ".into(), "prática ".into()],
        );
        insta::assert_snapshot!(
            "settings_100x28",
            render_engine_with_settings(100, 28, &engine, true)
        );
    }

    #[test]
    fn line_jump_only_happens_after_the_active_word_enters_a_fourth_line() {
        assert_eq!(
            (0..6).map(first_visible_line).collect::<Vec<_>>(),
            [0, 0, 0, 1, 2, 3]
        );
    }
}
