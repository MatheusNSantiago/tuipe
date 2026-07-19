use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, Paragraph,
        canvas::{Canvas, Line as CanvasLine, Points},
    },
};
use spline1d::pchip;
use std::env;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    content::Theme,
    persistence::{PriorityWord, SessionSummary, StatisticsOverview},
    typing::{Difficulty, Metrics, QuoteLength, TestEngine, TestMode, TestStatus},
};

const MIN_PAGE_PADDING: u16 = 2;
const WORD_GAP: usize = 1;
const CONFIG_MIN_WIDTH: u16 = 76;
const CONFIG_QUOTE_WIDTH: u16 = 82;
const CONFIG_CARD_GAP: u16 = 2;
const CONFIG_MODIFIER_WIDTH: u16 = 26;
const CONFIG_MODE_WIDTH: u16 = 26;
const CONFIG_COMPACT_VALUE_WIDTH: u16 = 20;
const CONFIG_QUOTE_VALUE_WIDTH: u16 = 26;
const RESULT_WIDE_WIDTH: u16 = 90;
const RESULT_MEDIUM_WIDTH: u16 = 54;
const RESULT_GROUP_HEIGHT: u16 = 4;
const RESULT_CHART_HEIGHT: u16 = 12;
const RESULT_AXIS_LABEL_WIDTH: u16 = 4;
const CURVE_SAMPLES_PER_INTERVAL: u16 = 16;

#[derive(Clone, Copy)]
struct Icons {
    teclado: &'static str,
    configuracoes: &'static str,
    tempo: &'static str,
    palavras: &'static str,
    citacao: &'static str,
    idioma: &'static str,
    dificuldade: &'static str,
}

const ICONES_UNICODE: Icons = Icons {
    teclado: "⌨",
    configuracoes: "⚙",
    tempo: "◷",
    palavras: "Aa",
    citacao: "❝",
    idioma: "◎",
    dificuldade: "★",
};

const ICONES_NERD: Icons = Icons {
    teclado: "",
    configuracoes: "",
    tempo: "",
    palavras: "",
    citacao: "",
    idioma: "",
    dificuldade: "",
};

fn icones_do_terminal() -> Icons {
    match env::var("TUIPE_ICONS").ok().as_deref() {
        Some("unicode") => ICONES_UNICODE,
        _ => ICONES_NERD,
    }
}

pub fn render(
    frame: &mut Frame,
    engine: &TestEngine,
    theme: &Theme,
    settings_open: bool,
    theme_name: &str,
) {
    render_com_icones(
        frame,
        engine,
        theme,
        settings_open,
        theme_name,
        icones_do_terminal(),
    );
}

pub fn render_statistics(frame: &mut Frame, statistics: &StatisticsOverview, theme: &Theme) {
    let viewport = frame.area();
    frame.render_widget(Clear, viewport);
    frame.render_widget(
        Block::default().style(Style::default().bg(color(&theme.bg))),
        viewport,
    );
    let content = page_content(viewport);
    if statistics.completed_tests == 0 {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("estatísticas", Style::default().fg(color(&theme.text))),
                Line::from(""),
                Line::styled(
                    "ainda não há testes concluídos",
                    Style::default().fg(color(&theme.sub)),
                ),
                Line::from(""),
                Line::styled("esc voltar", Style::default().fg(color(&theme.sub))),
            ])
            .alignment(Alignment::Center),
            centered_height(content, 7),
        );
        return;
    }

    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(11.min(content.height.saturating_sub(9))),
        Constraint::Length(3),
        Constraint::Min(7),
        Constraint::Length(1),
    ])
    .split(content);
    frame.render_widget(
        Paragraph::new("estatísticas").style(Style::default().fg(color(&theme.text))),
        sections[0],
    );
    render_statistics_chart(frame, sections[1], &statistics.recent_tests, theme);
    render_statistics_summary(frame, sections[2], statistics, theme);
    let details = Layout::horizontal([Constraint::Percentage(67), Constraint::Percentage(33)])
        .spacing(1)
        .split(sections[3]);
    render_recent_tests(frame, details[0], &statistics.recent_tests, theme);
    render_priority_words(frame, details[1], &statistics.priority_words, theme);
    frame.render_widget(
        Paragraph::new("esc voltar").style(Style::default().fg(color(&theme.sub))),
        sections[4],
    );
}

fn render_statistics_chart(
    frame: &mut Frame,
    area: Rect,
    sessions: &[SessionSummary],
    theme: &Theme,
) {
    if area.height < 4 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("wpm por teste", Style::default().fg(color(&theme.text))),
            Span::styled(
                format!("  ·  {} testes mais recentes", sessions.len()),
                Style::default().fg(color(&theme.sub)),
            ),
        ])),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let columns =
        Layout::horizontal([Constraint::Length(4), Constraint::Min(10)]).split(Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        ));
    let points = sessions
        .iter()
        .map(|session| (session.id as f64, session.wpm))
        .collect::<Vec<_>>();
    let ceiling =
        ((points.iter().map(|point| point.1).fold(20.0, f64::max) / 20.0).ceil() * 20.0).max(20.0);
    let canvas_area = Rect::new(
        columns[1].x,
        columns[1].y,
        columns[1].width,
        columns[1].height.saturating_sub(1),
    );
    render_chart_y_labels(frame, columns[0], canvas_area, ceiling, theme);
    frame.render_widget(
        Canvas::default()
            .marker(Marker::Braille)
            .background_color(color(&theme.bg))
            .block(
                Block::default()
                    .borders(Borders::LEFT | Borders::BOTTOM)
                    .border_style(Style::default().fg(color(&theme.main))),
            )
            .x_bounds([
                points.first().map_or(0.0, |point| point.0),
                points.last().map_or(1.0, |point| point.0).max(1.0),
            ])
            .y_bounds([0.0, ceiling])
            .paint(|context| {
                context.draw(&Points {
                    coords: &points,
                    color: color(&theme.main),
                });
            }),
        canvas_area,
    );
    if let (Some(first), Some(last)) = (sessions.first(), sessions.last()) {
        let middle = sessions[sessions.len() / 2].id;
        let labels = Layout::horizontal([
            Constraint::Percentage(33),
            Constraint::Percentage(34),
            Constraint::Percentage(33),
        ])
        .split(Rect::new(
            columns[1].x,
            canvas_area.bottom(),
            columns[1].width,
            1,
        ));
        for (label, value, alignment) in [
            (labels[0], format!("#{}", first.id), Alignment::Left),
            (labels[1], format!("#{middle}"), Alignment::Center),
            (labels[2], format!("#{}", last.id), Alignment::Right),
        ] {
            frame.render_widget(
                Paragraph::new(value)
                    .style(Style::default().fg(color(&theme.sub)))
                    .alignment(alignment),
                label,
            );
        }
    }
}

fn render_statistics_summary(
    frame: &mut Frame,
    area: Rect,
    statistics: &StatisticsOverview,
    theme: &Theme,
) {
    let values = [
        ("testes", statistics.completed_tests.to_string()),
        ("wpm médio", format!("{:.0}", statistics.average_wpm)),
        ("precisão", format!("{:.0}%", statistics.average_accuracy)),
        ("melhor", format!("{:.0}", statistics.best_wpm)),
        ("tempo", format_duration(statistics.active_ms)),
    ];
    for (area, (label, value)) in Layout::horizontal(vec![Constraint::Ratio(1, 5); 5])
        .spacing(2)
        .split(area)
        .iter()
        .zip(values)
    {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(label, Style::default().fg(color(&theme.sub))),
                Line::styled(value, Style::default().fg(color(&theme.main))),
            ]),
            *area,
        );
    }
}

fn render_recent_tests(frame: &mut Frame, area: Rect, sessions: &[SessionSummary], theme: &Theme) {
    let mut lines = vec![Line::styled(
        "histórico recente",
        Style::default().fg(color(&theme.text)),
    )];
    lines.push(Line::styled(
        "teste    wpm  bruto  precisão  caracteres  duração",
        Style::default().fg(color(&theme.sub)),
    ));
    lines.extend(
        sessions
            .iter()
            .rev()
            .take(area.height.saturating_sub(2) as usize)
            .map(|session| {
                Line::from(vec![
                    Span::styled(
                        format!("#{:<7}", session.id),
                        Style::default().fg(color(&theme.sub)),
                    ),
                    Span::styled(
                        format!("{:<5.0}", session.wpm),
                        Style::default().fg(color(&theme.main)),
                    ),
                    Span::styled(
                        format!("{:<7.0}", session.raw_wpm),
                        Style::default().fg(color(&theme.main)),
                    ),
                    Span::styled(
                        format!("{:<10}", format!("{:.0}%", session.accuracy)),
                        Style::default().fg(color(&theme.main)),
                    ),
                    Span::styled(
                        format!(
                            "{}/{}/{}      ",
                            session.correct_chars, session.incorrect_chars, session.extra_chars
                        ),
                        Style::default().fg(color(&theme.main)),
                    ),
                    Span::styled(
                        format_duration(session.elapsed_ms),
                        Style::default().fg(color(&theme.sub)),
                    ),
                ])
            }),
    );
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_priority_words(frame: &mut Frame, area: Rect, words: &[PriorityWord], theme: &Theme) {
    let mut lines = vec![Line::styled(
        "palavras prioritárias",
        Style::default().fg(color(&theme.text)),
    )];
    if words.is_empty() {
        lines.push(Line::styled(
            "sem evidência suficiente",
            Style::default().fg(color(&theme.sub)),
        ));
    } else {
        lines.push(Line::styled(
            "palavra       chance   erros  correções",
            Style::default().fg(color(&theme.sub)),
        ));
        lines.extend(
            words
                .iter()
                .take(area.height.saturating_sub(2) as usize)
                .map(|word| {
                    Line::from(vec![
                        Span::styled(
                            format!("{:<14}", word.word),
                            Style::default().fg(color(&theme.main)),
                        ),
                        Span::styled(
                            format!("{:>5.1}%   ", word.estimated_session_chance * 100.0),
                            Style::default().fg(color(&theme.main)),
                        ),
                        Span::styled(
                            format!("{:>3.0}    ", word.confirmed_errors),
                            Style::default().fg(color(&theme.error)),
                        ),
                        Span::styled(
                            format!("{:>3.0}", word.corrections),
                            Style::default().fg(color(&theme.sub)),
                        ),
                    ])
                }),
        );
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_com_icones(
    frame: &mut Frame,
    engine: &TestEngine,
    theme: &Theme,
    settings_open: bool,
    theme_name: &str,
    icones: Icons,
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
            icones,
        );
        render_config_bar(frame, viewport, engine, theme, icones);
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
    let result_area = Rect::new(
        content.x,
        viewport.y,
        content.width,
        viewport.height.saturating_sub(3),
    );

    match engine.status() {
        TestStatus::Completed { .. } | TestStatus::Failed { .. } => {
            render_result(frame, result_area, engine, theme, icones)
        }
        TestStatus::Ready | TestStatus::Running { .. } => {
            render_test(frame, test_area, engine, theme, icones)
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
            icones,
        );
    }
    if settings_open {
        render_settings(frame, viewport, engine, theme, theme_name, icones);
    }
}

fn render_header(frame: &mut Frame, area: Rect, theme: &Theme, icones: Icons) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} ", icones.teclado),
                Style::default().fg(color(&theme.main)),
            ),
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
    icones: Icons,
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
        Line::styled(
            format!("{} configurações do teste", icones.configuracoes),
            Style::default().fg(color(&theme.sub)),
        ),
        Line::from(""),
        button_group(
            &[
                ("p pontuação", config.punctuation),
                ("n números", config.numbers),
            ],
            theme,
        ),
        Line::from(""),
        button_group(
            &[
                ("m tempo", matches!(config.mode, TestMode::Time { .. })),
                ("m palavras", matches!(config.mode, TestMode::Words { .. })),
                ("m citação", matches!(config.mode, TestMode::Quote)),
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
                    ("todas", config.quote_length == QuoteLength::All),
                    ("curta", config.quote_length == QuoteLength::Short),
                    ("média", config.quote_length == QuoteLength::Medium),
                    ("longa", config.quote_length == QuoteLength::Long),
                ],
                theme,
            ),
        },
        Line::from(""),
        button_group(
            &[
                ("d normal", config.difficulty == Difficulty::Normal),
                ("d especialista", config.difficulty == Difficulty::Expert),
                ("d mestre", config.difficulty == Difficulty::Master),
            ],
            theme,
        ),
        button_group(&[("a adaptativo", config.adaptive)], theme),
        Line::from(""),
        button_group(
            &[
                ("l português", config.language == "portuguese"),
                ("l inglês", config.language == "english"),
            ],
            theme,
        ),
        button_group(
            &[
                ("k comum", config.word_pack == "common"),
                ("k 1k", config.word_pack == "1k"),
                ("k 5k", config.word_pack == "5k"),
            ],
            theme,
        ),
        Line::from(vec![
            Span::styled("t tema  ", Style::default().fg(color(&theme.sub))),
            chip(theme_name.to_owned(), true, theme),
        ]),
        Line::from(""),
        Line::styled("esc fechar", Style::default().fg(color(&theme.sub))),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_config_bar(
    frame: &mut Frame,
    viewport: Rect,
    engine: &TestEngine,
    theme: &Theme,
    icones: Icons,
) {
    let area = config_bar_area(viewport);
    let config = engine.config();
    let Some(cards) = config_card_areas(viewport, &config.mode) else {
        let card = centered_width(area, 21.min(area.width));
        render_card(
            frame,
            card,
            Line::styled(
                format!("{}  configurações", icones.configuracoes),
                Style::default().fg(color(&theme.sub)),
            ),
            theme,
        );
        return;
    };

    let active = Style::default()
        .fg(color(&theme.main))
        .add_modifier(Modifier::BOLD);
    let idle = Style::default().fg(color(&theme.sub));

    let modifiers = Line::from(vec![
        selector("@ pontuação", config.punctuation, active, idle),
        Span::raw(" "),
        selector("# números", config.numbers, active, idle),
    ]);
    render_card(frame, cards[0], modifiers, theme);

    let modes = Line::from(vec![
        selector(
            format!("{} tempo", icones.tempo),
            matches!(config.mode, TestMode::Time { .. }),
            active,
            idle,
        ),
        Span::raw(" "),
        selector(
            format!("{} palavras", icones.palavras),
            matches!(config.mode, TestMode::Words { .. }),
            active,
            idle,
        ),
        Span::raw(" "),
        selector(
            format!("{} citação", icones.citacao),
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

pub fn config_card_areas(viewport: Rect, mode: &TestMode) -> Option<[Rect; 3]> {
    let area = config_bar_area(viewport);
    let value_width = if matches!(mode, TestMode::Quote) {
        CONFIG_QUOTE_VALUE_WIDTH
    } else {
        CONFIG_COMPACT_VALUE_WIDTH
    };
    let row_width =
        CONFIG_MODIFIER_WIDTH + CONFIG_CARD_GAP + CONFIG_MODE_WIDTH + CONFIG_CARD_GAP + value_width;
    let minimum_width = if matches!(mode, TestMode::Quote) {
        CONFIG_QUOTE_WIDTH
    } else {
        CONFIG_MIN_WIDTH
    };
    if area.width < minimum_width {
        return None;
    }
    let row = centered_width(area, row_width);
    let layout = Layout::horizontal([
        Constraint::Length(CONFIG_MODIFIER_WIDTH),
        Constraint::Length(CONFIG_CARD_GAP),
        Constraint::Length(CONFIG_MODE_WIDTH),
        Constraint::Length(CONFIG_CARD_GAP),
        Constraint::Length(value_width),
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

fn render_test(frame: &mut Frame, area: Rect, engine: &TestEngine, theme: &Theme, icones: Icons) {
    let text_width = area.width;
    if text_width < 20 || area.height < 4 {
        frame.render_widget(
            Paragraph::new("terminal pequeno demais")
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
        Paragraph::new(test_descriptor(engine, icones))
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

fn render_result(frame: &mut Frame, area: Rect, engine: &TestEngine, theme: &Theme, icones: Icons) {
    let metrics = engine.metrics();
    let group_count = 7;
    let details_height = result_details_height(area.width, group_count);
    let body = centered_height(
        area,
        (RESULT_CHART_HEIGHT + 1 + details_height).min(area.height),
    );
    let top_height = body
        .height
        .saturating_sub(details_height.saturating_add(1))
        .max(5)
        .min(body.height);
    let top = Rect::new(body.x, body.y, body.width, top_height);
    render_result_chart(frame, top, &metrics, theme);

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
        icones,
    );
}

fn render_result_chart(frame: &mut Frame, area: Rect, metrics: &Metrics, theme: &Theme) {
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "wpm ao longo do tempo",
                Style::default().fg(color(&theme.text)),
            ),
            Span::raw("   "),
            Span::styled("× erros", Style::default().fg(color(&theme.error))),
        ])),
        sections[0],
    );

    let chart_columns = Layout::horizontal([
        Constraint::Length(RESULT_AXIS_LABEL_WIDTH),
        Constraint::Min(10),
    ])
    .split(sections[1]);
    let labels = chart_columns[0];
    let plot = chart_columns[1];
    let wpm_points = metrics
        .wpm_history
        .iter()
        .enumerate()
        .map(|(index, value)| (index as f64, *value))
        .collect::<Vec<_>>();
    let error_points = metrics
        .error_history
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        .map(|(index, _)| (index as f64, metrics.wpm_history[index]))
        .collect::<Vec<_>>();
    let last_point = wpm_points.len().saturating_sub(1) as f64;
    let smooth_wpm_points = smooth_wpm_points(&wpm_points);
    let peak_wpm = wpm_points
        .iter()
        .map(|point| point.1)
        .fold(metrics.raw_wpm.max(metrics.wpm), f64::max);
    let chart_ceiling = ((peak_wpm.max(20.0) / 20.0).ceil() * 20.0).max(20.0);

    render_chart_y_labels(frame, labels, plot, chart_ceiling, theme);
    frame.render_widget(
        Canvas::default()
            .marker(Marker::Braille)
            .background_color(color(&theme.bg))
            .block(
                Block::default()
                    .borders(Borders::LEFT | Borders::BOTTOM)
                    .border_style(Style::default().fg(color(&theme.main))),
            )
            .x_bounds([0.0, last_point.max(1.0)])
            .y_bounds([0.0, chart_ceiling])
            .paint(|context| {
                context.draw(&Points {
                    coords: &wpm_points,
                    color: color(&theme.main),
                });
                for points in smooth_wpm_points.windows(2) {
                    context.draw(&CanvasLine {
                        x1: points[0].0,
                        y1: points[0].1,
                        x2: points[1].0,
                        y2: points[1].1,
                        color: color(&theme.main),
                    });
                }
                context.layer();
                context.marker(Marker::Custom('×'));
                context.draw(&Points {
                    coords: &error_points,
                    color: color(&theme.error),
                });
            }),
        plot,
    );
    render_chart_x_labels(frame, sections[2], plot, metrics, theme);
}

fn smooth_wpm_points(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    if points.len() < 2 {
        return points.to_vec();
    }

    let xs = points.iter().map(|(x, _)| *x).collect::<Vec<_>>();
    let ys = points.iter().map(|(_, y)| *y).collect::<Vec<_>>();
    let spline = pchip(&xs, &ys);
    let mut smoothed =
        Vec::with_capacity((points.len() - 1) * CURVE_SAMPLES_PER_INTERVAL as usize + 1);

    for (index, _) in points.windows(2).enumerate() {
        for step in 0..CURVE_SAMPLES_PER_INTERVAL {
            let x = index as f64 + f64::from(step) / f64::from(CURVE_SAMPLES_PER_INTERVAL);
            let y = spline
                .interpolate(&x)
                .expect("amostras de WPM possuem coordenadas estritamente crescentes");
            smoothed.push((x, y));
        }
    }
    smoothed.push(*points.last().expect("histórico de WPM não está vazio"));
    smoothed
}

fn render_chart_y_labels(frame: &mut Frame, area: Rect, plot: Rect, ceiling: f64, theme: &Theme) {
    let style = Style::default().fg(color(&theme.main));
    for (offset, label) in [
        (0, format!("{ceiling:.0}")),
        (
            plot.height.saturating_sub(1) / 2,
            format!("{:.0}", ceiling / 2.0),
        ),
        (plot.height.saturating_sub(1), "0".to_owned()),
    ] {
        frame.render_widget(
            Paragraph::new(Line::styled(label, style)).alignment(Alignment::Right),
            Rect::new(area.x, plot.y + offset, area.width, 1),
        );
    }
}

fn render_chart_x_labels(
    frame: &mut Frame,
    area: Rect,
    plot: Rect,
    metrics: &Metrics,
    theme: &Theme,
) {
    let style = Style::default().fg(color(&theme.main));
    let duration = format_chart_duration(metrics.duration_ms);
    frame.render_widget(
        Paragraph::new(Line::styled("1", style)),
        Rect::new(plot.x, area.y, 4.min(plot.width), 1),
    );
    if metrics.wpm_history.len() >= 3 {
        frame.render_widget(
            Paragraph::new(Line::styled(
                format_chart_seconds((1.0 + metrics.duration_ms as f64 / 1_000.0) / 2.0),
                style,
            ))
            .alignment(Alignment::Center),
            Rect::new(plot.x, area.y, plot.width, 1),
        );
    }
    frame.render_widget(
        Paragraph::new(Line::styled(duration.clone(), style)).alignment(Alignment::Right),
        Rect::new(
            plot.right().saturating_sub(duration.len() as u16),
            area.y,
            duration.len() as u16,
            1,
        ),
    );
}

fn format_chart_seconds(seconds: f64) -> String {
    if seconds.fract().abs() < f64::EPSILON {
        format!("{seconds:.0}")
    } else {
        format!("{seconds:.1}")
    }
}

fn format_chart_duration(duration_ms: u64) -> String {
    format_chart_seconds((duration_ms as f64 / 1_000.0).max(1.0))
}

fn render_result_details(
    frame: &mut Frame,
    area: Rect,
    engine: &TestEngine,
    metrics: &Metrics,
    theme: &Theme,
    icones: Icons,
) {
    let stats = metrics.characters;
    let details = vec![
        result_group_lines("tipo de teste", result_descriptor(engine, icones), theme),
        result_group_lines("wpm", format!("{:.0}", metrics.wpm), theme),
        result_group_lines("precisão", format!("{:.0}%", metrics.accuracy), theme),
        result_group_lines("bruto", format!("{:.0}", metrics.raw_wpm), theme),
        result_group_lines(
            "caracteres",
            format!(
                "{}/{}/{}/{}",
                stats.correct_word, stats.incorrect, stats.extra, stats.missed
            ),
            theme,
        ),
        result_group_lines(
            "consistência",
            format!("{:.0}%", metrics.consistency),
            theme,
        ),
        result_group_lines(
            "tempo",
            format!("{:.1}s", metrics.duration_ms as f64 / 1_000.0),
            theme,
        ),
    ];

    for (group_area, lines) in result_detail_areas(area, details.len())
        .into_iter()
        .zip(details)
    {
        frame.render_widget(Paragraph::new(lines), group_area);
    }
}

fn result_detail_columns(width: u16, group_count: usize) -> usize {
    if width >= RESULT_WIDE_WIDTH {
        group_count
    } else if width >= RESULT_MEDIUM_WIDTH {
        3
    } else {
        2
    }
}

fn result_details_height(width: u16, group_count: usize) -> u16 {
    let columns = result_detail_columns(width, group_count);
    let rows = group_count.div_ceil(columns) as u16;
    rows * RESULT_GROUP_HEIGHT + rows.saturating_sub(1)
}

fn result_detail_areas(area: Rect, group_count: usize) -> Vec<Rect> {
    let columns = result_detail_columns(area.width, group_count);
    if columns == group_count {
        let constraints = if group_count == 7 {
            vec![
                Constraint::Length(16),
                Constraint::Length(8),
                Constraint::Length(11),
                Constraint::Length(8),
                Constraint::Length(13),
                Constraint::Length(14),
                Constraint::Min(8),
            ]
        } else if group_count == 6 {
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
        };
        return Layout::horizontal(constraints)
            .spacing(2)
            .split(area)
            .to_vec();
    }

    let rows = group_count.div_ceil(columns);
    let row_areas = Layout::vertical(vec![Constraint::Length(RESULT_GROUP_HEIGHT); rows])
        .spacing(1)
        .split(area);
    row_areas
        .iter()
        .flat_map(|row| {
            Layout::horizontal(vec![Constraint::Ratio(1, columns as u32); columns])
                .spacing(2)
                .split(*row)
                .to_vec()
        })
        .collect()
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

fn render_footer(
    frame: &mut Frame,
    area: Rect,
    engine: &TestEngine,
    theme: &Theme,
    _icones: Icons,
) {
    let line = match engine.status() {
        TestStatus::Ready => key_hints(
            &[
                ("enter", "reiniciar"),
                ("esc", "configurações"),
                ("q", "sair"),
            ],
            theme,
        ),
        TestStatus::Running { .. } => return,
        TestStatus::Completed { .. } | TestStatus::Failed { .. } => key_hints(
            &[
                ("enter", "próximo"),
                ("r", "repetir"),
                ("s", "estatísticas"),
                ("q", "sair"),
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

fn test_descriptor(engine: &TestEngine, icones: Icons) -> String {
    let config = engine.config();
    let mut modifiers = vec![difficulty_name(config.difficulty)];
    if config.punctuation {
        modifiers.push("pontuação");
    }
    if config.numbers {
        modifiers.push("números");
    }
    format!(
        "{} {} · {} {}",
        icones.idioma,
        language_descriptor(&config.language, &config.word_pack),
        icones.dificuldade,
        modifiers.join(" · ")
    )
}

fn result_descriptor(engine: &TestEngine, icones: Icons) -> String {
    let config = engine.config();
    let mode = match config.mode {
        TestMode::Time { seconds } => format!("{} {seconds} segundos", icones.tempo),
        TestMode::Words { count } => format!("palavras {count}"),
        TestMode::Quote => "citação".into(),
    };
    let mut modifiers = vec![difficulty_name(config.difficulty)];
    if config.punctuation {
        modifiers.push("pontuação");
    }
    if config.numbers {
        modifiers.push("números");
    }
    format!(
        "{mode}\n{} {}\n{} {}",
        icones.idioma,
        language_descriptor(&config.language, &config.word_pack),
        icones.dificuldade,
        modifiers.join(" · ")
    )
}

fn format_duration(duration_ms: u64) -> String {
    let total_seconds = duration_ms / 1_000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    if minutes == 0 {
        format!("{seconds}s")
    } else {
        format!("{minutes}m {seconds:02}s")
    }
}

fn language_name(language: &str) -> &str {
    match language {
        "portuguese" => "português",
        "english" => "inglês",
        _ => language,
    }
}

fn language_descriptor(language: &str, pack: &str) -> String {
    match pack {
        "common" => language_name(language).into(),
        _ => format!("{} {pack}", language_name(language)),
    }
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

fn selector(text: impl Into<String>, selected: bool, active: Style, idle: Style) -> Span<'static> {
    Span::styled(text.into(), if selected { active } else { idle })
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
            spans.push(Span::raw("  "));
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
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            (*value).to_owned(),
            if index == selected { active } else { idle },
        ));
    }
    Line::from(spans)
}

fn difficulty_name(difficulty: Difficulty) -> &'static str {
    match difficulty {
        Difficulty::Normal => "normal",
        Difficulty::Expert => "especialista",
        Difficulty::Master => "mestre",
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
            .draw(|frame| {
                render_com_icones(frame, engine, theme, settings_open, "arch", ICONES_UNICODE)
            })
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
            assert!(rendered.contains(if width < 80 {
                "configurações"
            } else {
                "tempo"
            }));
            assert!(rendered.contains("olá"));
            assert!(!rendered.contains("adaptativo"));
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
    fn failed_result_stays_centered_and_preserves_the_result_hierarchy() {
        let config = TestConfig {
            mode: TestMode::Words { count: 4 },
            ..TestConfig::default()
        };
        let mut engine = TestEngine::new(
            config,
            ["cada ".into(), "aqui ".into(), "sem ".into(), "fim".into()],
        );
        for (text, at_ms) in [("cada ", 100), ("aqui ", 900), ("se ", 1_800)] {
            engine.update(InputEvent::Key {
                action: KeyAction::Text(text.into()),
                at_ms,
            });
        }

        assert!(matches!(
            engine.status(),
            TestStatus::Failed { word_index: 2, .. }
        ));
        insta::assert_snapshot!("test_failed_100x28", render_engine_at(100, 28, &engine));
        insta::assert_snapshot!("test_failed_70x50", render_engine_at(70, 50, &engine));
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
    fn statistics_overview_remains_readable() {
        let catalog = ContentCatalog::bundled().unwrap();
        let theme = catalog.theme("arch").unwrap();
        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(Paragraph::new("resíduo da tela anterior"), frame.area());
                render_statistics(
                    frame,
                    &StatisticsOverview {
                        completed_tests: 42,
                        active_ms: 3_661_000,
                        average_wpm: 84.0,
                        average_accuracy: 96.0,
                        best_wpm: 112.0,
                        recent_tests: (1_u16..=12)
                            .map(|id| SessionSummary {
                                id: u64::from(id),
                                elapsed_ms: 15_000,
                                wpm: 70.0 + f64::from(id),
                                accuracy: 90.0 + f64::from(id) / 2.0,
                                raw_wpm: 70.0 + f64::from(id),
                                correct_chars: 100,
                                incorrect_chars: 0,
                                extra_chars: 0,
                                config: TestConfig::default(),
                            })
                            .collect(),
                        priority_words: Vec::new(),
                    },
                    theme,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..28)
            .map(|y| {
                (0..100)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!("statistics_100x28", rendered);
    }

    #[test]
    fn line_jump_only_happens_after_the_active_word_enters_a_fourth_line() {
        assert_eq!(
            (0..6).map(first_visible_line).collect::<Vec<_>>(),
            [0, 0, 0, 1, 2, 3]
        );
    }

    #[test]
    fn smooth_wpm_curve_keeps_the_observed_endpoints() {
        let observed = [(0.0, 120.0), (1.0, 60.0), (2.0, 80.0)];
        let curve = smooth_wpm_points(&observed);

        assert_eq!(curve.first(), Some(&observed[0]));
        assert_eq!(curve.last(), Some(&observed[2]));
        assert_eq!(curve.len(), 33);
    }
}
