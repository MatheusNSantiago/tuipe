use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, Paragraph, Wrap,
        canvas::{Canvas, Line as CanvasLine, Points},
    },
};
use spline1d::pchip;
use std::{
    cell::RefCell,
    collections::HashMap,
    env,
    io::Read,
    process::{Command, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};
use supports_color::Stream;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    content::Theme,
    persistence::{
        ActivityDay, Keymap, PriorityPattern, PriorityWord, SessionDetail, SessionHistoryItem,
        SessionKind, SessionOutcome, SessionSummary, StatisticsOverview, WordDetail, WpmBucket,
    },
    typing::{Difficulty, Metrics, QuoteLength, TestEngine, TestMode, TestStatus},
};

const MIN_PAGE_PADDING: u16 = 2;
const WORD_GAP: usize = 1;
const CONFIG_CARD_GAP: u16 = 1;
const CONFIG_MODIFIER_WIDTH: u16 = 27;
// O grupo central é o mais largo. A largura inclui as bordas e precisa caber
// tanto com Nerd Font quanto com o fallback Unicode, cujos rótulos não têm a
// mesma largura de célula.
const CONFIG_MODE_WIDTH: u16 = 37;
const CONFIG_COMPACT_VALUE_WIDTH: u16 = 24;
const CONFIG_QUOTE_VALUE_WIDTH: u16 = 33;
const RESULT_WIDE_WIDTH: u16 = 90;
const RESULT_MEDIUM_WIDTH: u16 = 54;
const RESULT_GROUP_HEIGHT: u16 = 4;
const RESULT_CHART_HEIGHT: u16 = 12;
const RESULT_STATUS_HEIGHT: u16 = 4;
const RESULT_AXIS_LABEL_WIDTH: u16 = 4;
const RESULT_ERROR_AXIS_LABEL_WIDTH: u16 = 3;
const CURVE_SAMPLES_PER_INTERVAL: u16 = 16;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum ColorProfile {
    TrueColor,
    Ansi256,
    Ansi16,
    None,
}

static COLOR_PROFILE: OnceLock<ColorProfile> = OnceLock::new();
static ICON_PROFILE: OnceLock<Icons> = OnceLock::new();

type ThemeColorKey = ((u8, u8, u8), (u8, u8, u8), u16, ColorProfile);

thread_local! {
    static THEME_COLOR_CACHE: RefCell<HashMap<ThemeColorKey, Color>> = RefCell::new(HashMap::new());
}

#[derive(Clone, Copy)]
struct Icons {
    teclado: &'static str,
    configuracoes: &'static str,
    tempo: &'static str,
    palavras: &'static str,
    citacao: &'static str,
    idioma: &'static str,
    dificuldade: &'static str,
    repeticao: &'static str,
    proximo: &'static str,
    estatisticas: &'static str,
    visao_geral: &'static str,
    progresso: &'static str,
    historico: &'static str,
    sair: &'static str,
    mouse: &'static str,
    favorito: &'static str,
    nao_favorito: &'static str,
    sucesso: &'static str,
    recorde: &'static str,
    falha: &'static str,
}

const ICONES_UNICODE: Icons = Icons {
    teclado: "⌨",
    configuracoes: "⚙",
    tempo: "◷",
    palavras: "Aa",
    citacao: "❝",
    idioma: "🌐",
    dificuldade: "★",
    repeticao: "↻",
    proximo: "›",
    estatisticas: "⌁",
    visao_geral: "▦",
    progresso: "↗",
    historico: "≡",
    sair: "×",
    mouse: "↖",
    favorito: "♥",
    nao_favorito: "♡",
    sucesso: "✓",
    recorde: "★",
    falha: "×",
};

const ICONES_NERD: Icons = Icons {
    teclado: "",
    configuracoes: "",
    tempo: "",
    palavras: "",
    citacao: "",
    idioma: "",
    dificuldade: "",
    repeticao: "",
    proximo: "",
    estatisticas: "",
    visao_geral: "",
    progresso: "",
    historico: "",
    sair: "",
    mouse: "",
    favorito: "",
    nao_favorito: "",
    sucesso: "",
    recorde: "",
    falha: "",
};

fn icones_do_terminal() -> Icons {
    *ICON_PROFILE.get_or_init(|| match env::var("TUIPE_ICONS").ok().as_deref() {
        Some("unicode") => ICONES_UNICODE,
        Some("nerd") => ICONES_NERD,
        // Os snapshots usam glifos Nerd de forma deliberada. A detecção real
        // depende do terminal que iniciou o processo e tornaria a suíte
        // diferente entre a máquina local e o CI.
        _ if cfg!(test) => ICONES_NERD,
        _ if active_terminal_uses_nerd_font() => ICONES_NERD,
        _ => ICONES_UNICODE,
    })
}

fn active_terminal_uses_nerd_font() -> bool {
    let tmux_kitty = tmux_client_uses_kitty();
    let kitty = env::var("KITTY_WINDOW_ID").is_ok_and(|window| !window.is_empty())
        || env::var("TERM").is_ok_and(|term| term.contains("kitty"))
        || env::var("TERM_PROGRAM").is_ok_and(|program| program.contains("kitty"))
        || tmux_kitty;
    if !kitty {
        return false;
    }
    let Ok(mut query) = Command::new("kitten")
        .args(["query_terminal", "--wait-for", "0.15", "font_family"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return tmux_kitty;
    };
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        match query.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return tmux_kitty;
                }
                let mut family = String::new();
                let detected = query
                    .stdout
                    .take()
                    .is_some_and(|mut stdout| stdout.read_to_string(&mut family).is_ok())
                    && is_nerd_font_family(&family);
                // O tmux pode bloquear a consulta de fonte mesmo quando o
                // cliente Kitty usa a fonte configurada pelo usuário. Nesse
                // ambiente preferimos os ícones Nerd; `TUIPE_ICONS=unicode`
                // continua sendo o fallback explícito para fontes sem glifos.
                return detected || tmux_kitty;
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = query.kill();
                let _ = query.wait();
                return tmux_kitty;
            }
            Err(_) => return tmux_kitty,
        }
    }
}

fn tmux_client_uses_kitty() -> bool {
    if env::var_os("TMUX").is_none() {
        return false;
    }
    Command::new("tmux")
        .args(["display-message", "-p", "#{client_termname}"])
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && terminal_name_is_kitty(&String::from_utf8_lossy(&output.stdout))
        })
}

fn terminal_name_is_kitty(name: &str) -> bool {
    name.to_ascii_lowercase().contains("kitty")
}

fn is_nerd_font_family(family: &str) -> bool {
    let family = family
        .split_once(':')
        .map_or(family, |(_, value)| value)
        .trim()
        .to_lowercase();
    let compact = family
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>();
    family.contains("nerd font")
        || compact.contains("nerdfont")
        || family
            .split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|part| !part.is_empty())
            .any(|part| {
                matches!(part, "nf" | "nfm" | "nfp")
                    || part.ends_with("nf")
                    || part.ends_with("nfm")
                    || part.ends_with("nfp")
            })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PersistenceUiState {
    #[default]
    Saved,
    Saving,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PersonalBest {
    pub previous_wpm: Option<f64>,
}

#[derive(Clone, Copy)]
struct RenderContext<'a> {
    settings_open: bool,
    settings_focus: usize,
    theme_name: &'a str,
    session_kind: SessionKind,
    persistence: PersistenceUiState,
    notice: Option<&'a str>,
    focus_warning: bool,
    quote: Option<QuoteRenderState<'a>>,
    keymap: &'a Keymap,
    icones: Icons,
    personal_best: Option<PersonalBest>,
    result_animation_ms: u64,
}

#[derive(Clone, Copy)]
struct FooterContext<'a> {
    persistence: PersistenceUiState,
    quote: Option<QuoteRenderState<'a>>,
    keymap: &'a Keymap,
    icones: Icons,
}

#[derive(Clone, Copy)]
struct ResultContext<'a> {
    session_kind: SessionKind,
    quote: Option<QuoteRenderState<'a>>,
    icones: Icons,
    personal_best: Option<PersonalBest>,
    animation_ms: u64,
}

#[derive(Clone, Copy)]
struct SettingsContext<'a> {
    theme_name: &'a str,
    keymap: &'a Keymap,
    focus: usize,
    icones: Icons,
}

#[derive(Clone, Copy)]
pub struct QuoteRenderState<'a> {
    pub source: &'a str,
    pub favorite: bool,
}

#[derive(Clone, Copy)]
pub struct RenderState<'a> {
    pub settings_open: bool,
    pub settings_focus: usize,
    pub theme_name: &'a str,
    pub session_kind: SessionKind,
    pub persistence: PersistenceUiState,
    pub notice: Option<&'a str>,
    pub focus_warning: bool,
    pub quote: Option<QuoteRenderState<'a>>,
    pub keymap: &'a Keymap,
    pub personal_best: Option<PersonalBest>,
    pub result_animation_ms: u64,
}

#[derive(Clone, Copy)]
pub struct StatisticsRenderState<'a> {
    pub page: StatisticsPage,
    pub selected_word: usize,
    pub selected_session: usize,
    pub history_filter: HistoryFilter,
    pub word_detail: Option<&'a WordDetail>,
    pub session_detail: Option<&'a SessionDetail>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatisticsPage {
    #[default]
    Overview,
    Progress,
    History,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryFilter {
    #[default]
    All,
    Completed,
    Failed,
}

#[derive(Clone, Copy)]
pub enum ResetConfirmation<'a> {
    Word(&'a str),
    Model,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsAction {
    Focus(usize),
    Punctuation(bool),
    Numbers(bool),
    ModeTime,
    ModeWords,
    ModeQuote,
    Value(usize),
    Difficulty(Difficulty),
    Adaptive(bool),
    LanguagePortuguese,
    LanguageEnglish,
    PackCommon,
    Pack1k,
    Pack5k,
    NextTheme,
    Close,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultAction {
    Next,
    Repeat,
    Statistics,
    Favorite,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatisticsAction {
    Page(StatisticsPage),
    Session(usize),
    FilterHistory,
    ResetModel,
    ResetWord,
    Back,
    ConfirmReset,
    CancelReset,
}

const STATISTICS_WIDE_MIN_WIDTH: u16 = 86;
const MAX_PAGE_CONTENT_WIDTH: u16 = 160;

pub fn render(frame: &mut Frame, engine: &TestEngine, theme: &Theme, state: RenderState<'_>) {
    render_com_icones(
        frame,
        engine,
        theme,
        RenderContext {
            settings_open: state.settings_open,
            settings_focus: state.settings_focus,
            theme_name: state.theme_name,
            session_kind: state.session_kind,
            persistence: state.persistence,
            notice: state.notice,
            focus_warning: state.focus_warning,
            quote: state.quote,
            keymap: state.keymap,
            icones: icones_do_terminal(),
            personal_best: state.personal_best,
            result_animation_ms: state.result_animation_ms,
        },
    );
}

/// Descobre capacidades que exigem conversar com o terminal antes de ativar o
/// modo bruto, evitando disputar as teclas do usuário durante a interface.
pub fn inicializar_capacidades_do_terminal() {
    let _ = icones_do_terminal();
}

pub fn render_statistics(
    frame: &mut Frame,
    statistics: &StatisticsOverview,
    state: StatisticsRenderState<'_>,
    theme: &Theme,
) {
    let viewport = frame.area();
    frame.render_widget(Clear, viewport);
    frame.render_widget(
        Block::default().style(Style::default().bg(color(&theme.bg))),
        viewport,
    );
    if viewport.width < 50 || viewport.height < 14 {
        render_size_requirement(frame, viewport, theme, 50, 14, "ver as estatísticas");
        return;
    }
    let content = statistics_content_area(viewport);
    if let Some(detail) = state.word_detail {
        render_word_detail(frame, content, detail, theme);
        return;
    }
    if let Some(detail) = state.session_detail {
        render_session_detail(frame, content, detail, theme);
        return;
    }
    if statistics.completed_tests == 0 && statistics.history.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    "estatísticas",
                    Style::default().fg(theme_color(theme, &theme.text, 4.5)),
                ),
                Line::from(""),
                Line::styled(
                    "ainda não há testes concluídos",
                    Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                ),
                Line::styled(
                    "volte e comece a digitar",
                    Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                ),
                Line::styled(
                    "o treino é escolhido automaticamente",
                    Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                ),
                Line::from(""),
                Line::styled(
                    "esc voltar",
                    Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                ),
            ])
            .alignment(Alignment::Center),
            centered_height(content, 7),
        );
        return;
    }
    let sections = Layout::vertical([
        Constraint::Length(statistics_navigation_height(content.width)),
        Constraint::Min(1),
    ])
    .split(content);
    render_statistics_navigation(frame, sections[0], statistics, state.page, theme);
    match state.page {
        StatisticsPage::Overview => render_statistics_overview(
            frame,
            sections[1],
            statistics,
            state.selected_word,
            content.width < STATISTICS_WIDE_MIN_WIDTH || viewport.height < 24,
            theme,
        ),
        StatisticsPage::Progress => {
            render_statistics_progress(frame, sections[1], statistics, theme)
        }
        StatisticsPage::History => render_statistics_history(
            frame,
            sections[1],
            &statistics.history,
            state.selected_session,
            state.history_filter,
            theme,
        ),
    }
}

fn statistics_content_area(viewport: Rect) -> Rect {
    centered_height(page_content(viewport), 30)
}

fn render_statistics_overview(
    frame: &mut Frame,
    content: Rect,
    statistics: &StatisticsOverview,
    selected_word: usize,
    compact: bool,
    theme: &Theme,
) {
    if compact {
        render_statistics_compact(frame, content, statistics, selected_word, theme);
        return;
    }
    let sections = Layout::vertical([
        Constraint::Length(11.min(content.height.saturating_sub(12))),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(7),
        Constraint::Length(3),
    ])
    .split(content);
    render_statistics_chart(frame, sections[0], &statistics.trend_tests, theme);
    render_statistics_summary(frame, sections[2], statistics, theme);
    if content.width < 120 {
        render_statistics_diagnostics_compact(frame, sections[3], statistics, selected_word, theme);
    } else {
        let details = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .spacing(1)
            .split(sections[3]);
        render_priority_words(
            frame,
            details[0],
            &statistics.priority_words,
            selected_word,
            theme,
        );
        render_priority_patterns(frame, details[1], &statistics.priority_patterns, theme);
    }
    render_statistics_controls(
        frame,
        sections[4],
        "↑↓ selecionar   enter detalhes   R zerar modelo   esc voltar",
        theme,
    );
}

fn render_statistics_diagnostics_compact(
    frame: &mut Frame,
    area: Rect,
    statistics: &StatisticsOverview,
    selected_word: usize,
    theme: &Theme,
) {
    let mut lines = vec![Line::styled(
        "palavras prioritárias",
        Style::default().fg(theme_color(theme, &theme.text, 4.5)),
    )];
    if let Some(word) = statistics.priority_words.get(selected_word) {
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "› {}  ·  prioridade {}  ·  ",
                    word.word,
                    priority_label(word.estimated_exposure_uplift)
                ),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
            Span::styled(
                format!(
                    "falhou {}  ·  corrigiu {}",
                    evidence_fraction(word.confirmed_errors, f64::from(word.observations)),
                    evidence_fraction(word.corrections, f64::from(word.observations))
                ),
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
        ]));
    } else {
        lines.push(Line::styled(
            "sem evidência suficiente",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        "padrões que pedem treino",
        Style::default().fg(theme_color(theme, &theme.text, 4.5)),
    ));
    if let Some(pattern) = statistics.priority_patterns.first() {
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{}  ·  prioridade {}  ·  ",
                    pattern.pattern,
                    priority_label(pattern.estimated_exposure_uplift)
                ),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
            Span::styled(
                format!(
                    "falhou {}  ·  corrigiu {}  ·  {} palavras",
                    evidence_fraction(
                        pattern.uncorrected_error_rate * pattern.effective_exposures,
                        pattern.effective_exposures
                    ),
                    evidence_fraction(
                        pattern.corrected_error_rate * pattern.effective_exposures,
                        pattern.effective_exposures
                    ),
                    pattern.distinct_words
                ),
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_statistics_navigation(
    frame: &mut Frame,
    area: Rect,
    statistics: &StatisticsOverview,
    active: StatisticsPage,
    theme: &Theme,
) {
    let compact = area.width < STATISTICS_WIDE_MIN_WIDTH;
    if !compact {
        let header = Layout::horizontal([Constraint::Min(1), Constraint::Length(30)])
            .split(Rect { height: 1, ..area });
        frame.render_widget(
            Paragraph::new("estatísticas").style(
                Style::default()
                    .fg(theme_color(theme, &theme.text, 4.5))
                    .add_modifier(Modifier::BOLD),
            ),
            header[0],
        );
        frame.render_widget(
            Paragraph::new(format!(
                "nível {}  ·  {} {}",
                statistics.level,
                statistics.streak,
                if statistics.streak == 1 {
                    "dia"
                } else {
                    "dias"
                }
            ))
            .alignment(Alignment::Right)
            .style(Style::default().fg(theme_color(theme, &theme.sub, 2.0))),
            header[1],
        );
    }

    let icones = icones_do_terminal();
    let labels = if compact {
        [
            (icones.visao_geral, "1 visão"),
            (icones.progresso, "2 progresso"),
            (icones.historico, "3 histórico"),
        ]
    } else {
        [
            (icones.visao_geral, "1  visão geral"),
            (icones.progresso, "2  progresso"),
            (icones.historico, "3  histórico"),
        ]
    };
    let active_index = match active {
        StatisticsPage::Overview => 0,
        StatisticsPage::Progress => 1,
        StatisticsPage::History => 2,
    };
    for (index, (tab, (icon, label))) in statistics_tab_areas(area)
        .into_iter()
        .zip(labels)
        .enumerate()
    {
        let selected = index == active_index;
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("{icon} "),
                    Style::default().fg(theme_color(
                        theme,
                        if selected { &theme.main } else { &theme.sub },
                        if selected { 3.0 } else { 2.0 },
                    )),
                ),
                Span::styled(
                    label,
                    Style::default()
                        .fg(theme_color(
                            theme,
                            if selected { &theme.main } else { &theme.sub },
                            if selected { 3.0 } else { 2.0 },
                        ))
                        .add_modifier(if selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
            ]))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme_color(
                        theme,
                        if selected {
                            &theme.main
                        } else {
                            &theme.sub_alt
                        },
                        if selected { 3.0 } else { 1.5 },
                    )))
                    .style(Style::default().bg(color(&theme.bg))),
            ),
            tab,
        );
    }
}

fn statistics_navigation_height(width: u16) -> u16 {
    if width < STATISTICS_WIDE_MIN_WIDTH {
        3
    } else {
        5
    }
}

fn statistics_tab_areas(area: Rect) -> Vec<Rect> {
    let tabs = if area.height >= 4 {
        Rect::new(area.x, area.y + 1, area.width, 3)
    } else {
        Rect::new(area.x, area.y, area.width, area.height.min(3))
    };
    Layout::horizontal([
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
    ])
    .spacing(1)
    .split(tabs)
    .to_vec()
}

fn render_statistics_progress(
    frame: &mut Frame,
    area: Rect,
    statistics: &StatisticsOverview,
    theme: &Theme,
) {
    if area.width < 80 || area.height < 20 {
        let sections = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(area);
        render_statistics_summary_compact(frame, sections[0], statistics, theme);
        render_wpm_distribution(frame, sections[1], &statistics.distribution, theme);
        render_activity_summary(frame, sections[2], &statistics.daily_activity, theme);
        render_statistics_controls(frame, sections[3], "tab navegar   esc voltar", theme);
        return;
    }
    let chart_height = if area.height >= 24 { 10 } else { 8 };
    let sections = Layout::vertical([
        Constraint::Length(chart_height),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(7),
        Constraint::Length(3),
    ])
    .split(area);
    render_statistics_chart(frame, sections[0], &statistics.trend_tests, theme);
    render_statistics_summary(frame, sections[2], statistics, theme);
    let lower = Layout::horizontal([Constraint::Percentage(48), Constraint::Percentage(52)])
        .spacing(4)
        .split(sections[3]);
    render_wpm_distribution(frame, lower[0], &statistics.distribution, theme);
    render_daily_activity(frame, lower[1], &statistics.daily_activity, theme);
    render_statistics_controls(frame, sections[4], "tab ou 1–3 navegar   esc voltar", theme);
}

fn render_statistics_controls(frame: &mut Frame, area: Rect, controls: &str, theme: &Theme) {
    let separated = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1),
    );
    frame.render_widget(
        Paragraph::new(controls)
            .style(Style::default().fg(theme_color(theme, &theme.sub, 2.0)))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(theme_color(theme, &theme.sub_alt, 1.5))),
            ),
        separated,
    );
}

fn render_activity_summary(frame: &mut Frame, area: Rect, days: &[ActivityDay], theme: &Theme) {
    let active_days = days.iter().filter(|day| day.tests > 0).count();
    let tests = days.iter().map(|day| day.tests).sum::<u32>();
    let active_ms = days.iter().map(|day| day.active_ms).sum::<u64>();
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                "atividade  ·  últimos 14 dias",
                Style::default().fg(theme_color(theme, &theme.text, 4.5)),
            ),
            Line::styled(
                format!(
                    "{active_days} dias ativos  ·  {tests} testes  ·  {}",
                    format_active_time(active_ms)
                ),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
        ]),
        area,
    );
}

fn render_statistics_summary_compact(
    frame: &mut Frame,
    area: Rect,
    statistics: &StatisticsOverview,
    theme: &Theme,
) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    format!("{:.0} wpm médio", statistics.average_wpm),
                    Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                ),
                Span::styled(
                    format!("  ·  {:.0}% precisão", statistics.average_accuracy),
                    Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                ),
            ]),
            Line::styled(
                format!(
                    "{} testes iguais  ·  melhor {:.0}  ·  {}",
                    statistics.comparable_tests,
                    statistics.best_wpm,
                    format_active_time(statistics.active_ms)
                ),
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
        ]),
        area,
    );
}

fn render_wpm_distribution(frame: &mut Frame, area: Rect, buckets: &[WpmBucket], theme: &Theme) {
    let total = buckets.iter().map(|bucket| bucket.count).sum::<u32>();
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "distribuição de wpm",
            Style::default().fg(theme_color(theme, &theme.text, 4.5)),
        ),
        Span::styled(
            format!("  ·  {total} testes"),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
    ])];
    if buckets.is_empty() {
        lines.push(Line::styled(
            "ainda sem testes na mesma configuração",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    } else {
        lines.push(Line::styled(
            format!("{:<9}{:>7}{:>5}  distribuição", "faixa", "testes", "%"),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
        let visible = area.height.saturating_sub(2) as usize;
        let start = buckets.len().saturating_sub(visible);
        let bar_width = area.width.saturating_sub(24).max(1) as usize;
        lines.extend(buckets[start..].iter().map(|bucket| {
            let share = if total == 0 {
                0.0
            } else {
                f64::from(bucket.count) / f64::from(total)
            };
            let filled = if bucket.count == 0 {
                0
            } else {
                (share * bar_width as f64).round().max(1.0) as usize
            };
            let mut spans = vec![
                Span::styled(
                    format!("{:>3}–{:<3}  ", bucket.start, bucket.end),
                    Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                ),
                Span::styled(
                    format!("{:>7}", bucket.count),
                    Style::default().fg(theme_color(theme, &theme.text, 4.5)),
                ),
                Span::styled(
                    format!("{:>5.0}%  ", share * 100.0),
                    Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                ),
            ];
            spans.extend(statistics_bar_track(filled, bar_width, theme));
            Line::from(spans)
        }));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_daily_activity(frame: &mut Frame, area: Rect, days: &[ActivityDay], theme: &Theme) {
    let visible = area.height.saturating_sub(2) as usize;
    let start = days.len().saturating_sub(visible);
    let visible_days = &days[start..];
    let maximum = visible_days
        .iter()
        .map(|day| day.active_ms)
        .max()
        .unwrap_or(1);
    let bar_width = area.width.saturating_sub(25).max(1) as usize;
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "atividade diária",
            Style::default().fg(theme_color(theme, &theme.text, 4.5)),
        ),
        Span::styled(
            format!("  ·  {} dias", visible_days.len()),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
    ])];
    lines.push(Line::styled(
        format!("{:<8}{:>7}{:>8}  atividade", "data", "min", "testes"),
        Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
    ));
    lines.extend(visible_days.iter().map(|day| {
        let filled = if maximum == 0 || day.active_ms == 0 {
            0
        } else {
            (day.active_ms as usize * bar_width).div_ceil(maximum as usize)
        };
        let minutes = day.active_ms as f64 / 60_000.0;
        let mut spans = vec![
            Span::styled(
                format!("{:<8}", day.date.format("%d/%m")),
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Span::styled(
                format!("{:>7.1}", minutes),
                Style::default().fg(theme_color(theme, &theme.text, 4.5)),
            ),
            Span::styled(
                format!("{:>8}  ", day.tests),
                Style::default().fg(theme_color(theme, &theme.text, 4.5)),
            ),
        ];
        spans.extend(statistics_bar_track(filled, bar_width, theme));
        Line::from(spans)
    }));
    frame.render_widget(Paragraph::new(lines), area);
}

fn statistics_bar_track(filled: usize, width: usize, theme: &Theme) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            "█".repeat(filled.min(width)),
            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
        ),
        Span::styled(
            "░".repeat(width.saturating_sub(filled)),
            Style::default().fg(theme_color(theme, &theme.sub_alt, 1.5)),
        ),
    ]
}

fn filtered_history(
    history: &[SessionHistoryItem],
    filter: HistoryFilter,
) -> Vec<&SessionHistoryItem> {
    history
        .iter()
        .filter(|session| match filter {
            HistoryFilter::All => true,
            HistoryFilter::Completed => session.outcome == SessionOutcome::Completed,
            HistoryFilter::Failed => session.outcome == SessionOutcome::Failed,
        })
        .collect()
}

fn render_statistics_history(
    frame: &mut Frame,
    area: Rect,
    history: &[SessionHistoryItem],
    selected: usize,
    filter: HistoryFilter,
    theme: &Theme,
) {
    let sessions = filtered_history(history, filter);
    let filter_label = match filter {
        HistoryFilter::All => "todos",
        HistoryFilter::Completed => "concluídos",
        HistoryFilter::Failed => "falhas",
    };
    let sections = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    "histórico de sessões",
                    Style::default().fg(theme_color(theme, &theme.text, 4.5)),
                ),
                Span::styled(
                    format!("  ·  filtro: {filter_label}"),
                    Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                ),
            ]),
            Line::styled(
                if area.width < 72 {
                    "sessão     resultado    wpm   acc   tempo"
                } else {
                    "sessão     quando          resultado       modo          wpm  precisão  duração"
                },
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
        ]),
        sections[0],
    );
    if sessions.is_empty() {
        frame.render_widget(
            Paragraph::new("nenhuma sessão neste filtro")
                .style(Style::default().fg(theme_color(theme, &theme.sub, 2.0))),
            sections[1],
        );
    } else {
        let visible = sections[1].height as usize;
        let selected = selected.min(sessions.len().saturating_sub(1));
        let offset = selected.saturating_sub(visible.saturating_sub(1));
        let lines = sessions
            .iter()
            .enumerate()
            .skip(offset)
            .take(visible)
            .map(|(index, session)| history_line(session, index == selected, area.width, theme))
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), sections[1]);
    }
    render_statistics_controls(
        frame,
        sections[2],
        if area.width < 72 {
            "↑↓ mover  enter abrir  f filtro  esc voltar"
        } else {
            "↑↓ selecionar   enter detalhes   f filtrar   tab navegar   esc voltar"
        },
        theme,
    );
}

fn history_line(
    session: &SessionHistoryItem,
    selected: bool,
    width: u16,
    theme: &Theme,
) -> Line<'static> {
    let result = match session.outcome {
        SessionOutcome::Completed => "concluído",
        SessionOutcome::Failed => "falhou",
    };
    let result_color = if session.outcome == SessionOutcome::Completed {
        &theme.main
    } else {
        &theme.error
    };
    let prefix = if selected { "›" } else { " " };
    let duration = format_session_duration(session.elapsed_ms);
    if width < 72 {
        return Line::from(vec![
            Span::styled(
                format!("{prefix} #{:<7}", session.id),
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Span::styled(
                format!("{result:<13}"),
                Style::default().fg(theme_color(theme, result_color, 3.0)),
            ),
            Span::styled(
                format!(
                    "{:>3.0}  {:>4.0}%  {duration:>6}",
                    session.wpm, session.accuracy
                ),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
        ]);
    }
    Line::from(vec![
        Span::styled(
            format!("{prefix} #{:<7}", session.id),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
        Span::styled(
            format!("{:<15}", format_session_date(session.created_at_unix_s)),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
        Span::styled(
            format!("{result:<16}"),
            Style::default().fg(theme_color(theme, result_color, 3.0)),
        ),
        Span::styled(
            format!("{:<14}", test_mode_label(&session.config.mode)),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
        Span::styled(
            format!(
                "{:>3.0}  {:>7.0}%  {duration:>7}",
                session.wpm, session.accuracy
            ),
            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
        ),
    ])
}

fn render_session_detail(frame: &mut Frame, area: Rect, detail: &SessionDetail, theme: &Theme) {
    let compact = area.width < 80 || area.height < 22;
    let session = &detail.session;
    let outcome = match session.outcome {
        SessionOutcome::Completed => "concluído",
        SessionOutcome::Failed => "falhou",
    };
    let outcome_color = if session.outcome == SessionOutcome::Completed {
        &theme.main
    } else {
        &theme.error
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("sessão #{}  ·  ", session.id),
                Style::default()
                    .fg(theme_color(theme, &theme.text, 4.5))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                outcome,
                Style::default().fg(theme_color(theme, outcome_color, 3.0)),
            ),
            Span::styled(
                format!("  ·  {}", format_session_date(session.created_at_unix_s)),
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            metric_span("wpm", format!("{:.0}", session.wpm), theme),
            Span::raw("    "),
            metric_span("precisão", format!("{:.0}%", session.accuracy), theme),
            Span::raw("    "),
            metric_span("bruto", format!("{:.0}", session.raw_wpm), theme),
            Span::raw("    "),
            metric_span("tempo", format_session_duration(session.elapsed_ms), theme),
        ]),
        Line::styled(
            format!(
                "{}  ·  {}  ·  {}  ·  {}",
                test_mode_label(&session.config.mode),
                language_name(&session.config.language),
                difficulty_name(session.config.difficulty),
                session_kind_name(session.kind)
            ),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
        Line::from(""),
        Line::styled(
            "sinais desta sessão",
            Style::default().fg(theme_color(theme, &theme.text, 4.5)),
        ),
        Line::styled(
            format!(
                "{} palavras observadas  ·  {} limpas  ·  {} corrigidas  ·  {} falhas  ·  {} lentas",
                detail.observed_words,
                detail.clean_words,
                detail.corrected_words,
                detail.failed_words,
                detail.slow_words
            ),
            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
        ),
    ];
    if !detail.challenges.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            "pontos que mais exigiram atenção",
            Style::default().fg(theme_color(theme, &theme.text, 4.5)),
        ));
        let limit = if compact { 4 } else { 8 };
        lines.extend(detail.challenges.iter().take(limit).map(|challenge| {
            let signal = if challenge.confirmed_error {
                "falha"
            } else if challenge.corrected {
                "corrigida"
            } else {
                "ritmo lento"
            };
            let ratio = challenge
                .latency_ratio
                .filter(|ratio| ratio.is_finite())
                .map_or_else(String::new, |ratio| format!("  ·  {ratio:.1}× seu ritmo"));
            Line::from(vec![
                Span::styled(
                    format!("{:<16}", challenge.word),
                    Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                ),
                Span::styled(
                    format!("{signal}{ratio}"),
                    Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                ),
            ])
        }));
    }
    if !compact && !detail.stimuli.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            "texto praticado",
            Style::default().fg(theme_color(theme, &theme.text, 4.5)),
        ));
        lines.push(Line::styled(
            quote_source_label(&detail.stimuli.join(" "), area.width as usize * 2),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    }
    let body_height = area.height.saturating_sub(3);
    while lines.len() < body_height as usize {
        lines.push(Line::from(""));
    }
    lines.truncate(body_height as usize);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        Rect::new(area.x, area.y, area.width, body_height),
    );
    render_statistics_controls(
        frame,
        Rect::new(area.x, area.bottom().saturating_sub(3), area.width, 3),
        "enter ou esc voltar",
        theme,
    );
}

fn metric_span(label: &str, value: String, theme: &Theme) -> Span<'static> {
    Span::styled(
        format!("{label} {value}"),
        Style::default().fg(theme_color(theme, &theme.main, 3.0)),
    )
}

fn test_mode_label(mode: &TestMode) -> String {
    match mode {
        TestMode::Time { seconds } => format!("{seconds} segundos"),
        TestMode::Words { count } => format!("{count} palavras"),
        TestMode::Quote => "citação".into(),
    }
}

fn session_kind_name(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Practice => "treino",
        SessionKind::Assessment => "avaliação",
        SessionKind::Transfer => "transferência",
        SessionKind::Retention => "retenção",
        SessionKind::Repeat => "repetição",
    }
}

fn format_session_date(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%d/%m %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "data inválida".into())
}

fn format_session_duration(milliseconds: u64) -> String {
    if milliseconds >= 60_000 {
        format!(
            "{}m{:02}s",
            milliseconds / 60_000,
            milliseconds / 1_000 % 60
        )
    } else {
        format!("{:.1}s", milliseconds as f64 / 1_000.0)
    }
}

pub fn render_reset_confirmation(
    frame: &mut Frame,
    confirmation: ResetConfirmation<'_>,
    theme: &Theme,
) {
    let viewport = frame.area();
    for y in viewport.y..viewport.bottom() {
        for x in viewport.x..viewport.right() {
            let style = frame.buffer_mut()[(x, y)].style();
            frame.buffer_mut()[(x, y)].set_style(style.add_modifier(Modifier::DIM));
        }
    }
    let area = centered_width(centered_height(viewport, 7), 58);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme_color(theme, &theme.error, 3.0)))
            .style(Style::default().bg(color(&theme.bg))),
        area,
    );
    let (title, target) = match confirmation {
        ResetConfirmation::Word(word) => ("zerar aprendizado da palavra?", word),
        ResetConfirmation::Model => ("zerar todo o aprendizado adaptativo?", "modelo inteiro"),
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                title,
                Style::default()
                    .fg(theme_color(theme, &theme.text, 4.5))
                    .add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                target.to_owned(),
                Style::default().fg(theme_color(theme, &theme.error, 3.0)),
            ),
            Line::styled(
                "sessões, métricas, XP e streak serão preservados",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Line::from(""),
            key_hints(&[("s", "confirmar"), ("n", "cancelar")], theme),
        ])
        .alignment(Alignment::Center),
        Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(1),
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        ),
    );
}

pub fn statistics_word_at(
    viewport: Rect,
    statistics: &StatisticsOverview,
    selected_word: usize,
    position: Position,
) -> Option<usize> {
    if viewport.width < 50 || viewport.height < 14 || statistics.priority_words.is_empty() {
        return None;
    }
    let content = statistics_content_area(viewport);
    let navigation_height = statistics_navigation_height(content.width);
    let content = Rect::new(
        content.x,
        content.y.saturating_add(navigation_height),
        content.width,
        content.height.saturating_sub(navigation_height),
    );
    if content.width < STATISTICS_WIDE_MIN_WIDTH || viewport.height < 24 {
        let first_row = content.y.saturating_add(6);
        let visible = statistics
            .priority_words
            .len()
            .min(compact_diagnostic_limit(content.height));
        let offset = selected_word.saturating_sub(visible.saturating_sub(1));
        let index = offset + usize::from(position.y.saturating_sub(first_row));
        return (position.y >= first_row
            && position.x >= content.x
            && position.x < content.right()
            && index < statistics.priority_words.len().min(offset + visible))
        .then_some(index);
    }
    let sections = Layout::vertical([
        Constraint::Length(11.min(content.height.saturating_sub(12))),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(7),
        Constraint::Length(3),
    ])
    .split(content);
    let details = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
        .spacing(1)
        .split(sections[3]);
    let first_row = details[0].y.saturating_add(3);
    let visible = details[0].height.saturating_sub(3) as usize;
    let offset = selected_word.saturating_sub(visible.saturating_sub(1));
    let index = offset + usize::from(position.y.saturating_sub(first_row));
    (position.y >= first_row
        && position.x >= details[0].x
        && position.x < details[0].right()
        && index < statistics.priority_words.len().min(offset + visible))
    .then_some(index)
}

pub fn statistics_action_at(
    viewport: Rect,
    statistics: &StatisticsOverview,
    page: StatisticsPage,
    selected_session: usize,
    filter: HistoryFilter,
    position: Position,
) -> Option<StatisticsAction> {
    if viewport.width < 50 || viewport.height < 14 {
        return None;
    }
    let content = statistics_content_area(viewport);
    for (tab, target) in statistics_tab_areas(Rect::new(
        content.x,
        content.y,
        content.width,
        statistics_navigation_height(content.width),
    ))
    .into_iter()
    .zip([
        StatisticsPage::Overview,
        StatisticsPage::Progress,
        StatisticsPage::History,
    ]) {
        if tab.contains(position) {
            return Some(StatisticsAction::Page(target));
        }
    }
    if position.y == content.bottom().saturating_sub(1) {
        let line = match page {
            StatisticsPage::Overview if content.width < 64 => {
                "↑↓ mover  enter detalhes  R zerar  esc voltar"
            }
            StatisticsPage::Overview => {
                "↑↓ selecionar   enter detalhes   R zerar modelo   esc voltar"
            }
            StatisticsPage::Progress if content.width < 80 || content.height < 20 => {
                "tab navegar   esc voltar"
            }
            StatisticsPage::Progress => "tab ou 1–3 navegar   esc voltar",
            StatisticsPage::History if content.width < 72 => {
                "↑↓ mover  enter abrir  f filtro  esc voltar"
            }
            StatisticsPage::History => {
                "↑↓ selecionar   enter detalhes   f filtrar   tab navegar   esc voltar"
            }
        };
        let relative_x = usize::from(position.x.saturating_sub(content.x));
        if label_hit(line, "esc voltar", relative_x) {
            return Some(StatisticsAction::Back);
        }
        if page == StatisticsPage::Overview && label_hit(line, "R zerar", relative_x) {
            return Some(StatisticsAction::ResetModel);
        }
        if page == StatisticsPage::History && label_hit(line, "f filtr", relative_x) {
            return Some(StatisticsAction::FilterHistory);
        }
    }
    if page != StatisticsPage::History {
        return None;
    }
    let sessions = filtered_history(&statistics.history, filter);
    let navigation_height = statistics_navigation_height(content.width);
    let body = Rect::new(
        content.x,
        content.y.saturating_add(navigation_height),
        content.width,
        content.height.saturating_sub(navigation_height),
    );
    let first_row = body.y.saturating_add(2);
    if position.y < first_row || position.y >= body.bottom().saturating_sub(1) {
        return None;
    }
    let visible = body.height.saturating_sub(3) as usize;
    let selected = selected_session.min(sessions.len().saturating_sub(1));
    let offset = selected.saturating_sub(visible.saturating_sub(1));
    let index = offset + usize::from(position.y - first_row);
    (position.x >= body.x && position.x < body.right() && index < sessions.len())
        .then_some(StatisticsAction::Session(index))
}

pub fn statistics_detail_action_at(
    viewport: Rect,
    word_detail: bool,
    session_detail: bool,
    position: Position,
) -> Option<StatisticsAction> {
    let content = statistics_content_area(viewport);
    if position.y != content.bottom().saturating_sub(1) {
        return None;
    }
    let relative_x = usize::from(position.x.saturating_sub(content.x));
    if word_detail {
        let line = "r zerar palavra   enter ou esc voltar";
        if label_hit(line, "r zerar palavra", relative_x) {
            return Some(StatisticsAction::ResetWord);
        }
        if label_hit(line, "enter ou esc voltar", relative_x) {
            return Some(StatisticsAction::Back);
        }
    } else if session_detail && label_hit("enter ou esc voltar", "enter ou esc voltar", relative_x)
    {
        return Some(StatisticsAction::Back);
    }
    None
}

pub fn reset_confirmation_action_at(
    viewport: Rect,
    position: Position,
) -> Option<StatisticsAction> {
    let area = centered_width(centered_height(viewport, 7), 58);
    let controls = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(5),
        area.width.saturating_sub(2),
        1,
    );
    if position.y != controls.y || !controls.contains(position) {
        return None;
    }
    centered_text_hit(
        controls,
        position.x,
        &[
            (StatisticsAction::ConfirmReset, "s confirmar".into()),
            (StatisticsAction::CancelReset, "n cancelar".into()),
        ],
        4,
    )
}

fn label_hit(line: &str, label: &str, x: usize) -> bool {
    line.find(label).is_some_and(|byte_start| {
        let start = line[..byte_start].width();
        (start..start + label.width()).contains(&x)
    })
}

fn render_word_detail(frame: &mut Frame, area: Rect, detail: &WordDetail, theme: &Theme) {
    let priority = &detail.priority;
    let compact = area.width < 80 || area.height < 22;
    if compact {
        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    "palavra  ",
                    Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                ),
                Span::styled(
                    priority.word.clone(),
                    Style::default()
                        .fg(theme_color(theme, &theme.text, 4.5))
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::styled(
                format!(
                    "prioridade: {}",
                    priority_label(priority.estimated_exposure_uplift)
                ),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
            Line::styled(
                format!(
                    "falhou {}  ·  corrigiu {}  ·  {:.0} caracteres apagados",
                    evidence_fraction(priority.confirmed_errors, f64::from(priority.observations)),
                    evidence_fraction(priority.corrections, f64::from(priority.observations)),
                    priority.corrected_graphemes
                ),
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            word_speed_line(detail, theme),
            word_trend_line(detail, theme),
            Line::styled(
                format!(
                    "última prática {}",
                    format_last_seen(detail.last_seen_unix_s)
                ),
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Line::from(""),
            Line::styled(
                "tentativas recentes",
                Style::default().fg(theme_color(theme, &theme.text, 4.5)),
            ),
        ];
        lines.extend(
            detail
                .recent_attempts
                .iter()
                .take(area.height.saturating_sub(10) as usize)
                .map(|attempt| word_attempt_line(attempt, theme)),
        );
        let body_height = area.height.saturating_sub(3);
        while lines.len() < body_height as usize {
            lines.push(Line::from(""));
        }
        lines.truncate(body_height as usize);
        frame.render_widget(
            Paragraph::new(lines),
            Rect::new(area.x, area.y, area.width, body_height),
        );
        render_statistics_controls(
            frame,
            Rect::new(area.x, area.bottom().saturating_sub(3), area.width, 3),
            "r zerar palavra   enter ou esc voltar",
            theme,
        );
        return;
    }

    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(8),
        Constraint::Length(3),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "detalhes da palavra  ·  ",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Span::styled(
                priority.word.clone(),
                Style::default()
                    .fg(theme_color(theme, &theme.text, 4.5))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  ·  {}", language_name(&priority.language)),
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
        ])),
        sections[0],
    );
    frame.render_widget(
        Paragraph::new(format!(
            "prioridade no treino: {}",
            priority_label(priority.estimated_exposure_uplift)
        ))
        .style(Style::default().fg(theme_color(theme, &theme.main, 3.0))),
        sections[1],
    );
    let metrics = [
        (
            "falhou ao confirmar",
            evidence_fraction(priority.confirmed_errors, f64::from(priority.observations)),
        ),
        (
            "precisou corrigir",
            evidence_fraction(priority.corrections, f64::from(priority.observations)),
        ),
        (
            "caracteres apagados",
            format!("{:.0}", priority.corrected_graphemes),
        ),
        (
            "tempo em correções",
            format_duration_ms(priority.correction_ms),
        ),
    ];
    for (metric_area, (label, value)) in Layout::horizontal([
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
    ])
    .spacing(2)
    .split(sections[2])
    .iter()
    .zip(metrics)
    {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    label,
                    Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                ),
                Line::styled(
                    value,
                    Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                ),
            ]),
            *metric_area,
        );
    }
    let body = Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)])
        .spacing(4)
        .split(sections[4]);
    let mut diagnosis = vec![
        Line::styled(
            "diagnóstico",
            Style::default().fg(theme_color(theme, &theme.text, 4.5)),
        ),
        word_speed_line(detail, theme),
        word_trend_line(detail, theme),
        Line::styled(
            format!(
                "última prática {}",
                format_last_seen(detail.last_seen_unix_s)
            ),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
        Line::from(""),
        Line::styled(
            "sequências relacionadas",
            Style::default().fg(theme_color(theme, &theme.text, 4.5)),
        ),
    ];
    diagnosis.push(Line::styled(
        if detail.relevant_sequences.is_empty() {
            "nenhuma com evidência independente".into()
        } else {
            detail.relevant_sequences.join("  ·  ")
        },
        Style::default().fg(theme_color(theme, &theme.main, 3.0)),
    ));
    frame.render_widget(Paragraph::new(diagnosis), body[0]);

    let mut recent = vec![Line::styled(
        "tentativas recentes",
        Style::default().fg(theme_color(theme, &theme.text, 4.5)),
    )];
    recent.extend(
        detail
            .recent_attempts
            .iter()
            .take(body[1].height.saturating_sub(1) as usize)
            .map(|attempt| word_attempt_line(attempt, theme)),
    );
    frame.render_widget(Paragraph::new(recent), body[1]);
    render_statistics_controls(
        frame,
        sections[5],
        "r zerar palavra   enter ou esc voltar",
        theme,
    );
}

fn priority_label(increase: f64) -> String {
    format!("+{:.0}%", increase.clamp(0.0, 1.0) * 100.0)
}

fn evidence_fraction(signal: f64, exposures: f64) -> String {
    format!("{:.0}/{:.0}", signal.max(0.0), exposures.max(0.0))
}

fn format_duration_ms(milliseconds: f64) -> String {
    if milliseconds >= 1_000.0 {
        format!("{:.1} s", milliseconds / 1_000.0)
    } else {
        format!("{milliseconds:.0} ms")
    }
}

fn word_speed_line(detail: &WordDetail, theme: &Theme) -> Line<'static> {
    let text = match (
        detail.median_ms_per_grapheme,
        detail.personal_baseline_ms_per_grapheme,
    ) {
        (Some(word), Some(baseline)) if baseline > 0.0 => {
            let difference = (word / baseline - 1.0) * 100.0;
            let comparison = if difference.abs() < 5.0 {
                "dentro da base".into()
            } else {
                format!("{difference:+.0}% vs base")
            };
            format!("ritmo {word:.0} ms/caractere  ·  {comparison}")
        }
        (Some(word), None) => format!("ritmo {word:.0} ms/caractere  ·  base em formação"),
        _ => "ritmo ainda sem amostras suficientes".into(),
    };
    Line::styled(
        text,
        Style::default().fg(theme_color(theme, &theme.main, 3.0)),
    )
}

fn word_trend_line(detail: &WordDetail, theme: &Theme) -> Line<'static> {
    let trend = word_trend(detail);
    Line::styled(
        format!("tendência {trend}"),
        Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
    )
}

fn word_trend(detail: &WordDetail) -> &'static str {
    if detail.recent_attempts.len() < 4 {
        return "ainda incerta";
    }
    let middle = detail.recent_attempts.len() / 2;
    let error_score = |attempts: &[crate::persistence::WordAttemptSummary]| {
        attempts
            .iter()
            .map(|attempt| {
                if attempt.confirmed_error {
                    1.0
                } else if attempt.corrected {
                    0.45
                } else {
                    0.0
                }
            })
            .sum::<f64>()
            / attempts.len() as f64
    };
    let recent = error_score(&detail.recent_attempts[..middle]);
    let older = error_score(&detail.recent_attempts[middle..]);
    if recent + 0.15 < older {
        "melhorando"
    } else if recent > older + 0.15 {
        "piorando"
    } else {
        "estável"
    }
}

fn word_attempt_line(
    attempt: &crate::persistence::WordAttemptSummary,
    theme: &Theme,
) -> Line<'static> {
    let (status, value) = if attempt.confirmed_error && attempt.corrected {
        (
            format!("corrigiu {} e falhou", attempt.corrections),
            &theme.error,
        )
    } else if attempt.confirmed_error {
        ("falhou".into(), &theme.error)
    } else if attempt.corrected {
        (format!("corrigiu {}", attempt.corrections), &theme.sub)
    } else {
        ("limpa".into(), &theme.main)
    };
    let detail = if attempt.corrections > 0 {
        format!(" · {}", format_duration_ms(attempt.correction_ms as f64))
    } else {
        attempt.milliseconds_per_grapheme.map_or_else(
            || " · sem ritmo".into(),
            |milliseconds| format!(" · {milliseconds:.0} ms/caractere"),
        )
    };
    Line::from(vec![
        Span::styled(
            format!("#{:<6}", attempt.session_id),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
        Span::styled(status, Style::default().fg(theme_color(theme, value, 3.0))),
        Span::styled(
            detail,
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
    ])
}

fn format_last_seen(timestamp: Option<i64>) -> String {
    let Some(timestamp) = timestamp else {
        return "sem data disponível".into();
    };
    let seconds = chrono::Utc::now().timestamp().saturating_sub(timestamp);
    match seconds {
        ..=59 => "agora".into(),
        60..=3_599 => format!("há {} min", seconds / 60),
        3_600..=86_399 => format!("há {} h", seconds / 3_600),
        _ => format!("há {} dias", seconds / 86_400),
    }
}

fn render_statistics_compact(
    frame: &mut Frame,
    area: Rect,
    statistics: &StatisticsOverview,
    selected_word: usize,
    theme: &Theme,
) {
    let mut lines = vec![
        Line::styled(
            "estatísticas",
            Style::default().fg(theme_color(theme, &theme.text, 4.5)),
        ),
        Line::from(vec![
            Span::styled(
                format!("{} testes totais", statistics.completed_tests),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
            Span::styled(
                "  ·  ",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Span::styled(
                format!("{} testes iguais", statistics.comparable_tests),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                format!("{:.0} wpm", statistics.average_wpm),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
            Span::styled(
                "  ·  ",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Span::styled(
                format!("{:.0}% precisão", statistics.average_accuracy),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
            Span::styled(
                "  ·  ",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Span::styled(
                format!("melhor {:.0}", statistics.best_wpm),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
        ]),
        compact_trend(&statistics.trend_tests, theme),
        Line::from(""),
        Line::styled(
            "palavras prioritárias",
            Style::default().fg(theme_color(theme, &theme.text, 4.5)),
        ),
    ];

    if statistics.priority_words.is_empty() {
        lines.push(Line::styled(
            "sem evidência suficiente",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    } else {
        let visible = statistics
            .priority_words
            .len()
            .min(compact_diagnostic_limit(area.height));
        let offset = selected_word.saturating_sub(visible.saturating_sub(1));
        lines.extend(
            statistics
                .priority_words
                .iter()
                .enumerate()
                .skip(offset)
                .take(visible)
                .map(|(index, word)| {
                    let priority = if area.width < 60 {
                        format!("  {}  ", priority_label(word.estimated_exposure_uplift))
                    } else {
                        format!(
                            "  ·  prioridade {}  ·  ",
                            priority_label(word.estimated_exposure_uplift)
                        )
                    };
                    Line::from(vec![
                        Span::styled(
                            if index == selected_word { "› " } else { "  " },
                            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                        ),
                        Span::styled(
                            word.word.clone(),
                            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                        ),
                        Span::styled(
                            priority,
                            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                        ),
                        Span::styled(
                            format!(
                                "falhou {} · corrigiu {}",
                                evidence_fraction(
                                    word.confirmed_errors,
                                    f64::from(word.observations)
                                ),
                                evidence_fraction(word.corrections, f64::from(word.observations))
                            ),
                            Style::default().fg(theme_color(theme, &theme.error, 3.0)),
                        ),
                    ])
                }),
        );
    }

    lines.push(Line::from(""));
    lines.push(Line::styled(
        "padrões que pedem treino",
        Style::default().fg(theme_color(theme, &theme.text, 4.5)),
    ));
    if statistics.priority_patterns.is_empty() {
        lines.push(Line::styled(
            "sem evidência em palavras distintas",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    } else {
        lines.extend(
            statistics
                .priority_patterns
                .iter()
                .take(compact_diagnostic_limit(area.height))
                .map(|pattern| {
                    let kind = (pattern.kind == "mecânica").then_some("técnica");
                    let label = if area.width < 60 {
                        kind.map_or_else(
                            || pattern.pattern.clone(),
                            |kind| format!("{kind} {}", pattern.pattern),
                        )
                    } else {
                        kind.map_or_else(
                            || pattern.pattern.clone(),
                            |kind| format!("{kind} {}", pattern.pattern),
                        )
                    };
                    let contexts = if area.width < 60 {
                        format!(
                            "  prioridade {}  ·  ",
                            priority_label(pattern.estimated_exposure_uplift)
                        )
                    } else {
                        format!(
                            "  ·  prioridade {}  ·  ",
                            priority_label(pattern.estimated_exposure_uplift)
                        )
                    };
                    Line::from(vec![
                        Span::styled(
                            label,
                            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                        ),
                        Span::styled(
                            contexts,
                            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                        ),
                        Span::styled(
                            format!(
                                "falhou {}",
                                evidence_fraction(
                                    pattern.uncorrected_error_rate * pattern.effective_exposures,
                                    pattern.effective_exposures
                                )
                            ),
                            Style::default().fg(theme_color(theme, &theme.error, 3.0)),
                        ),
                        Span::styled(
                            format!(
                                "  ·  corrigiu {}",
                                evidence_fraction(
                                    pattern.corrected_error_rate * pattern.effective_exposures,
                                    pattern.effective_exposures
                                )
                            ),
                            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                        ),
                        Span::styled(
                            format!("  ·  {} palavras", pattern.distinct_words),
                            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                        ),
                    ])
                }),
        );
    }

    let body_height = area.height.saturating_sub(3);
    while lines.len() < body_height as usize {
        lines.push(Line::from(""));
    }
    lines.truncate(body_height as usize);
    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(area.x, area.y, area.width, body_height),
    );
    render_statistics_controls(
        frame,
        Rect::new(area.x, area.bottom().saturating_sub(3), area.width, 3),
        if area.width < 64 {
            "↑↓ mover  enter detalhes  R zerar  esc voltar"
        } else {
            "↑↓ selecionar   enter detalhes   R zerar modelo   esc voltar"
        },
        theme,
    );
}

fn compact_trend(sessions: &[SessionSummary], theme: &Theme) -> Line<'static> {
    if sessions.len() < 4 {
        return Line::styled(
            "tendência disponível após 4 testes válidos",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        );
    }
    let middle = sessions.len() / 2;
    let first = median_wpm(&sessions[..middle]);
    let last = median_wpm(&sessions[middle..]);
    Line::from(vec![
        Span::styled(
            format!("{} testes válidos", sessions.len()),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
        Span::styled(
            "  ·  ",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
        Span::styled(
            format!("{first:.0} → {last:.0} wpm típico"),
            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
        ),
    ])
}

fn median_wpm(sessions: &[SessionSummary]) -> f64 {
    let mut values = sessions
        .iter()
        .map(|session| session.wpm)
        .collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn compact_diagnostic_limit(height: u16) -> usize {
    if height >= 20 { 3 } else { 1 }
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
            Span::styled(
                "tendência de wpm",
                Style::default().fg(theme_color(theme, &theme.text, 4.5)),
            ),
            Span::styled(
                format!("  ·  {} testes válidos", sessions.len()),
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
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
    let smoothed = smooth_session_wpm(&points);
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
                    .border_style(Style::default().fg(theme_color(theme, &theme.main, 3.0))),
            )
            .x_bounds([
                points.first().map_or(0.0, |point| point.0),
                points.last().map_or(1.0, |point| point.0).max(1.0),
            ])
            .y_bounds([0.0, ceiling])
            .paint(|context| {
                context.draw(&Points {
                    coords: &points,
                    color: theme_color(theme, &theme.sub, 2.0),
                });
                for segment in smoothed.windows(2) {
                    context.draw(&CanvasLine {
                        x1: segment[0].0,
                        y1: segment[0].1,
                        x2: segment[1].0,
                        y2: segment[1].1,
                        color: theme_color(theme, &theme.main, 3.0),
                    });
                }
            }),
        canvas_area,
    );
    if !sessions.is_empty() {
        let tick_count = chart_tick_count(columns[1].width, sessions.len());
        let labels = (0..tick_count)
            .map(|index| {
                let session_index = if tick_count == 1 {
                    0
                } else {
                    index * (sessions.len() - 1) / (tick_count - 1)
                };
                format!("#{}", sessions[session_index].id)
            })
            .collect::<Vec<_>>();
        render_chart_tick_labels(
            frame,
            Rect::new(columns[1].x, canvas_area.bottom(), columns[1].width, 1),
            &labels,
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        );
    }
}

fn smooth_session_wpm(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let Some(&(first_x, first_wpm)) = points.first() else {
        return Vec::new();
    };
    let window = points.len().clamp(2, 9) as f64;
    let alpha = 2.0 / (window + 1.0);
    let mut current = first_wpm;
    let mut smoothed = vec![(first_x, current)];
    for &(x, wpm) in &points[1..] {
        current += alpha * (wpm - current);
        smoothed.push((x, current));
    }
    smoothed
}

fn render_statistics_summary(
    frame: &mut Frame,
    area: Rect,
    statistics: &StatisticsOverview,
    theme: &Theme,
) {
    let values = [
        ("testes totais", statistics.completed_tests.to_string()),
        ("testes válidos", statistics.comparable_tests.to_string()),
        ("wpm médio", format!("{:.0}", statistics.average_wpm)),
        ("precisão", format!("{:.0}%", statistics.average_accuracy)),
        ("melhor", format!("{:.0}", statistics.best_wpm)),
        ("tempo ativo", format_active_time(statistics.active_ms)),
    ];
    for (area, (label, value)) in Layout::horizontal(vec![Constraint::Ratio(1, 6); 6])
        .spacing(2)
        .split(area)
        .iter()
        .zip(values)
    {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    label,
                    Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                ),
                Line::styled(
                    value,
                    Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                ),
            ]),
            *area,
        );
    }
}

fn format_active_time(milliseconds: u64) -> String {
    let minutes = milliseconds / 60_000;
    let hours = minutes / 60;
    if hours > 0 {
        format!("{hours}h {:02}m", minutes % 60)
    } else {
        format!("{minutes}m")
    }
}

fn render_priority_words(
    frame: &mut Frame,
    area: Rect,
    words: &[PriorityWord],
    selected_word: usize,
    theme: &Theme,
) {
    let mut lines = vec![Line::styled(
        "palavras prioritárias",
        Style::default().fg(theme_color(theme, &theme.text, 4.5)),
    )];
    lines.push(Line::from(""));
    if words.is_empty() {
        lines.push(Line::styled(
            "sem evidência suficiente",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    } else {
        lines.push(Line::styled(
            "palavra       prioridade  falhou  corrigiu  apagou",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
        let visible = area.height.saturating_sub(3) as usize;
        let offset = selected_word.saturating_sub(visible.saturating_sub(1));
        lines.extend(
            words
                .iter()
                .enumerate()
                .skip(offset)
                .take(visible)
                .map(|(index, word)| {
                    Line::from(vec![
                        Span::styled(
                            if index == selected_word { "›" } else { " " },
                            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                        ),
                        Span::styled(
                            format!("{:<14}", word.word),
                            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                        ),
                        Span::styled(
                            format!("{:>10}  ", priority_label(word.estimated_exposure_uplift)),
                            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                        ),
                        Span::styled(
                            format!(
                                "{:>6}  ",
                                evidence_fraction(
                                    word.confirmed_errors,
                                    f64::from(word.observations)
                                )
                            ),
                            Style::default().fg(theme_color(theme, &theme.error, 3.0)),
                        ),
                        Span::styled(
                            format!(
                                "{:>7}  ",
                                evidence_fraction(word.corrections, f64::from(word.observations))
                            ),
                            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                        ),
                        Span::styled(
                            format!("{:>5.0}", word.corrected_graphemes),
                            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                        ),
                    ])
                }),
        );
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_priority_patterns(
    frame: &mut Frame,
    area: Rect,
    patterns: &[PriorityPattern],
    theme: &Theme,
) {
    let mut lines = vec![Line::styled(
        "padrões que pedem treino",
        Style::default().fg(theme_color(theme, &theme.text, 4.5)),
    )];
    lines.push(Line::from(""));
    if patterns.is_empty() {
        lines.push(Line::styled(
            "sem evidência em palavras distintas",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    } else {
        let label_width = if area.width >= 62 { 20 } else { 12 };
        lines.push(Line::styled(
            format!(
                "{:<width$}  prioridade  falhou  corrigiu  palavras",
                "padrão",
                width = label_width
            ),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
        lines.extend(
            patterns
                .iter()
                .take(area.height.saturating_sub(3) as usize)
                .map(|pattern| {
                    let label = if pattern.kind == "mecânica" {
                        quote_source_label(&format!("técnica {}", pattern.pattern), label_width)
                    } else {
                        quote_source_label(&pattern.pattern, label_width)
                    };
                    Line::from(vec![
                        Span::styled(
                            format!("{label:<width$}  ", width = label_width),
                            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                        ),
                        Span::styled(
                            format!("{:>10} ", priority_label(pattern.estimated_exposure_uplift)),
                            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                        ),
                        Span::styled(
                            format!(
                                "{:>6}  ",
                                evidence_fraction(
                                    pattern.uncorrected_error_rate * pattern.effective_exposures,
                                    pattern.effective_exposures
                                )
                            ),
                            Style::default().fg(theme_color(theme, &theme.error, 3.0)),
                        ),
                        Span::styled(
                            format!(
                                "{:>7}  ",
                                evidence_fraction(
                                    pattern.corrected_error_rate * pattern.effective_exposures,
                                    pattern.effective_exposures
                                )
                            ),
                            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                        ),
                        Span::styled(
                            format!("{} palavras", pattern.distinct_words),
                            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
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
    context: RenderContext<'_>,
) {
    let RenderContext {
        settings_open,
        settings_focus,
        theme_name,
        session_kind,
        persistence,
        notice,
        focus_warning,
        quote,
        keymap,
        icones,
        personal_best,
        result_animation_ms,
    } = context;
    let viewport = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(color(&theme.bg))),
        viewport,
    );
    if viewport.width < 50 || viewport.height < 14 {
        render_size_requirement(frame, viewport, theme, 50, 14, "fazer um teste");
        return;
    }

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
        if ready {
            render_config_bar(frame, viewport, engine, theme, icones);
        }
    }

    let compact = viewport.height < 18;
    let test_top = if compact {
        // O cartão compacto termina na linha 6. O teste começa depois dele,
        // sem compartilhar a borda inferior com o descritor da sessão.
        viewport.y + 7
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
        TestStatus::Completed { .. } | TestStatus::Failed { .. } => render_result(
            frame,
            result_area,
            engine,
            theme,
            ResultContext {
                session_kind,
                quote,
                icones,
                personal_best,
                animation_ms: result_animation_ms,
            },
        ),
        TestStatus::Ready | TestStatus::Running { .. } => {
            render_test(frame, test_area, engine, theme, session_kind, icones)
        }
    }
    if focus_warning
        && matches!(
            engine.status(),
            TestStatus::Ready | TestStatus::Running { .. }
        )
    {
        render_focus_warning(frame, test_area, theme, icones);
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
            FooterContext {
                persistence,
                quote,
                keymap,
                icones,
            },
        );
    }
    if settings_open {
        render_settings(
            frame,
            viewport,
            engine,
            theme,
            SettingsContext {
                theme_name,
                keymap,
                focus: settings_focus,
                icones,
            },
        );
    } else if ready && let Some(notice) = notice {
        render_startup_notice(frame, viewport, notice, theme);
    }
}

fn render_startup_notice(frame: &mut Frame, viewport: Rect, notice: &str, theme: &Theme) {
    let area = centered_width(
        centered_height(viewport, 12.min(viewport.height.saturating_sub(2))),
        76,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme_color(theme, &theme.error, 3.0)))
            .style(Style::default().bg(color(&theme.bg))),
        area,
    );
    let inner = Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(2),
        area.width.saturating_sub(4),
        area.height.saturating_sub(4),
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                "recuperação concluída",
                Style::default()
                    .fg(theme_color(theme, &theme.error, 3.0))
                    .add_modifier(Modifier::BOLD),
            ),
            Line::from(notice),
            Line::from(""),
            Line::styled(
                "enter continuar",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_header(frame: &mut Frame, area: Rect, theme: &Theme, icones: Icons) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} ", icones.teclado),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
            Span::styled(
                "tuipe",
                Style::default()
                    .fg(theme_color(theme, &theme.text, 4.5))
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
    context: SettingsContext<'_>,
) {
    let SettingsContext {
        theme_name,
        keymap,
        focus,
        icones,
    } = context;
    frame.render_widget(Clear, viewport);
    frame.render_widget(
        Block::default().style(Style::default().bg(color(&theme.bg))),
        viewport,
    );
    let area = settings_area(viewport);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(color(&theme.sub_alt)))
            .style(Style::default().bg(color(&theme.bg))),
        area,
    );
    let config = engine.config();
    if settings_are_wide(viewport) {
        render_wide_settings(frame, area, config, theme, context);
        return;
    }
    let inner = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };
    let compact = true;
    let row = |index: usize, label: &str, choices: Line<'static>| {
        let selected = focus == index;
        let mut spans = vec![Span::styled(
            if selected { "› " } else { "  " },
            Style::default()
                .fg(theme_color(theme, &theme.main, 3.0))
                .add_modifier(Modifier::BOLD),
        )];
        spans.push(Span::styled(
            format!("{label:<14}"),
            Style::default()
                .fg(theme_color(
                    theme,
                    if selected { &theme.text } else { &theme.sub },
                    if selected { 4.5 } else { 2.0 },
                ))
                .add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ));
        spans.extend(choices.spans);
        Line::from(spans)
    };
    let disabled = matches!(config.mode, TestMode::Quote);
    let mut sections = vec![
        Line::styled(
            if compact {
                format!("{}  configurações", icones.configuracoes)
            } else {
                format!(
                    "{}  configurações  ·  alterações salvas automaticamente",
                    icones.configuracoes
                )
            },
            Style::default()
                .fg(theme_color(theme, &theme.text, 4.5))
                .add_modifier(Modifier::BOLD),
        ),
        row(
            0,
            "pontuação",
            setting_toggle(
                config.punctuation,
                disabled,
                compact,
                "desligada",
                "ligada",
                theme,
            ),
        ),
        row(
            1,
            "números",
            setting_toggle(
                config.numbers,
                disabled,
                compact,
                "desligados",
                "ligados",
                theme,
            ),
        ),
        row(
            2,
            "modo",
            if compact {
                compact_setting_value(
                    match config.mode {
                        TestMode::Time { .. } => "tempo",
                        TestMode::Words { .. } => "palavras",
                        TestMode::Quote => "citação",
                    },
                    theme,
                )
            } else {
                button_group(
                    &[
                        ("tempo", matches!(config.mode, TestMode::Time { .. })),
                        ("palavras", matches!(config.mode, TestMode::Words { .. })),
                        ("citação", matches!(config.mode, TestMode::Quote)),
                    ],
                    theme,
                )
            },
        ),
        row(
            3,
            "duração",
            if compact {
                compact_setting_value(
                    match config.mode {
                        TestMode::Time { seconds } => format!("{seconds} s"),
                        TestMode::Words { count } => count.to_string(),
                        TestMode::Quote => match config.quote_length {
                            QuoteLength::All => "todas".into(),
                            QuoteLength::Short => "curta".into(),
                            QuoteLength::Medium => "média".into(),
                            QuoteLength::Long => "longa".into(),
                        },
                    },
                    theme,
                )
            } else {
                match config.mode {
                    TestMode::Time { seconds } => button_group(
                        &[
                            ("15 s", seconds == 15),
                            ("30 s", seconds == 30),
                            ("60 s", seconds == 60),
                            ("120 s", seconds == 120),
                        ],
                        theme,
                    ),
                    TestMode::Words { count } => button_group(
                        &[
                            ("10", count == 10),
                            ("25", count == 25),
                            ("50", count == 50),
                            ("100", count == 100),
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
                }
            },
        ),
        row(
            4,
            "dificuldade",
            if compact {
                compact_setting_value(difficulty_name(config.difficulty), theme)
            } else {
                button_group(
                    &[
                        ("normal", config.difficulty == Difficulty::Normal),
                        ("especialista", config.difficulty == Difficulty::Expert),
                        ("mestre", config.difficulty == Difficulty::Master),
                    ],
                    theme,
                )
            },
        ),
        row(
            5,
            "treino",
            if compact {
                compact_setting_value(
                    if config.adaptive {
                        "adaptativo"
                    } else {
                        "padrão"
                    },
                    theme,
                )
            } else {
                button_group(
                    &[
                        ("padrão", !config.adaptive),
                        ("adaptativo", config.adaptive),
                    ],
                    theme,
                )
            },
        ),
        row(
            6,
            "idioma",
            if compact {
                compact_setting_value(language_name(&config.language), theme)
            } else {
                button_group(
                    &[
                        ("português", config.language == "portuguese"),
                        ("inglês", config.language == "english"),
                    ],
                    theme,
                )
            },
        ),
        row(
            7,
            "vocabulário",
            if compact {
                compact_setting_value(
                    if config.word_pack == "common" {
                        "comum"
                    } else {
                        &config.word_pack
                    },
                    theme,
                )
            } else {
                button_group(
                    &[
                        ("comum", config.word_pack == "common"),
                        ("1k", config.word_pack == "1k"),
                        ("5k", config.word_pack == "5k"),
                    ],
                    theme,
                )
            },
        ),
        row(
            8,
            "tema",
            if compact {
                compact_setting_value(theme_name, theme)
            } else {
                Line::from(chip(theme_name.to_owned(), true, theme))
            },
        ),
    ];
    if compact {
        sections.push(key_hints(&[("↑↓", "navegar"), ("←→", "alterar")], theme));
        sections.push(key_hints(
            &[
                ("enter", "confirmar"),
                (&Keymap::label(keymap.settings), "fechar"),
                (&Keymap::label(keymap.quit), "sair"),
            ],
            theme,
        ));
    } else {
        sections.push(key_hints(
            &[
                ("↑↓", "navegar"),
                ("←→", "alterar"),
                ("enter", "confirmar e fechar"),
                (&Keymap::label(keymap.settings), "fechar"),
                (&Keymap::label(keymap.quit), "sair"),
            ],
            theme,
        ));
    }
    frame.render_widget(Paragraph::new(sections), inner);
}

#[derive(Clone, Copy)]
struct WideSettingsLayout {
    header: Rect,
    list: Rect,
    detail: Rect,
    footer: Rect,
}

fn wide_settings_layout(area: Rect) -> WideSettingsLayout {
    let inner = Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(1),
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    let body = Rect::new(
        inner.x,
        inner.y.saturating_add(2),
        inner.width,
        inner.height.saturating_sub(4),
    );
    let list_width = 31.min(body.width.saturating_sub(2));
    WideSettingsLayout {
        header: Rect::new(inner.x, inner.y, inner.width, 1),
        list: Rect::new(body.x, body.y, list_width, body.height),
        detail: Rect::new(
            body.x.saturating_add(list_width).saturating_add(2),
            body.y,
            body.width.saturating_sub(list_width).saturating_sub(2),
            body.height,
        ),
        footer: Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    }
}

fn render_wide_settings(
    frame: &mut Frame,
    area: Rect,
    config: &crate::typing::TestConfig,
    theme: &Theme,
    context: SettingsContext<'_>,
) {
    let SettingsContext {
        theme_name,
        keymap,
        focus,
        icones,
    } = context;
    let layout = wide_settings_layout(area);
    let header = Line::from(vec![
        Span::styled(
            format!("{}  configurações", icones.configuracoes),
            Style::default()
                .fg(theme_color(theme, &theme.text, 4.5))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  ·  salvo automaticamente",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
    ]);
    frame.render_widget(Paragraph::new(header), layout.header);

    let labels = [
        "pontuação",
        "números",
        "modo",
        "duração",
        "dificuldade",
        "treino",
        "idioma",
        "vocabulário",
        "tema",
    ];
    let values = settings_current_values(config, theme_name);
    let list_block = Block::default()
        .title(" preferências ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme_color(theme, &theme.sub_alt, 1.5)));
    frame.render_widget(list_block, layout.list);
    let list_inner = Rect::new(
        layout.list.x.saturating_add(1),
        layout.list.y.saturating_add(1),
        layout.list.width.saturating_sub(2),
        layout.list.height.saturating_sub(2),
    );
    for (index, (label, value)) in labels.iter().zip(values.iter()).enumerate() {
        let Some(y) = list_inner.y.checked_add(index as u16) else {
            continue;
        };
        if y >= list_inner.bottom() {
            break;
        }
        let selected = focus == index;
        let row_style = if selected {
            Style::default()
                .fg(theme_color(theme, &theme.text, 4.5))
                .bg(color(&theme.sub_alt))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme_color(theme, &theme.sub, 2.0))
        };
        let available = list_inner.width.saturating_sub(3) as usize;
        let gap = available
            .saturating_sub(UnicodeWidthStr::width(*label))
            .saturating_sub(UnicodeWidthStr::width(value.as_str()))
            .max(1);
        let line = Line::from(vec![
            Span::styled(if selected { "› " } else { "  " }, row_style),
            Span::styled((*label).to_owned(), row_style),
            Span::raw(" ".repeat(gap)),
            Span::styled(
                value.clone(),
                if selected {
                    row_style.fg(theme_color(theme, &theme.main, 3.0))
                } else {
                    row_style
                },
            ),
            Span::raw(" "),
        ]);
        frame.render_widget(
            Paragraph::new(line).style(row_style),
            Rect::new(list_inner.x, y, list_inner.width, 1),
        );
    }

    let detail_block = Block::default()
        .title(format!(" {} ", labels[focus.min(labels.len() - 1)]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme_color(theme, &theme.main, 3.0)));
    frame.render_widget(detail_block, layout.detail);
    let detail_inner = Rect::new(
        layout.detail.x.saturating_add(2),
        layout.detail.y.saturating_add(1),
        layout.detail.width.saturating_sub(4),
        layout.detail.height.saturating_sub(2),
    );
    frame.render_widget(
        Paragraph::new(settings_description(focus))
            .style(Style::default().fg(theme_color(theme, &theme.sub, 2.0)))
            .wrap(Wrap { trim: true }),
        Rect::new(detail_inner.x, detail_inner.y, detail_inner.width, 2),
    );
    let choices = settings_choices(config, theme_name, focus);
    if choices.is_empty() {
        frame.render_widget(
            Paragraph::new("indisponível no modo citação")
                .alignment(Alignment::Center)
                .style(
                    Style::default()
                        .fg(theme_color(theme, &theme.sub, 2.0))
                        .add_modifier(Modifier::DIM),
                ),
            Rect::new(
                detail_inner.x,
                detail_inner.y.saturating_add(4),
                detail_inner.width,
                1,
            ),
        );
    } else {
        let buttons = choices
            .iter()
            .map(|(label, active)| (label.as_str(), *active))
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(button_group(&buttons, theme)).alignment(Alignment::Center),
            Rect::new(
                detail_inner.x,
                detail_inner.y.saturating_add(4),
                detail_inner.width,
                1,
            ),
        );
    }
    if focus == 4 {
        render_difficulty_explanation(frame, detail_inner, config.difficulty, theme);
    }
    frame.render_widget(
        Paragraph::new(if focus == 8 {
            "← ou → percorre os temas instalados"
        } else {
            "← ou → altera  ·  enter confirma e fecha"
        })
        .alignment(Alignment::Center)
        .style(Style::default().fg(theme_color(theme, &theme.sub, 2.0))),
        Rect::new(
            detail_inner.x,
            detail_inner.bottom().saturating_sub(1),
            detail_inner.width,
            1,
        ),
    );

    frame.render_widget(
        Paragraph::new(key_hints(
            &[
                ("↑↓", "navegar"),
                ("←→", "alterar"),
                ("enter", "confirmar e fechar"),
                (&Keymap::label(keymap.settings), "fechar"),
                (&Keymap::label(keymap.quit), "sair"),
            ],
            theme,
        ))
        .alignment(Alignment::Center),
        layout.footer,
    );
}

fn render_difficulty_explanation(
    frame: &mut Frame,
    area: Rect,
    difficulty: Difficulty,
    theme: &Theme,
) {
    let explanation = difficulty_explanation(difficulty);
    frame.render_widget(
        Paragraph::new(Line::styled(
            explanation,
            Style::default().fg(theme_color(theme, &theme.text, 4.5)),
        ))
        .alignment(Alignment::Center),
        Rect::new(area.x, area.y.saturating_add(7), area.width, 1),
    );
}

fn difficulty_explanation(difficulty: Difficulty) -> &'static str {
    match difficulty {
        Difficulty::Normal => "você pode corrigir os erros e continuar",
        Difficulty::Expert => "espaço após palavra errada encerra o teste",
        Difficulty::Master => "o primeiro caractere incorreto encerra o teste",
    }
}

fn settings_current_values(config: &crate::typing::TestConfig, theme_name: &str) -> Vec<String> {
    vec![
        if config.punctuation {
            "ligada"
        } else {
            "desligada"
        }
        .into(),
        if config.numbers {
            "ligados"
        } else {
            "desligados"
        }
        .into(),
        match config.mode {
            TestMode::Time { .. } => "tempo".into(),
            TestMode::Words { .. } => "palavras".into(),
            TestMode::Quote => "citação".into(),
        },
        match config.mode {
            TestMode::Time { seconds } => format!("{seconds} s"),
            TestMode::Words { count } => format!("{count} palavras"),
            TestMode::Quote => match config.quote_length {
                QuoteLength::All => "todas".into(),
                QuoteLength::Short => "curta".into(),
                QuoteLength::Medium => "média".into(),
                QuoteLength::Long => "longa".into(),
            },
        },
        difficulty_name(config.difficulty).into(),
        if config.adaptive {
            "adaptativo"
        } else {
            "padrão"
        }
        .into(),
        language_name(&config.language).into(),
        if config.word_pack == "common" {
            "comum".into()
        } else {
            config.word_pack.clone()
        },
        theme_name.into(),
    ]
}

fn settings_description(focus: usize) -> &'static str {
    [
        "Inclui sinais de pontuação no conteúdo gerado.",
        "Mistura números às palavras do teste.",
        "Define como cada teste termina.",
        "Ajusta a duração ou a quantidade do modo atual.",
        "Escolha o que acontece depois de um erro.",
        "Personaliza automaticamente as próximas palavras.",
        "Seleciona o idioma do conteúdo.",
        "Escolhe o tamanho do vocabulário.",
        "Muda as cores de toda a interface.",
    ][focus.min(8)]
}

fn settings_choices(
    config: &crate::typing::TestConfig,
    theme_name: &str,
    focus: usize,
) -> Vec<(String, bool)> {
    let choices: &[(&str, bool)] = match focus {
        0 if !matches!(config.mode, TestMode::Quote) => &[
            ("desligada", !config.punctuation),
            ("ligada", config.punctuation),
        ],
        1 if !matches!(config.mode, TestMode::Quote) => {
            &[("desligados", !config.numbers), ("ligados", config.numbers)]
        }
        2 => &[
            ("tempo", matches!(config.mode, TestMode::Time { .. })),
            ("palavras", matches!(config.mode, TestMode::Words { .. })),
            ("citação", matches!(config.mode, TestMode::Quote)),
        ],
        3 => match config.mode {
            TestMode::Time { seconds } => &[
                ("15 s", seconds == 15),
                ("30 s", seconds == 30),
                ("60 s", seconds == 60),
                ("120 s", seconds == 120),
            ],
            TestMode::Words { count } => &[
                ("10", count == 10),
                ("25", count == 25),
                ("50", count == 50),
                ("100", count == 100),
            ],
            TestMode::Quote => &[
                ("todas", config.quote_length == QuoteLength::All),
                ("curta", config.quote_length == QuoteLength::Short),
                ("média", config.quote_length == QuoteLength::Medium),
                ("longa", config.quote_length == QuoteLength::Long),
            ],
        },
        4 => &[
            ("normal", config.difficulty == Difficulty::Normal),
            ("especialista", config.difficulty == Difficulty::Expert),
            ("mestre", config.difficulty == Difficulty::Master),
        ],
        5 => &[
            ("padrão", !config.adaptive),
            ("adaptativo", config.adaptive),
        ],
        6 => &[
            ("português", config.language == "portuguese"),
            ("inglês", config.language == "english"),
        ],
        7 => &[
            ("comum", config.word_pack == "common"),
            ("1k", config.word_pack == "1k"),
            ("5k", config.word_pack == "5k"),
        ],
        _ => &[],
    };
    if focus == 8 {
        return vec![(format!("‹  {theme_name}  ›"), true)];
    }
    choices
        .iter()
        .map(|(label, active)| ((*label).into(), *active))
        .collect()
}

fn setting_toggle(
    enabled: bool,
    disabled: bool,
    compact: bool,
    off: &'static str,
    on: &'static str,
    theme: &Theme,
) -> Line<'static> {
    if disabled {
        return Line::styled(
            "indisponível no modo citação",
            Style::default()
                .fg(theme_color(theme, &theme.sub, 2.0))
                .add_modifier(Modifier::DIM),
        );
    }
    if compact {
        return compact_setting_value(if enabled { on } else { off }, theme);
    }
    button_group(&[(off, !enabled), (on, enabled)], theme)
}

fn compact_setting_value(value: impl Into<String>, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            "‹ ",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
        Span::styled(
            value.into(),
            Style::default()
                .fg(theme_color(theme, &theme.main, 3.0))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " ›",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
    ])
}

pub fn settings_area(viewport: Rect) -> Rect {
    if settings_are_wide(viewport) {
        centered_width(centered_height(viewport, 18), 86)
    } else {
        centered_width(centered_height(viewport, 14), 72)
    }
}

fn settings_are_wide(viewport: Rect) -> bool {
    viewport.width >= 90 && viewport.height >= 22
}

pub fn settings_action_at(
    viewport: Rect,
    config: &crate::typing::TestConfig,
    theme_name: &str,
    keymap: &Keymap,
    focus: usize,
    position: Position,
) -> Option<SettingsAction> {
    let area = settings_area(viewport);
    if !area.contains(position) {
        return None;
    }
    if settings_are_wide(viewport) {
        return wide_settings_action_at(area, config, theme_name, keymap, focus, position);
    }
    let inner = Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(1),
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    let row = position.y.saturating_sub(inner.y);
    let section = usize::from(row);
    let choices_x = inner.x.saturating_add(16);
    let compact = area.width < 72;
    if compact && position.x >= choices_x && position.x < inner.right() {
        return match section {
            1 if !matches!(config.mode, TestMode::Quote) => {
                Some(SettingsAction::Punctuation(!config.punctuation))
            }
            2 if !matches!(config.mode, TestMode::Quote) => {
                Some(SettingsAction::Numbers(!config.numbers))
            }
            3 => Some(match config.mode {
                TestMode::Time { .. } => SettingsAction::ModeWords,
                TestMode::Words { .. } => SettingsAction::ModeQuote,
                TestMode::Quote => SettingsAction::ModeTime,
            }),
            4 => Some(SettingsAction::Value(match config.mode {
                TestMode::Time { seconds } => next_index(&[15, 30, 60, 120], seconds),
                TestMode::Words { count } => next_index(&[10, 25, 50, 100], count),
                TestMode::Quote => next_index(
                    &[
                        QuoteLength::All,
                        QuoteLength::Short,
                        QuoteLength::Medium,
                        QuoteLength::Long,
                    ],
                    config.quote_length,
                ),
            })),
            5 => Some(SettingsAction::Difficulty(match config.difficulty {
                Difficulty::Normal => Difficulty::Expert,
                Difficulty::Expert => Difficulty::Master,
                Difficulty::Master => Difficulty::Normal,
            })),
            6 => Some(SettingsAction::Adaptive(!config.adaptive)),
            7 => Some(if config.language == "portuguese" {
                SettingsAction::LanguageEnglish
            } else {
                SettingsAction::LanguagePortuguese
            }),
            8 => Some(match config.word_pack.as_str() {
                "common" => SettingsAction::Pack1k,
                "1k" => SettingsAction::Pack5k,
                _ => SettingsAction::PackCommon,
            }),
            9 => Some(SettingsAction::NextTheme),
            _ => None,
        };
    }
    let choice = |labels: &[&str]| hit_chip(position.x, choices_x, labels);
    match section {
        1 if !matches!(config.mode, TestMode::Quote) => {
            choice(&["desligada", "ligada"]).map(|index| SettingsAction::Punctuation(index == 1))
        }
        2 if !matches!(config.mode, TestMode::Quote) => {
            choice(&["desligados", "ligados"]).map(|index| SettingsAction::Numbers(index == 1))
        }
        3 => match choice(&["tempo", "palavras", "citação"])? {
            0 => Some(SettingsAction::ModeTime),
            1 => Some(SettingsAction::ModeWords),
            _ => Some(SettingsAction::ModeQuote),
        },
        4 => choice(match config.mode {
            TestMode::Time { .. } => &["15 s", "30 s", "60 s", "120 s"],
            TestMode::Words { .. } => &["10", "25", "50", "100"],
            TestMode::Quote => &["todas", "curta", "média", "longa"],
        })
        .map(SettingsAction::Value),
        5 => match choice(&["normal", "especialista", "mestre"])? {
            0 => Some(SettingsAction::Difficulty(Difficulty::Normal)),
            1 => Some(SettingsAction::Difficulty(Difficulty::Expert)),
            _ => Some(SettingsAction::Difficulty(Difficulty::Master)),
        },
        6 => choice(&["padrão", "adaptativo"]).map(|index| SettingsAction::Adaptive(index == 1)),
        7 => match choice(&["português", "inglês"])? {
            0 => Some(SettingsAction::LanguagePortuguese),
            _ => Some(SettingsAction::LanguageEnglish),
        },
        8 => match choice(&["comum", "1k", "5k"])? {
            0 => Some(SettingsAction::PackCommon),
            1 => Some(SettingsAction::Pack1k),
            _ => Some(SettingsAction::Pack5k),
        },
        9 => hit_chip(position.x, choices_x, &[theme_name]).map(|_| SettingsAction::NextTheme),
        10 if !compact => {
            let close = format!("{} fechar", Keymap::label(keymap.settings));
            let quit = format!("{} sair", Keymap::label(keymap.quit));
            match hit_text(
                position.x,
                inner.x,
                &[
                    "↑↓ navegar",
                    "←→ alterar",
                    "enter confirmar e fechar",
                    &close,
                    &quit,
                ],
                4,
            )? {
                3 => Some(SettingsAction::Close),
                4 => Some(SettingsAction::Quit),
                _ => None,
            }
        }
        11 if compact => {
            let close = format!("{} fechar", Keymap::label(keymap.settings));
            let quit = format!("{} sair", Keymap::label(keymap.quit));
            match hit_text(position.x, inner.x, &["enter confirmar", &close, &quit], 4)? {
                1 => Some(SettingsAction::Close),
                2 => Some(SettingsAction::Quit),
                _ => None,
            }
        }
        _ => None,
    }
}

fn wide_settings_action_at(
    area: Rect,
    config: &crate::typing::TestConfig,
    theme_name: &str,
    keymap: &Keymap,
    focus: usize,
    position: Position,
) -> Option<SettingsAction> {
    let layout = wide_settings_layout(area);
    let list_inner = Rect::new(
        layout.list.x.saturating_add(1),
        layout.list.y.saturating_add(1),
        layout.list.width.saturating_sub(2),
        layout.list.height.saturating_sub(2),
    );
    if list_inner.contains(position) {
        let index = usize::from(position.y.saturating_sub(list_inner.y));
        return (index < 9).then_some(SettingsAction::Focus(index));
    }

    let detail_inner = Rect::new(
        layout.detail.x.saturating_add(2),
        layout.detail.y.saturating_add(1),
        layout.detail.width.saturating_sub(4),
        layout.detail.height.saturating_sub(2),
    );
    let choices = settings_choices(config, theme_name, focus);
    let choices_y = detail_inner.y.saturating_add(4);
    if position.y == choices_y && !choices.is_empty() {
        let labels = choices
            .iter()
            .map(|(label, _)| label.as_str())
            .collect::<Vec<_>>();
        let width = labels.iter().fold(0_u16, |total, label| {
            total.saturating_add(UnicodeWidthStr::width(*label) as u16 + 2)
        });
        let gaps = 2_u16.saturating_mul(labels.len().saturating_sub(1) as u16);
        let start = detail_inner
            .x
            .saturating_add(detail_inner.width.saturating_sub(width + gaps) / 2);
        if let Some(index) = hit_chip(position.x, start, &labels) {
            return settings_action_for_choice(config, focus, index);
        }
    }

    if layout.footer.contains(position) {
        let close = format!("{} fechar", Keymap::label(keymap.settings));
        let quit = format!("{} sair", Keymap::label(keymap.quit));
        let labels = [
            "↑↓ navegar",
            "←→ alterar",
            "enter confirmar e fechar",
            &close,
            &quit,
        ];
        let total_width = labels
            .iter()
            .map(|label| UnicodeWidthStr::width(*label) as u16)
            .sum::<u16>()
            .saturating_add(4 * (labels.len().saturating_sub(1) as u16));
        let start = layout
            .footer
            .x
            .saturating_add(layout.footer.width.saturating_sub(total_width) / 2);
        return match hit_text(position.x, start, &labels, 4)? {
            3 => Some(SettingsAction::Close),
            4 => Some(SettingsAction::Quit),
            _ => None,
        };
    }
    None
}

fn settings_action_for_choice(
    config: &crate::typing::TestConfig,
    focus: usize,
    index: usize,
) -> Option<SettingsAction> {
    match focus {
        0 if !matches!(config.mode, TestMode::Quote) => {
            Some(SettingsAction::Punctuation(index == 1))
        }
        1 if !matches!(config.mode, TestMode::Quote) => Some(SettingsAction::Numbers(index == 1)),
        2 => Some(match index {
            0 => SettingsAction::ModeTime,
            1 => SettingsAction::ModeWords,
            2 => SettingsAction::ModeQuote,
            _ => return None,
        }),
        3 if index < 4 => Some(SettingsAction::Value(index)),
        4 => Some(SettingsAction::Difficulty(match index {
            0 => Difficulty::Normal,
            1 => Difficulty::Expert,
            2 => Difficulty::Master,
            _ => return None,
        })),
        5 if index < 2 => Some(SettingsAction::Adaptive(index == 1)),
        6 => match index {
            0 => Some(SettingsAction::LanguagePortuguese),
            1 => Some(SettingsAction::LanguageEnglish),
            _ => None,
        },
        7 => match index {
            0 => Some(SettingsAction::PackCommon),
            1 => Some(SettingsAction::Pack1k),
            2 => Some(SettingsAction::Pack5k),
            _ => None,
        },
        8 if index == 0 => Some(SettingsAction::NextTheme),
        _ => None,
    }
}

fn next_index<T: PartialEq>(values: &[T], current: T) -> usize {
    values
        .iter()
        .position(|value| *value == current)
        .map_or(0, |index| (index + 1) % values.len())
}

fn hit_chip(x: u16, start: u16, labels: &[&str]) -> Option<usize> {
    let mut cursor = start;
    for (index, label) in labels.iter().enumerate() {
        let width = UnicodeWidthStr::width(*label) as u16 + 2;
        if (cursor..cursor.saturating_add(width)).contains(&x) {
            return Some(index);
        }
        cursor = cursor.saturating_add(width + 2);
    }
    None
}

fn hit_text(x: u16, start: u16, labels: &[&str], gap: u16) -> Option<usize> {
    let mut cursor = start;
    for (index, label) in labels.iter().enumerate() {
        let width = UnicodeWidthStr::width(*label) as u16;
        if (cursor..cursor.saturating_add(width)).contains(&x) {
            return Some(index);
        }
        cursor = cursor.saturating_add(width + gap);
    }
    None
}

fn render_focus_warning(frame: &mut Frame, area: Rect, theme: &Theme, icones: Icons) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let style = frame.buffer_mut()[(x, y)].style();
            frame.buffer_mut()[(x, y)].set_style(style.add_modifier(Modifier::DIM));
        }
    }
    let warning = centered_width(centered_height(area, 1), 44);
    frame.render_widget(Clear, warning);
    frame.render_widget(
        Paragraph::new(format!(
            "{}  clique no terminal para continuar",
            icones.mouse
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(theme_color(theme, &theme.text, 4.5))),
        warning,
    );
}

fn render_config_bar(
    frame: &mut Frame,
    viewport: Rect,
    engine: &TestEngine,
    theme: &Theme,
    icones: Icons,
) {
    let config = engine.config();
    let Some(cards) = config_card_areas(viewport, &config.mode) else {
        let card = config_compact_card_area(viewport);
        render_card(
            frame,
            card,
            Line::styled(
                format!("{}  configurações", icones.configuracoes),
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            theme,
        );
        return;
    };

    let mut active = Style::default()
        .fg(theme_color(theme, &theme.main, 3.0))
        .add_modifier(Modifier::BOLD);
    let mut idle = Style::default().fg(theme_color(theme, &theme.sub, 2.0));
    if color_profile() == ColorProfile::None {
        active = active.add_modifier(Modifier::REVERSED);
        idle = idle.add_modifier(Modifier::DIM);
    }

    let modifier_idle = if matches!(config.mode, TestMode::Quote) {
        idle.add_modifier(Modifier::DIM)
    } else {
        idle
    };
    let modifiers = Line::from(vec![
        selector(
            "@ pontuação",
            config.punctuation && !matches!(config.mode, TestMode::Quote),
            active,
            modifier_idle,
        ),
        config_group_divider(theme),
        selector(
            "# números",
            config.numbers && !matches!(config.mode, TestMode::Quote),
            active,
            modifier_idle,
        ),
    ]);
    render_card(frame, cards[0], modifiers, theme);

    let modes = Line::from(vec![
        selector(
            format!("{} tempo", icones.tempo),
            matches!(config.mode, TestMode::Time { .. }),
            active,
            idle,
        ),
        config_group_divider(theme),
        selector(
            format!("{} palavras", icones.palavras),
            matches!(config.mode, TestMode::Words { .. }),
            active,
            idle,
        ),
        config_group_divider(theme),
        selector(
            format!("{} citação", icones.citacao),
            matches!(config.mode, TestMode::Quote),
            active,
            idle,
        ),
    ]);
    render_card(frame, cards[1], modes, theme);

    let values = match config.mode {
        TestMode::Time { seconds } => choices(&[15, 30, 60, 120], seconds, active, idle, theme),
        TestMode::Words { count } => choices(&[10, 25, 50, 100], count, active, idle, theme),
        TestMode::Quote => choice_names(
            &["todas", "curta", "média", "longa"],
            match config.quote_length {
                QuoteLength::All => 0,
                QuoteLength::Short => 1,
                QuoteLength::Medium => 2,
                QuoteLength::Long => 3,
            },
            active,
            idle,
            theme,
        ),
    };
    render_card(frame, cards[2], values, theme);
}

pub fn config_bar_area(viewport: Rect) -> Rect {
    let content = centered_width(viewport, MAX_PAGE_CONTENT_WIDTH);
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
    if area.width < row_width {
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

pub fn config_compact_card_area(viewport: Rect) -> Rect {
    let area = config_bar_area(viewport);
    centered_width(area, 21.min(area.width))
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

fn render_test(
    frame: &mut Frame,
    area: Rect,
    engine: &TestEngine,
    theme: &Theme,
    session_kind: SessionKind,
    icones: Icons,
) {
    let text_width = area.width;
    if text_width < 20 || area.height < 4 {
        frame.render_widget(
            Paragraph::new("terminal pequeno demais")
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme_color(theme, &theme.error, 3.0))),
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

    if matches!(engine.status(), TestStatus::Ready) {
        frame.render_widget(
            Paragraph::new(test_descriptor(engine, session_kind, icones))
                .style(Style::default().fg(theme_color(theme, &theme.sub, 2.0)))
                .alignment(Alignment::Center),
            Rect::new(area.x, area.y, area.width, 1),
        );
    } else if matches!(engine.status(), TestStatus::Running { .. }) {
        frame.render_widget(
            Paragraph::new(mini_progress(engine)).style(Style::default().fg(theme_color(
                theme,
                &theme.main,
                3.0,
            ))),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }
    for (index, line) in visible.into_iter().enumerate() {
        frame.render_widget(
            Paragraph::new(line).style(Style::default().fg(theme_color(theme, &theme.sub, 2.0))),
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

fn render_result(
    frame: &mut Frame,
    area: Rect,
    engine: &TestEngine,
    theme: &Theme,
    context: ResultContext<'_>,
) {
    let metrics = engine.metrics();
    let group_count = 7;
    let details_height = result_details_height(area.width, group_count);
    let required_height = RESULT_STATUS_HEIGHT + RESULT_CHART_HEIGHT + 2 + details_height;
    if area.height < required_height {
        render_compact_result(frame, area, engine, theme, context);
        return;
    }
    let body = centered_height(area, required_height.min(area.height));
    let status = Rect::new(body.x, body.y, body.width, RESULT_STATUS_HEIGHT);
    render_result_status(frame, status, engine, theme, context);
    let top = Rect::new(
        body.x,
        status.bottom().saturating_add(1),
        body.width,
        RESULT_CHART_HEIGHT,
    );
    render_result_chart(
        frame,
        top,
        &metrics,
        theme,
        context.session_kind,
        context.quote,
        context.icones,
    );

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
        context.icones,
    );
}

fn render_compact_result(
    frame: &mut Frame,
    area: Rect,
    engine: &TestEngine,
    theme: &Theme,
    context: ResultContext<'_>,
) {
    let metrics = engine.metrics();
    let stats = metrics.characters;
    let mut lines = vec![
        compact_result_status_line(
            engine.status(),
            metrics.wpm,
            context.personal_best,
            context.animation_ms,
            theme,
            context.icones,
        ),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "wpm ",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Span::styled(
                format!("{:.0}", metrics.wpm),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
            Span::raw("    "),
            Span::styled(
                "precisão ",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Span::styled(
                format!("{:.0}%", metrics.accuracy),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "bruto ",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Span::styled(
                format!("{:.0}", metrics.raw_wpm),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
            Span::raw("    "),
            Span::styled(
                "consistência ",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Span::styled(
                format!("{:.0}%", metrics.consistency),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "caracteres ",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Span::styled(
                format!(
                    "{}/{}/{}/{}",
                    stats.correct_word, stats.incorrect, stats.extra, stats.missed
                ),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
            Span::raw("    "),
            Span::styled(
                "tempo ",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Span::styled(
                format!("{:.1}s", metrics.duration_ms as f64 / 1_000.0),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
        ]),
    ];
    let descriptor_text = result_descriptor(engine, context.icones);
    let mut descriptor = descriptor_text
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let Some(quote) = context.quote {
        let heart = if quote.favorite {
            context.icones.favorito
        } else {
            context.icones.nao_favorito
        };
        descriptor.push(format!(
            "{heart} f favoritar · {}",
            quote_source_label(quote.source, 30)
        ));
    }
    let descriptor = descriptor.into_iter().map(|line| {
        Line::styled(
            line,
            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
        )
    });
    lines.extend(descriptor);
    let line_count = lines.len() as u16;
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        centered_height(area, line_count),
    );
}

fn render_result_status(
    frame: &mut Frame,
    area: Rect,
    engine: &TestEngine,
    theme: &Theme,
    context: ResultContext<'_>,
) {
    let status = engine.status();
    let wpm = engine.metrics().wpm;
    let personal_best = context.personal_best;
    let animation_ms = context.animation_ms;
    let personal_best = personal_best.filter(|_| matches!(status, TestStatus::Completed { .. }));
    let pulso_claro = animation_ms < 1_200 && (animation_ms / 120).is_multiple_of(2);
    let accent = if matches!(status, TestStatus::Failed { .. }) {
        theme_color(theme, &theme.error, 3.0)
    } else if pulso_claro {
        theme_color(theme, &theme.text, 4.5)
    } else {
        theme_color(theme, &theme.main, 3.0)
    };

    if personal_best.is_some() && animation_ms < 2_400 && area.width > 12 {
        render_record_particles(frame, area, animation_ms, theme);
    }

    let card = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1).min(3),
    );
    let card = centered_width(card, 58.min(area.width));
    frame.render_widget(
        Paragraph::new(result_status_line(
            status,
            wpm,
            personal_best,
            theme,
            context.icones,
        ))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(accent))
                .style(Style::default().bg(color(&theme.bg))),
        ),
        card,
    );
}

fn render_record_particles(frame: &mut Frame, area: Rect, animation_ms: u64, theme: &Theme) {
    let phase = (animation_ms / 80) as u16;
    let width = area.width.saturating_sub(2).max(1);
    let particles = [(3, 1), (11, 3), (23, 5), (37, 7), (49, 9), (61, 11)];
    for (index, (seed, speed)) in particles.into_iter().enumerate() {
        let x = area.x + 1 + (seed + phase.saturating_mul(speed)) % width;
        let symbol = if (usize::from(phase) + index).is_multiple_of(3) {
            "✦"
        } else {
            "·"
        };
        let role = if index.is_multiple_of(2) {
            &theme.main
        } else {
            &theme.text
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                symbol,
                Style::default().fg(theme_color(theme, role, 3.0)),
            )),
            Rect::new(x, area.y, 1, 1),
        );
    }
}

fn compact_result_status_line(
    status: &TestStatus,
    wpm: f64,
    personal_best: Option<PersonalBest>,
    animation_ms: u64,
    theme: &Theme,
    icones: Icons,
) -> Line<'static> {
    let mut line = result_status_line(status, wpm, personal_best, theme, icones);
    if personal_best.is_some() && animation_ms < 2_400 {
        line.spans.insert(
            0,
            Span::styled(
                "✦  ",
                Style::default().fg(theme_color(theme, &theme.text, 4.5)),
            ),
        );
        line.spans.push(Span::styled(
            "  ✦",
            Style::default().fg(theme_color(theme, &theme.text, 4.5)),
        ));
    }
    line
}

fn result_status_line(
    status: &TestStatus,
    wpm: f64,
    personal_best: Option<PersonalBest>,
    theme: &Theme,
    icones: Icons,
) -> Line<'static> {
    let (text, role) = match status {
        TestStatus::Completed { .. } => match personal_best {
            Some(PersonalBest {
                previous_wpm: Some(previous),
            }) => (
                format!(
                    "{}  NOVO RECORDE  ·  {:.0} wpm  ·  +{:.0}",
                    icones.recorde,
                    wpm,
                    (wpm - previous).max(0.0)
                ),
                &theme.main,
            ),
            Some(PersonalBest { previous_wpm: None }) => (
                format!("{}  PRIMEIRO RECORDE  ·  {:.0} wpm", icones.recorde, wpm),
                &theme.main,
            ),
            None => (format!("{}  TESTE CONCLUÍDO", icones.sucesso), &theme.main),
        },
        TestStatus::Failed { .. } => (
            format!("{}  TESTE NÃO CONCLUÍDO", icones.falha),
            &theme.error,
        ),
        TestStatus::Ready | TestStatus::Running { .. } => (String::new(), &theme.sub),
    };
    Line::styled(
        text,
        Style::default()
            .fg(theme_color(theme, role, 3.0))
            .add_modifier(Modifier::BOLD),
    )
}

fn render_result_chart(
    frame: &mut Frame,
    area: Rect,
    metrics: &Metrics,
    theme: &Theme,
    _session_kind: SessionKind,
    quote: Option<QuoteRenderState<'_>>,
    icones: Icons,
) {
    let mut context = Vec::<Line<'static>>::new();
    if let Some(quote) = quote {
        let heart = if quote.favorite {
            icones.favorito
        } else {
            icones.nao_favorito
        };
        let prefix = format!("{heart} ");
        context.push(Line::styled(
            format!(
                "{prefix}{}",
                truncate_to_width(
                    quote.source,
                    usize::from(area.width).saturating_sub(prefix.width())
                )
            ),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    }
    let sections = Layout::vertical([
        Constraint::Length(1 + context.len() as u16),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .split(area);
    let has_errors = metrics.error_history.iter().any(|count| *count > 0);
    let raw_differs = !metrics.raw_wpm_history.is_empty()
        && (metrics.raw_wpm_history.len() != metrics.wpm_history.len()
            || metrics
                .raw_wpm_history
                .iter()
                .zip(&metrics.wpm_history)
                .any(|(raw, wpm)| (raw - wpm).abs() >= 0.5));
    let mut title = vec![Span::styled(
        "wpm ao longo do tempo",
        Style::default().fg(theme_color(theme, &theme.text, 4.5)),
    )];
    if raw_differs {
        title.extend([
            Span::raw("   "),
            Span::styled(
                "bruto",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
        ]);
    }
    if has_errors {
        title.extend([
            Span::raw("   "),
            Span::styled(
                "× erros",
                Style::default().fg(theme_color(theme, &theme.error, 3.0)),
            ),
        ]);
    }
    let mut title_lines = vec![Line::from(title)];
    title_lines.extend(context);
    frame.render_widget(Paragraph::new(title_lines), sections[0]);

    let chart_columns = Layout::horizontal([
        Constraint::Length(RESULT_AXIS_LABEL_WIDTH),
        Constraint::Min(10),
        Constraint::Length(if has_errors {
            RESULT_ERROR_AXIS_LABEL_WIDTH
        } else {
            0
        }),
    ])
    .split(sections[1]);
    let labels = chart_columns[0];
    let plot = chart_columns[1];
    let error_labels = chart_columns[2];
    let wpm_points = metrics
        .wpm_history
        .iter()
        .enumerate()
        .map(|(index, value)| (index as f64, *value))
        .collect::<Vec<_>>();
    let raw_wpm_points = if raw_differs {
        metrics
            .raw_wpm_history
            .iter()
            .enumerate()
            .map(|(index, value)| (index as f64, *value))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let last_point = wpm_points.len().saturating_sub(1) as f64;
    let smoothed_wpm_points = smooth_wpm_points(&wpm_points);
    let smoothed_raw_wpm_points = smooth_wpm_points(&raw_wpm_points);
    let peak_wpm = wpm_points
        .iter()
        .chain(&raw_wpm_points)
        .map(|point| point.1)
        .fold(metrics.raw_wpm.max(metrics.wpm), f64::max);
    let chart_ceiling = ((peak_wpm.max(20.0) / 20.0).ceil() * 20.0).max(20.0);
    let (error_ceiling, error_points) = result_error_points(&metrics.error_history, chart_ceiling);

    render_chart_y_labels(frame, labels, plot, chart_ceiling, theme);
    if has_errors {
        render_chart_error_labels(frame, error_labels, plot, error_ceiling, theme);
    }
    frame.render_widget(
        Canvas::default()
            .marker(Marker::Braille)
            .background_color(color(&theme.bg))
            .block(
                Block::default()
                    .borders(Borders::LEFT | Borders::BOTTOM)
                    .border_style(Style::default().fg(theme_color(theme, &theme.main, 3.0))),
            )
            .x_bounds([0.0, last_point.max(1.0)])
            .y_bounds([0.0, chart_ceiling])
            .paint(|context| {
                let columns_per_second =
                    f64::from(plot.width.saturating_sub(1).max(1)) / last_point.max(1.0);
                for points in smoothed_raw_wpm_points.windows(2) {
                    let column = (points[0].0 * columns_per_second).floor() as u16;
                    if column % 4 < 2 {
                        context.draw(&CanvasLine {
                            x1: points[0].0,
                            y1: points[0].1,
                            x2: points[1].0,
                            y2: points[1].1,
                            color: theme_color(theme, &theme.sub, 2.0),
                        });
                    }
                }
                context.draw(&Points {
                    coords: &wpm_points,
                    color: theme_color(theme, &theme.main, 3.0),
                });
                for points in smoothed_wpm_points.windows(2) {
                    context.draw(&CanvasLine {
                        x1: points[0].0,
                        y1: points[0].1,
                        x2: points[1].0,
                        y2: points[1].1,
                        color: theme_color(theme, &theme.main, 3.0),
                    });
                }
                if has_errors {
                    context.layer();
                    context.marker(Marker::Custom('×'));
                    context.draw(&Points {
                        coords: &error_points,
                        color: theme_color(theme, &theme.error, 3.0),
                    });
                }
            }),
        plot,
    );
    render_chart_x_labels(frame, sections[2], plot, metrics, theme);
}

fn result_error_points(history: &[u32], chart_ceiling: f64) -> (u32, Vec<(f64, f64)>) {
    let error_ceiling = history.iter().copied().max().unwrap_or(0).max(1);
    let points = history
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        .map(|(index, count)| {
            (
                index as f64,
                f64::from(*count) / f64::from(error_ceiling) * chart_ceiling,
            )
        })
        .collect();
    (error_ceiling, points)
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
    let style = Style::default().fg(theme_color(theme, &theme.main, 3.0));
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

fn render_chart_error_labels(
    frame: &mut Frame,
    area: Rect,
    plot: Rect,
    ceiling: u32,
    theme: &Theme,
) {
    let style = Style::default().fg(theme_color(theme, &theme.error, 3.0));
    for (offset, label) in [
        (0, ceiling.to_string()),
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
    let style = Style::default().fg(theme_color(theme, &theme.main, 3.0));
    let duration = (metrics.duration_ms as f64 / 1_000.0).max(1.0);
    let tick_count = chart_tick_count(plot.width, metrics.wpm_history.len());
    let labels = (0..tick_count)
        .map(|index| {
            let seconds = if tick_count == 1 {
                duration
            } else {
                1.0 + (duration - 1.0) * index as f64 / (tick_count - 1) as f64
            };
            format_chart_seconds(seconds)
        })
        .collect::<Vec<_>>();
    render_chart_tick_labels(
        frame,
        Rect::new(plot.x, area.y, plot.width, 1),
        &labels,
        style,
    );
}

fn chart_tick_count(width: u16, samples: usize) -> usize {
    let desired = match width {
        72.. => 6,
        48.. => 5,
        32.. => 4,
        _ => 3,
    };
    desired.min(samples.max(1))
}

fn render_chart_tick_labels(frame: &mut Frame, area: Rect, labels: &[String], style: Style) {
    for (index, label) in labels.iter().enumerate() {
        let width = (UnicodeWidthStr::width(label.as_str()) as u16).min(area.width);
        let anchor = if labels.len() <= 1 {
            area.width.saturating_sub(1) / 2
        } else {
            (index as u32 * u32::from(area.width.saturating_sub(1)) / (labels.len() - 1) as u32)
                as u16
        };
        let x = if index == 0 {
            area.x
        } else if index + 1 == labels.len() {
            area.right().saturating_sub(width)
        } else {
            area.x
                .saturating_add(anchor.saturating_sub(width / 2))
                .min(area.right().saturating_sub(width))
        };
        frame.render_widget(
            Paragraph::new(Line::styled(label.clone(), style)),
            Rect::new(x, area.y, width, 1),
        );
    }
}

fn format_chart_seconds(seconds: f64) -> String {
    if seconds.fract().abs() < f64::EPSILON {
        format!("{seconds:.0}")
    } else {
        format!("{seconds:.1}")
    }
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
        result_group_lines_primary("wpm", format!("{:.0}", metrics.wpm), theme),
        result_group_lines_primary("precisão", format!("{:.0}%", metrics.accuracy), theme),
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
        Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
    )];
    lines.extend(result.lines().map(|line| {
        Line::styled(
            line.to_owned(),
            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
        )
    }));
    lines
}

fn result_group_lines_primary(name: &str, result: String, theme: &Theme) -> Vec<Line<'static>> {
    vec![
        Line::styled(
            name.to_owned(),
            Style::default()
                .fg(theme_color(theme, &theme.sub, 2.0))
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            result,
            Style::default()
                .fg(theme_color(theme, &theme.main, 3.0))
                .add_modifier(Modifier::BOLD),
        ),
    ]
}

fn render_footer(
    frame: &mut Frame,
    area: Rect,
    engine: &TestEngine,
    theme: &Theme,
    context: FooterContext<'_>,
) {
    let FooterContext {
        persistence,
        quote,
        keymap,
        icones,
    } = context;
    let next = Keymap::label(keymap.next);
    let repeat = Keymap::label(keymap.repeat);
    let statistics = Keymap::label(keymap.statistics);
    let statistics_global = Keymap::label(keymap.statistics_global);
    let favorite = Keymap::label(keymap.favorite);
    let quit = Keymap::label(keymap.quit);
    let settings = Keymap::label(keymap.settings);
    let favorite_icon = quote.map_or(icones.nao_favorito, |quote| {
        if quote.favorite {
            icones.favorito
        } else {
            icones.nao_favorito
        }
    });
    if matches!(
        engine.status(),
        TestStatus::Completed { .. } | TestStatus::Failed { .. }
    ) {
        match persistence {
            PersistenceUiState::Saving => {
                frame.render_widget(
                    Paragraph::new("salvando resultado…")
                        .alignment(Alignment::Center)
                        .style(Style::default().fg(theme_color(theme, &theme.sub, 2.0))),
                    area,
                );
                return;
            }
            PersistenceUiState::Failed => {
                frame.render_widget(
                    Paragraph::new(format!(
                        "não foi possível salvar · {repeat} tentar novamente"
                    ))
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(theme_color(theme, &theme.error, 3.0))),
                    area,
                );
                return;
            }
            PersistenceUiState::Saved => {}
        }
    }
    let lines = match engine.status() {
        TestStatus::Ready if area.width < 64 => vec![
            key_hints(&[(&settings, "configurações")], theme),
            key_hints(&[(&statistics_global, "estatísticas")], theme),
        ],
        TestStatus::Ready => vec![key_hints(
            &[
                (&settings, "configurações"),
                (&statistics_global, "estatísticas"),
            ],
            theme,
        )],
        TestStatus::Running { .. } => return,
        TestStatus::Completed { .. } | TestStatus::Failed { .. }
            if result_actions_are_compact(area.width, keymap, quote.is_some())
                && quote.is_some() =>
        {
            vec![
                compact_action_line(
                    [
                        (icones.proximo, &next, "próximo"),
                        (icones.repeticao, &repeat, "repetir"),
                    ],
                    theme,
                ),
                compact_action_line(
                    [
                        (icones.estatisticas, &statistics, "dados"),
                        (favorite_icon, &favorite, "favoritar"),
                        (icones.sair, &quit, "sair"),
                    ],
                    theme,
                ),
            ]
        }
        TestStatus::Completed { .. } | TestStatus::Failed { .. }
            if result_actions_are_compact(area.width, keymap, false) =>
        {
            vec![
                compact_action_line(
                    [
                        (icones.proximo, &next, "próximo"),
                        (icones.repeticao, &repeat, "repetir"),
                    ],
                    theme,
                ),
                compact_action_line(
                    [
                        (icones.estatisticas, &statistics, "dados"),
                        (icones.sair, &quit, "sair"),
                    ],
                    theme,
                ),
            ]
        }
        TestStatus::Completed { .. } | TestStatus::Failed { .. } if quote.is_some() => vec![
            Line::from(""),
            compact_action_line(
                [
                    (icones.proximo, &next, "próximo"),
                    (icones.repeticao, &repeat, "repetir"),
                    (icones.estatisticas, &statistics, "estatísticas"),
                    (favorite_icon, &favorite, "favoritar"),
                    (icones.sair, &quit, "sair"),
                ],
                theme,
            ),
        ],
        TestStatus::Completed { .. } | TestStatus::Failed { .. } => vec![
            Line::from(""),
            compact_action_line(
                [
                    (icones.proximo, &next, "próximo"),
                    (icones.repeticao, &repeat, "repetir"),
                    (icones.estatisticas, &statistics, "estatísticas"),
                    (icones.sair, &quit, "sair"),
                ],
                theme,
            ),
        ],
    };
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
}

pub fn result_action_at(
    viewport: Rect,
    keymap: &Keymap,
    has_quote: bool,
    position: Position,
) -> Option<ResultAction> {
    let content = page_content(viewport);
    if !content.contains(position) {
        return None;
    }
    let icones = icones_do_terminal();
    if result_actions_are_compact(content.width, keymap, has_quote) {
        let row_actions: &[(ResultAction, String)] = if position.y == viewport.bottom() - 2 {
            &[
                (
                    ResultAction::Next,
                    format!("{} {} próximo", icones.proximo, Keymap::label(keymap.next)),
                ),
                (
                    ResultAction::Repeat,
                    format!(
                        "{} {} repetir",
                        icones.repeticao,
                        Keymap::label(keymap.repeat)
                    ),
                ),
            ]
        } else if position.y == viewport.bottom() - 1 {
            if has_quote {
                &[
                    (
                        ResultAction::Statistics,
                        format!(
                            "{} {} dados",
                            icones.estatisticas,
                            Keymap::label(keymap.statistics)
                        ),
                    ),
                    (
                        ResultAction::Favorite,
                        format!(
                            "{} {} favoritar",
                            icones.nao_favorito,
                            Keymap::label(keymap.favorite)
                        ),
                    ),
                    (
                        ResultAction::Quit,
                        format!("{} {} sair", icones.sair, Keymap::label(keymap.quit)),
                    ),
                ]
            } else {
                &[
                    (
                        ResultAction::Statistics,
                        format!(
                            "{} {} dados",
                            icones.estatisticas,
                            Keymap::label(keymap.statistics)
                        ),
                    ),
                    (
                        ResultAction::Quit,
                        format!("{} {} sair", icones.sair, Keymap::label(keymap.quit)),
                    ),
                ]
            }
        } else {
            return None;
        };
        return centered_text_hit(content, position.x, row_actions, 4);
    }
    if position.y == viewport.bottom() - 1 {
        let mut labels = vec![
            (
                ResultAction::Next,
                format!("{} {} próximo", icones.proximo, Keymap::label(keymap.next)),
            ),
            (
                ResultAction::Repeat,
                format!(
                    "{} {} repetir",
                    icones.repeticao,
                    Keymap::label(keymap.repeat)
                ),
            ),
            (
                ResultAction::Statistics,
                format!(
                    "{} {} estatísticas",
                    icones.estatisticas,
                    Keymap::label(keymap.statistics)
                ),
            ),
        ];
        if has_quote {
            labels.push((
                ResultAction::Favorite,
                format!(
                    "{} {} favoritar",
                    icones.nao_favorito,
                    Keymap::label(keymap.favorite)
                ),
            ));
        }
        labels.push((
            ResultAction::Quit,
            format!("{} {} sair", icones.sair, Keymap::label(keymap.quit)),
        ));
        return centered_text_hit(content, position.x, &labels, 4);
    }
    None
}

fn centered_text_hit<T: Copy>(area: Rect, x: u16, actions: &[(T, String)], gap: u16) -> Option<T> {
    let widths = actions
        .iter()
        .map(|(_, label)| UnicodeWidthStr::width(label.as_str()) as u16)
        .collect::<Vec<_>>();
    let total = widths
        .iter()
        .sum::<u16>()
        .saturating_add(gap * actions.len().saturating_sub(1) as u16);
    let mut cursor = area.x + area.width.saturating_sub(total) / 2;
    for ((action, _), width) in actions.iter().zip(widths) {
        if (cursor..cursor.saturating_add(width)).contains(&x) {
            return Some(*action);
        }
        cursor = cursor.saturating_add(width + gap);
    }
    None
}

fn compact_action_line<const N: usize>(
    actions: [(&str, &str, &str); N],
    theme: &Theme,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (icon, key, action)) in actions.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("    "));
        }
        spans.push(Span::styled(
            format!("{icon} "),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
        spans.push(Span::styled(
            key.to_owned(),
            Style::default().fg(theme_color(theme, &theme.text, 4.5)),
        ));
        spans.push(Span::styled(
            format!(" {action}"),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    }
    Line::from(spans)
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
                        Style::default().fg(theme_color(theme, &theme.text, 4.5))
                    }
                    (Some(_), Some(_)) => {
                        Style::default().fg(theme_color(theme, &theme.error, 3.0))
                    }
                    (Some(_), None) => {
                        Style::default().fg(theme_color(theme, &theme.error_extra, 3.0))
                    }
                    (None, Some(_)) => Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                    (None, None) => unreachable!(),
                };
                if color_profile() == ColorProfile::None {
                    style = style.add_modifier(match (typed, expected) {
                        (Some(actual), Some(expected)) if actual == expected => Modifier::BOLD,
                        (Some(_), Some(_)) => Modifier::REVERSED,
                        (Some(_), None) => Modifier::REVERSED | Modifier::UNDERLINED,
                        (None, Some(_)) => Modifier::DIM,
                        (None, None) => Modifier::empty(),
                    });
                }
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

fn test_descriptor(engine: &TestEngine, _session_kind: SessionKind, icones: Icons) -> String {
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
        TestMode::Words { count } => format!("{} {count} palavras", icones.palavras),
        TestMode::Quote => format!("{} citação", icones.citacao),
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

fn quote_source_label(source: &str, maximum: usize) -> String {
    let graphemes = source.graphemes(true).collect::<Vec<_>>();
    if graphemes.len() <= maximum {
        source.into()
    } else {
        format!("{}…", graphemes[..maximum.saturating_sub(1)].concat())
    }
}

fn truncate_to_width(text: &str, maximum: usize) -> String {
    if text.width() <= maximum {
        return text.to_owned();
    }
    if maximum == 0 {
        return String::new();
    }
    let target = maximum.saturating_sub(1);
    let mut result = String::new();
    let mut width = 0_usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = grapheme.width();
        if width.saturating_add(grapheme_width) > target {
            break;
        }
        result.push_str(grapheme);
        width += grapheme_width;
    }
    result.push('…');
    result
}

fn result_actions_are_compact(width: u16, keymap: &Keymap, has_quote: bool) -> bool {
    let labels = result_action_labels(keymap, has_quote);
    let required = labels
        .iter()
        .map(|(_, label)| label.width())
        .sum::<usize>()
        .saturating_add(4 * labels.len().saturating_sub(1));
    required > usize::from(width)
}

fn result_action_labels(keymap: &Keymap, has_quote: bool) -> Vec<(ResultAction, String)> {
    let mut labels = vec![
        (
            ResultAction::Next,
            format!("{} próximo", Keymap::label(keymap.next)),
        ),
        (
            ResultAction::Repeat,
            format!("{} repetir", Keymap::label(keymap.repeat)),
        ),
        (
            ResultAction::Statistics,
            format!("{} estatísticas", Keymap::label(keymap.statistics)),
        ),
    ];
    if has_quote {
        labels.push((
            ResultAction::Favorite,
            format!("{} favoritar", Keymap::label(keymap.favorite)),
        ));
    }
    labels.push((
        ResultAction::Quit,
        format!("{} sair", Keymap::label(keymap.quit)),
    ));
    labels
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
            Style::default().fg(theme_color(theme, &theme.text, 4.5)),
        ));
        spans.push(Span::styled(
            format!(" {action}"),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
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
    let mut style = if active {
        Style::default()
            .fg(color(&theme.bg))
            .bg(theme_color(theme, &theme.main, 3.0))
    } else {
        Style::default()
            .fg(theme_color(theme, &theme.sub, 2.0))
            .bg(color(&theme.sub_alt))
    };
    if color_profile() == ColorProfile::None {
        style = style.add_modifier(if active {
            Modifier::BOLD | Modifier::REVERSED
        } else {
            Modifier::DIM
        });
    }
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
    theme: &Theme,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            spans.push(config_group_divider(theme));
        }
        spans.push(Span::styled(
            value.to_string(),
            if *value == selected { active } else { idle },
        ));
    }
    Line::from(spans)
}

fn choice_names(
    values: &[&str],
    selected: usize,
    active: Style,
    idle: Style,
    theme: &Theme,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            spans.push(config_group_divider(theme));
        }
        spans.push(Span::styled(
            (*value).to_owned(),
            if index == selected { active } else { idle },
        ));
    }
    Line::from(spans)
}

fn config_group_divider(theme: &Theme) -> Span<'static> {
    let mut style = Style::default().fg(theme_color(theme, &theme.sub_alt, 1.2));
    if color_profile() == ColorProfile::None {
        style = style.add_modifier(Modifier::DIM);
    }
    Span::styled(" │ ", style)
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
    centered_width(
        Rect {
            x: area.x + padding,
            width: area.width.saturating_sub(padding * 2),
            ..area
        },
        MAX_PAGE_CONTENT_WIDTH,
    )
}

fn centered_height(area: Rect, height: u16) -> Rect {
    let height = height.min(area.height);
    Rect {
        y: area.y + area.height.saturating_sub(height) / 2,
        height,
        ..area
    }
}

fn render_size_requirement(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    minimum_width: u16,
    minimum_height: u16,
    action: &str,
) {
    let lines = vec![
        Line::styled(
            "mais espaço, por favor",
            Style::default()
                .fg(theme_color(theme, &theme.text, 4.5))
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(""),
        Line::styled(
            format!("para {action}: {minimum_width}×{minimum_height}"),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
        Line::styled(
            format!("terminal atual: {}×{}", area.width, area.height),
            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
        ),
        Line::styled(
            "redimensione ou ctrl+c para sair",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        centered_height(area, 5),
    );
}

pub fn uses_true_color() -> bool {
    color_profile() == ColorProfile::TrueColor
}

/// Alinha a emissão ANSI do Crossterm ao override explícito do tuipe. Sem
/// override, `NO_COLOR` continua sendo respeitado normalmente.
pub fn configure_terminal_color_output() {
    match env::var("TUIPE_COLORS").ok().as_deref() {
        Some("truecolor" | "24bit" | "256" | "16") => {
            crossterm::style::force_color_output(true);
        }
        Some("none") => crossterm::style::force_color_output(false),
        _ => {}
    }
}

fn color_profile() -> ColorProfile {
    *COLOR_PROFILE.get_or_init(|| {
        if let Ok(profile) = env::var("TUIPE_COLORS") {
            return match profile.as_str() {
                "truecolor" | "24bit" => ColorProfile::TrueColor,
                "256" => ColorProfile::Ansi256,
                "16" => ColorProfile::Ansi16,
                "none" => ColorProfile::None,
                _ => detected_color_profile(),
            };
        }
        detected_color_profile()
    })
}

fn detected_color_profile() -> ColorProfile {
    match supports_color::on_cached(Stream::Stdout) {
        Some(support) if support.has_16m => ColorProfile::TrueColor,
        Some(support) if support.has_256 => ColorProfile::Ansi256,
        Some(support) if support.has_basic => ColorProfile::Ansi16,
        _ => ColorProfile::None,
    }
}

fn color(value: &str) -> Color {
    color_with_profile(value, color_profile())
}

fn theme_color(theme: &Theme, value: &str, minimum_contrast: f64) -> Color {
    let foreground = parse_rgb(value);
    let background = parse_rgb(&theme.bg);
    match (foreground, background) {
        (Some(foreground), Some(background)) => {
            let profile = color_profile();
            let minimum = (minimum_contrast * 100.0).round() as u16;
            let key = (foreground, background, minimum, profile);
            THEME_COLOR_CACHE.with(|cache| {
                if let Some(color) = cache.borrow().get(&key) {
                    return *color;
                }
                let resolved = contrasting_color(foreground, background, minimum_contrast, profile);
                cache.borrow_mut().insert(key, resolved);
                resolved
            })
        }
        _ => Color::Reset,
    }
}

fn contrasting_color(
    foreground: (u8, u8, u8),
    background: (u8, u8, u8),
    minimum: f64,
    profile: ColorProfile,
) -> Color {
    let adjusted = ensure_contrast(foreground, background, minimum);
    match profile {
        ColorProfile::TrueColor => Color::Rgb(adjusted.0, adjusted.1, adjusted.2),
        ColorProfile::None => Color::Reset,
        ColorProfile::Ansi256 => {
            let background = ansi256_rgb(rgb_to_ansi256(background.0, background.1, background.2));
            let candidates =
                (0_u8..=u8::MAX).map(|index| (Color::Indexed(index), ansi256_rgb(index)));
            closest_contrasting_candidate(candidates, adjusted, background, minimum)
        }
        ColorProfile::Ansi16 => {
            let background_color = rgb_to_ansi16(background.0, background.1, background.2);
            let background = ansi16_rgb(background_color).unwrap_or(background);
            closest_contrasting_candidate(ANSI16_PALETTE, adjusted, background, minimum)
        }
    }
}

fn closest_contrasting_candidate(
    candidates: impl IntoIterator<Item = (Color, (u8, u8, u8))>,
    target: (u8, u8, u8),
    background: (u8, u8, u8),
    minimum: f64,
) -> Color {
    let candidates = candidates.into_iter().collect::<Vec<_>>();
    candidates
        .iter()
        .filter(|(_, rgb)| contrast_ratio(*rgb, background) >= minimum - 0.001)
        .min_by_key(|(_, rgb)| rgb_distance(*rgb, target))
        .or_else(|| {
            candidates.iter().max_by(|(_, left), (_, right)| {
                contrast_ratio(*left, background).total_cmp(&contrast_ratio(*right, background))
            })
        })
        .map_or(Color::Reset, |(color, _)| *color)
}

fn rgb_distance(left: (u8, u8, u8), right: (u8, u8, u8)) -> u32 {
    let channel = |left: u8, right: u8| u32::from(left.abs_diff(right)).pow(2);
    channel(left.0, right.0) + channel(left.1, right.1) + channel(left.2, right.2)
}

fn color_with_profile(value: &str, profile: ColorProfile) -> Color {
    parse_rgb(value).map_or(Color::Reset, |rgb| color_from_rgb(rgb, profile))
}

fn parse_rgb(value: &str) -> Option<(u8, u8, u8)> {
    value.parse::<csscolorparser::Color>().ok().map(|parsed| {
        let [red, green, blue, _] = parsed.to_rgba8();
        (red, green, blue)
    })
}

fn color_from_rgb((red, green, blue): (u8, u8, u8), profile: ColorProfile) -> Color {
    match profile {
        ColorProfile::TrueColor => Color::Rgb(red, green, blue),
        ColorProfile::Ansi256 => Color::Indexed(rgb_to_ansi256(red, green, blue)),
        ColorProfile::Ansi16 => rgb_to_ansi16(red, green, blue),
        ColorProfile::None => Color::Reset,
    }
}

fn ensure_contrast(
    foreground: (u8, u8, u8),
    background: (u8, u8, u8),
    minimum: f64,
) -> (u8, u8, u8) {
    if contrast_ratio(foreground, background) >= minimum {
        return foreground;
    }
    let black = (0, 0, 0);
    let white = (255, 255, 255);
    let target = if contrast_ratio(white, background) >= contrast_ratio(black, background) {
        white
    } else {
        black
    };
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..12 {
        let middle = (low + high) / 2.0;
        if contrast_ratio(mix_rgb(foreground, target, middle), background) >= minimum {
            high = middle;
        } else {
            low = middle;
        }
    }
    mix_rgb(foreground, target, high)
}

fn mix_rgb(from: (u8, u8, u8), to: (u8, u8, u8), amount: f64) -> (u8, u8, u8) {
    let mix = |from: u8, to: u8| {
        (f64::from(from) + (f64::from(to) - f64::from(from)) * amount).round() as u8
    };
    (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}

fn contrast_ratio(first: (u8, u8, u8), second: (u8, u8, u8)) -> f64 {
    let first = relative_luminance(first);
    let second = relative_luminance(second);
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

fn relative_luminance((red, green, blue): (u8, u8, u8)) -> f64 {
    let linear = |value: u8| {
        let value = f64::from(value) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
}

fn rgb_to_ansi256(red: u8, green: u8, blue: u8) -> u8 {
    if red == green && green == blue {
        return match red {
            0..=7 => 16,
            248..=255 => 231,
            value => 232 + (((u16::from(value) - 8 + 5) / 10).min(23)) as u8,
        };
    }
    let quantize = |value: u8| ((u16::from(value) * 5 + 127) / 255) as u8;
    16 + 36 * quantize(red) + 6 * quantize(green) + quantize(blue)
}

fn ansi256_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        0..=15 => ANSI16_PALETTE[usize::from(index)].1,
        16..=231 => {
            let index = index - 16;
            let channel = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            (
                channel(index / 36),
                channel(index % 36 / 6),
                channel(index % 6),
            )
        }
        232..=255 => {
            let gray = 8 + (index - 232) * 10;
            (gray, gray, gray)
        }
    }
}

const ANSI16_PALETTE: [(Color, (u8, u8, u8)); 16] = [
    (Color::Black, (0, 0, 0)),
    (Color::Red, (128, 0, 0)),
    (Color::Green, (0, 128, 0)),
    (Color::Yellow, (128, 128, 0)),
    (Color::Blue, (0, 0, 128)),
    (Color::Magenta, (128, 0, 128)),
    (Color::Cyan, (0, 128, 128)),
    (Color::Gray, (192, 192, 192)),
    (Color::DarkGray, (128, 128, 128)),
    (Color::LightRed, (255, 0, 0)),
    (Color::LightGreen, (0, 255, 0)),
    (Color::LightYellow, (255, 255, 0)),
    (Color::LightBlue, (0, 0, 255)),
    (Color::LightMagenta, (255, 0, 255)),
    (Color::LightCyan, (0, 255, 255)),
    (Color::White, (255, 255, 255)),
];

fn ansi16_rgb(color: Color) -> Option<(u8, u8, u8)> {
    ANSI16_PALETTE
        .iter()
        .find_map(|(candidate, rgb)| (*candidate == color).then_some(*rgb))
}

fn rgb_to_ansi16(red: u8, green: u8, blue: u8) -> Color {
    ANSI16_PALETTE
        .iter()
        .min_by_key(|(_, candidate)| {
            let red = i32::from(red) - i32::from(candidate.0);
            let green = i32::from(green) - i32::from(candidate.1);
            let blue = i32::from(blue) - i32::from(candidate.2);
            red * red + green * green + blue * blue
        })
        .map_or(Color::Reset, |(color, _)| *color)
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
        render_engine_with_kind(width, height, engine, settings_open, SessionKind::Practice)
    }

    fn render_engine_with_kind(
        width: u16,
        height: u16,
        engine: &TestEngine,
        settings_open: bool,
        session_kind: SessionKind,
    ) -> String {
        render_engine_with_persistence(
            width,
            height,
            engine,
            settings_open,
            session_kind,
            PersistenceUiState::Saved,
        )
    }

    fn render_engine_variant(
        width: u16,
        height: u16,
        engine: &TestEngine,
        theme_name: &str,
        icones: Icons,
    ) -> String {
        let catalog = ContentCatalog::bundled().unwrap();
        let theme = catalog.theme(theme_name).unwrap();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let keymap = Keymap::default();
        terminal
            .draw(|frame| {
                render_com_icones(
                    frame,
                    engine,
                    theme,
                    RenderContext {
                        settings_open: false,
                        settings_focus: 0,
                        theme_name,
                        session_kind: SessionKind::Practice,
                        persistence: PersistenceUiState::Saved,
                        notice: None,
                        focus_warning: false,
                        quote: None,
                        keymap: &keymap,
                        icones,
                        personal_best: None,
                        result_animation_ms: 0,
                    },
                )
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

    fn render_engine_with_persistence(
        width: u16,
        height: u16,
        engine: &TestEngine,
        settings_open: bool,
        session_kind: SessionKind,
        persistence: PersistenceUiState,
    ) -> String {
        render_engine_with_state(
            width,
            height,
            engine,
            TestRenderOptions {
                settings_open,
                settings_focus: 0,
                session_kind,
                persistence,
                focus_warning: false,
                personal_best: None,
                result_animation_ms: 0,
            },
        )
    }

    #[derive(Clone, Copy)]
    struct TestRenderOptions {
        settings_open: bool,
        settings_focus: usize,
        session_kind: SessionKind,
        persistence: PersistenceUiState,
        focus_warning: bool,
        personal_best: Option<PersonalBest>,
        result_animation_ms: u64,
    }

    fn render_engine_with_state(
        width: u16,
        height: u16,
        engine: &TestEngine,
        options: TestRenderOptions,
    ) -> String {
        let TestRenderOptions {
            settings_open,
            settings_focus,
            session_kind,
            persistence,
            focus_warning,
            personal_best,
            result_animation_ms,
        } = options;
        let catalog = ContentCatalog::bundled().unwrap();
        let theme = catalog.theme("arch").unwrap();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let keymap = Keymap::default();
        terminal
            .draw(|frame| {
                render_com_icones(
                    frame,
                    engine,
                    theme,
                    RenderContext {
                        settings_open,
                        settings_focus,
                        theme_name: "arch",
                        session_kind,
                        persistence,
                        notice: None,
                        focus_warning,
                        quote: None,
                        keymap: &keymap,
                        icones: ICONES_UNICODE,
                        personal_best,
                        result_animation_ms,
                    },
                )
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

    fn render_quote_result_at(width: u16, height: u16, favorite: bool) -> String {
        let config = TestConfig {
            mode: TestMode::Quote,
            ..TestConfig::default()
        };
        let mut engine = TestEngine::new(config, ["olá".into()]);
        engine.update(InputEvent::Key {
            action: KeyAction::Text("olá".into()),
            at_ms: 10,
        });
        let catalog = ContentCatalog::bundled().unwrap();
        let theme = catalog.theme("arch").unwrap();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let keymap = Keymap::default();
        terminal
            .draw(|frame| {
                render_com_icones(
                    frame,
                    &engine,
                    theme,
                    RenderContext {
                        settings_open: false,
                        settings_focus: 0,
                        theme_name: "arch",
                        session_kind: SessionKind::Transfer,
                        persistence: PersistenceUiState::Saved,
                        notice: None,
                        focus_warning: false,
                        quote: Some(QuoteRenderState {
                            source: "Fonte muito boa",
                            favorite,
                        }),
                        keymap: &keymap,
                        icones: ICONES_UNICODE,
                        personal_best: None,
                        result_animation_ms: 0,
                    },
                );
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

    fn statistics_fixture() -> StatisticsOverview {
        StatisticsOverview {
            completed_tests: 42,
            comparable_tests: 12,
            active_ms: 3_661_000,
            average_wpm: 84.0,
            average_accuracy: 96.0,
            best_wpm: 112.0,
            trend_tests: (1_u16..=12)
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
                    kind: SessionKind::Assessment,
                })
                .collect(),
            history: vec![
                SessionHistoryItem {
                    id: 42,
                    created_at_unix_s: 1_752_500_000,
                    outcome: SessionOutcome::Completed,
                    elapsed_ms: 15_000,
                    wpm: 92.0,
                    accuracy: 98.0,
                    raw_wpm: 94.0,
                    correct_chars: 115,
                    incorrect_chars: 0,
                    extra_chars: 1,
                    missed_chars: 0,
                    config: TestConfig::default(),
                    kind: SessionKind::Assessment,
                },
                SessionHistoryItem {
                    id: 41,
                    created_at_unix_s: 1_752_413_600,
                    outcome: SessionOutcome::Failed,
                    elapsed_ms: 4_200,
                    wpm: 61.0,
                    accuracy: 86.0,
                    raw_wpm: 70.0,
                    correct_chars: 23,
                    incorrect_chars: 2,
                    extra_chars: 1,
                    missed_chars: 0,
                    config: TestConfig::default(),
                    kind: SessionKind::Practice,
                },
            ],
            distribution: vec![
                WpmBucket {
                    start: 60,
                    end: 80,
                    count: 3,
                },
                WpmBucket {
                    start: 80,
                    end: 100,
                    count: 7,
                },
                WpmBucket {
                    start: 100,
                    end: 120,
                    count: 2,
                },
            ],
            daily_activity: (7..=20)
                .map(|day| ActivityDay {
                    date: chrono::NaiveDate::from_ymd_opt(2026, 7, day).unwrap(),
                    tests: day % 4,
                    active_ms: u64::from(day % 4) * 30_000,
                    average_wpm: 70.0 + f64::from(day),
                })
                .collect(),
            priority_words: vec![PriorityWord {
                language: "portuguese".into(),
                word: "através".into(),
                difficulty: 0.4,
                confirmed_errors: 3.0,
                corrections: 2.0,
                observations: 12,
                effective_exposures: 10.0,
                uncorrected_error_rate: 0.3,
                corrected_error_rate: 0.2,
                correction_burden: 1.4,
                corrected_graphemes: 5.0,
                corrective_events: 2.0,
                correction_ms: 900.0,
                baseline_exposure_chance: 0.07,
                adaptive_exposure_chance: 0.25,
                estimated_exposure_uplift: 0.18,
            }],
            priority_patterns: vec![PriorityPattern {
                language: "portuguese".into(),
                pattern: "acento agudo".into(),
                model_pattern: "acute_accent".into(),
                kind: "mecânica",
                difficulty: 0.3,
                estimated_exposure_uplift: 0.12,
                effective_exposures: 14.0,
                uncorrected_error_rate: 0.21,
                corrected_error_rate: 0.14,
                distinct_words: 5,
            }],
            total_xp: 0,
            level: 0,
            streak: 0,
        }
    }

    fn render_statistics_at(width: u16, height: u16) -> String {
        render_statistics_page_at(width, height, StatisticsPage::Overview)
    }

    fn render_statistics_page_at(width: u16, height: u16, page: StatisticsPage) -> String {
        let catalog = ContentCatalog::bundled().unwrap();
        let theme = catalog.theme("arch").unwrap();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(Paragraph::new("resíduo da tela anterior"), frame.area());
                render_statistics(
                    frame,
                    &statistics_fixture(),
                    StatisticsRenderState {
                        page,
                        selected_word: 0,
                        selected_session: 0,
                        history_filter: HistoryFilter::All,
                        word_detail: None,
                        session_detail: None,
                    },
                    theme,
                );
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

    fn render_word_detail_at(width: u16, height: u16) -> String {
        let catalog = ContentCatalog::bundled().unwrap();
        let theme = catalog.theme("arch").unwrap();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let statistics = statistics_fixture();
        let detail = WordDetail {
            priority: statistics.priority_words[0].clone(),
            personal_baseline_ms_per_grapheme: Some(90.0),
            median_ms_per_grapheme: Some(120.0),
            last_seen_unix_s: Some(chrono::Utc::now().timestamp() - 7_200),
            relevant_sequences: vec!["av".into(), "tra".into()],
            recent_attempts: vec![
                crate::persistence::WordAttemptSummary {
                    session_id: 42,
                    observed_at_unix_s: chrono::Utc::now().timestamp() - 7_200,
                    confirmed_error: true,
                    corrected: false,
                    corrections: 0,
                    correction_ms: 0,
                    milliseconds_per_grapheme: Some(140.0),
                    latency_ratio: Some(1.4),
                },
                crate::persistence::WordAttemptSummary {
                    session_id: 41,
                    observed_at_unix_s: chrono::Utc::now().timestamp() - 86_400,
                    confirmed_error: false,
                    corrected: true,
                    corrections: 2,
                    correction_ms: 240,
                    milliseconds_per_grapheme: Some(110.0),
                    latency_ratio: Some(1.1),
                },
            ],
        };
        terminal
            .draw(|frame| {
                render_statistics(
                    frame,
                    &statistics,
                    StatisticsRenderState {
                        page: StatisticsPage::Overview,
                        selected_word: 0,
                        selected_session: 0,
                        history_filter: HistoryFilter::All,
                        word_detail: Some(&detail),
                        session_detail: None,
                    },
                    theme,
                );
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
            if width >= 90 {
                assert!(rendered.contains("citação"));
            }
            assert!(!rendered.contains("adaptativo"));
            insta::assert_snapshot!(format!("test_{width}x{height}"), rendered);
        }
    }

    #[test]
    fn modos_de_referencia_preservam_a_hierarquia_do_monkeytype() {
        let base = [
            "casa", "tempo", "mundo", "pessoa", "trabalho", "depois", "cidade", "parte", "forma",
            "lugar", "fazer", "direito", "governo", "grande", "sempre", "vida",
        ];
        let practice_words = (0..50)
            .map(|index| format!("{} ", base[index % base.len()]))
            .collect::<Vec<_>>();
        let quote_words = "A prática constante transforma hesitação em confiança e permite que cada palavra encontre seu ritmo sem sacrificar a precisão durante o caminho."
            .split_whitespace()
            .enumerate()
            .map(|(index, word)| {
                if index == 21 {
                    word.to_owned()
                } else {
                    format!("{word} ")
                }
            })
            .collect::<Vec<_>>();
        let cases = [
            (
                "tempo_30",
                TestConfig::default(),
                practice_words.clone(),
                "15 │ 30 │ 60 │ 120",
            ),
            (
                "palavras_50",
                TestConfig {
                    mode: TestMode::Words { count: 50 },
                    ..TestConfig::default()
                },
                practice_words,
                "10 │ 25 │ 50 │ 100",
            ),
            (
                "citacao",
                TestConfig {
                    mode: TestMode::Quote,
                    adaptive: false,
                    ..TestConfig::default()
                },
                quote_words,
                "todas │ curta │ média │ longa",
            ),
        ];

        for (name, config, words, values) in cases {
            let rendered = render_engine_at(100, 28, &TestEngine::new(config, words));
            assert!(rendered.contains("pontuação"));
            assert!(rendered.contains("tempo"));
            assert!(rendered.contains("palavras"));
            assert!(rendered.contains("citação"));
            assert!(rendered.contains(values));
            assert!(rendered.contains("português"));
            assert!(rendered.contains("especialista"));
            insta::assert_snapshot!(format!("referencia_{name}_100x28"), rendered);
        }
    }

    #[test]
    fn todos_os_temas_funcionam_com_nerd_font_e_fallback_unicode() {
        let catalog = ContentCatalog::bundled().unwrap();
        let engine = TestEngine::new(
            TestConfig::default(),
            ["olá ".into(), "mundo ".into(), "prática ".into()],
        );

        for theme_name in catalog.theme_names() {
            for (icones, teclado, tempo) in [
                (ICONES_NERD, ICONES_NERD.teclado, ICONES_NERD.tempo),
                (ICONES_UNICODE, ICONES_UNICODE.teclado, ICONES_UNICODE.tempo),
            ] {
                let wide = render_engine_variant(100, 28, &engine, theme_name, icones);
                let compact = render_engine_variant(50, 14, &engine, theme_name, icones);
                assert!(wide.contains(teclado), "tema {theme_name} perdeu o ícone");
                assert!(wide.contains(tempo), "tema {theme_name} perdeu o modo");
                assert!(wide.contains("tuipe"));
                assert!(wide.contains("português"));
                assert!(wide.contains("especialista"));
                assert!(compact.contains("configurações"));
                assert!(compact.contains("estatísticas"));
            }
        }
    }

    #[test]
    fn aviso_de_foco_substitui_o_texto_sem_mudar_a_geometria() {
        let engine = TestEngine::new(
            TestConfig::default(),
            ["olá ".into(), "mundo ".into(), "prática ".into()],
        );
        let rendered = render_engine_with_state(
            100,
            28,
            &engine,
            TestRenderOptions {
                settings_open: false,
                settings_focus: 0,
                session_kind: SessionKind::Practice,
                persistence: PersistenceUiState::Saved,
                focus_warning: true,
                personal_best: None,
                result_animation_ms: 0,
            },
        );

        assert!(rendered.contains("clique no terminal para continuar"));
        assert!(rendered.contains("configurações"));
    }

    #[test]
    fn clique_na_configuracao_escolhe_a_opcao_exata() {
        let viewport = Rect::new(0, 0, 100, 28);
        let area = settings_area(viewport);
        let layout = wide_settings_layout(area);
        let list = Position::new(layout.list.x + 2, layout.list.y + 1);
        let detail = Rect::new(
            layout.detail.x + 2,
            layout.detail.y + 1,
            layout.detail.width - 4,
            layout.detail.height - 2,
        );
        let config = TestConfig::default();

        assert_eq!(
            settings_action_at(
                viewport,
                &config,
                "arch",
                &Keymap::default(),
                0,
                Position::new(list.x, list.y + 2),
            ),
            Some(SettingsAction::Focus(2))
        );
        assert_eq!(
            settings_action_at(
                viewport,
                &config,
                "arch",
                &Keymap::default(),
                2,
                Position::new(detail.x + detail.width / 2, detail.y + 4),
            ),
            Some(SettingsAction::ModeWords)
        );
        assert_eq!(
            settings_action_at(
                viewport,
                &config,
                "arch",
                &Keymap::default(),
                8,
                Position::new(detail.x + detail.width / 2, detail.y + 4),
            ),
            Some(SettingsAction::NextTheme)
        );
        assert!((layout.footer.x..layout.footer.right()).any(|x| {
            settings_action_at(
                viewport,
                &config,
                "arch",
                &Keymap::default(),
                0,
                Position::new(x, layout.footer.y),
            ) == Some(SettingsAction::Quit)
        }));
        assert_eq!(
            settings_action_at(
                viewport,
                &config,
                "arch",
                &Keymap::default(),
                0,
                Position::new(area.x + 1, area.y + 1),
            ),
            None
        );
    }

    #[test]
    fn dificuldades_explicam_exatamente_quando_o_teste_termina() {
        assert_eq!(
            difficulty_explanation(Difficulty::Normal),
            "você pode corrigir os erros e continuar"
        );
        assert_eq!(
            difficulty_explanation(Difficulty::Expert),
            "espaço após palavra errada encerra o teste"
        );
        assert_eq!(
            difficulty_explanation(Difficulty::Master),
            "o primeiro caractere incorreto encerra o teste"
        );
    }

    #[test]
    fn clique_no_resultado_exige_o_controle_visivel() {
        let viewport = Rect::new(0, 0, 100, 28);
        let keymap = Keymap::default();

        assert!((0..viewport.width).all(|x| {
            result_action_at(viewport, &keymap, false, Position::new(x, 26)).is_none()
        }));
        for expected in [
            ResultAction::Next,
            ResultAction::Repeat,
            ResultAction::Statistics,
            ResultAction::Quit,
        ] {
            assert!((0..viewport.width).any(|x| {
                result_action_at(viewport, &keymap, false, Position::new(x, 27)) == Some(expected)
            }));
        }
        assert!((0..viewport.width).any(|x| {
            result_action_at(viewport, &keymap, true, Position::new(x, 27))
                == Some(ResultAction::Favorite)
        }));

        let compact = Rect::new(0, 0, 50, 14);
        assert!((0..compact.width).any(|x| {
            result_action_at(compact, &keymap, true, Position::new(x, 13))
                == Some(ResultAction::Favorite)
        }));
    }

    #[test]
    fn descritor_do_resultado_identifica_o_idioma_com_icone() {
        let engine = TestEngine::new(TestConfig::default(), ["olá ".into()]);

        let nerd = result_descriptor(&engine, ICONES_NERD);
        let unicode = result_descriptor(&engine, ICONES_UNICODE);

        assert!(
            nerd.lines()
                .any(|line| line.starts_with(ICONES_NERD.idioma))
        );
        assert!(
            unicode
                .lines()
                .any(|line| line.starts_with(ICONES_UNICODE.idioma))
        );
    }

    #[test]
    fn clique_na_palavra_prioritaria_abre_seu_detalhe() {
        let statistics = statistics_fixture();
        for viewport in [Rect::new(0, 0, 100, 28), Rect::new(0, 0, 50, 14)] {
            assert!((0..viewport.height).any(|y| {
                (0..viewport.width).any(|x| {
                    statistics_word_at(viewport, &statistics, 0, Position::new(x, y)) == Some(0)
                })
            }));
        }
    }

    #[test]
    fn teste_real_preenche_tres_linhas_no_ultrawide() {
        let words = (0..120).map(|index| format!("palavra{index:03} "));
        let engine = TestEngine::new(TestConfig::default(), words);
        let rendered = render_engine_at(180, 40, &engine);

        assert!(rendered.contains("palavra000"));
        assert!(rendered.contains("palavra030"));
    }

    #[test]
    fn tipo_automatico_da_sessao_nao_vaza_para_a_interface() {
        let engine = TestEngine::new(
            TestConfig::default(),
            ["olá ".into(), "mundo ".into(), "prática ".into()],
        );

        for kind in [
            SessionKind::Assessment,
            SessionKind::Transfer,
            SessionKind::Retention,
            SessionKind::Repeat,
        ] {
            let rendered = render_engine_with_kind(100, 28, &engine, false, kind);
            assert!(!rendered.contains("avaliação de progresso"));
            assert!(!rendered.contains("palavras novas"));
            assert!(!rendered.contains("revisão de retenção"));
        }

        let mut completed = TestEngine::new(
            TestConfig {
                mode: TestMode::Words { count: 1 },
                ..TestConfig::default()
            },
            ["olá".into()],
        );
        completed.update(InputEvent::Key {
            action: KeyAction::Text("olá".into()),
            at_ms: 10,
        });
        let narrow_result =
            render_engine_with_kind(70, 22, &completed, false, SessionKind::Assessment);
        assert!(!narrow_result.contains("avaliação de progresso"));
    }

    #[test]
    fn resultado_de_citacao_exibe_fonte_e_favorito() {
        let favorite = render_quote_result_at(100, 28, true);
        let regular = render_quote_result_at(50, 18, false);

        assert!(favorite.contains("♥ Fonte muito boa"));
        assert!(favorite.contains("f favoritar"));
        assert!(regular.contains("♡ f favoritar · Fonte muito boa"));
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
    fn recorde_pessoal_recebe_celebracao_propria() {
        let mut engine = TestEngine::new(
            TestConfig {
                mode: TestMode::Words { count: 1 },
                ..TestConfig::default()
            },
            ["velocidade".into()],
        );
        engine.update(InputEvent::Key {
            action: KeyAction::Text("v".into()),
            at_ms: 100,
        });
        engine.update(InputEvent::Key {
            action: KeyAction::Text("elocidade".into()),
            at_ms: 10_000,
        });
        let rendered = render_engine_with_state(
            100,
            28,
            &engine,
            TestRenderOptions {
                settings_open: false,
                settings_focus: 0,
                session_kind: SessionKind::Practice,
                persistence: PersistenceUiState::Saved,
                focus_warning: false,
                personal_best: Some(PersonalBest {
                    previous_wpm: Some(8.0),
                }),
                result_animation_ms: 320,
            },
        );

        assert!(rendered.contains("NOVO RECORDE"));
        assert!(rendered.contains("+"));
        insta::assert_snapshot!("test_personal_best_100x28", rendered);
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
        let rendered = render_engine_at(100, 28, &engine);
        assert!(rendered.contains("TESTE NÃO CONCLUÍDO"));
        insta::assert_snapshot!("test_failed_100x28", rendered);
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
        insta::assert_snapshot!(
            "settings_50x14",
            render_engine_with_settings(50, 14, &engine, true)
        );
        insta::assert_snapshot!(
            "settings_difficulty_100x28",
            render_engine_with_state(
                100,
                28,
                &engine,
                TestRenderOptions {
                    settings_open: true,
                    settings_focus: 4,
                    session_kind: SessionKind::Practice,
                    persistence: PersistenceUiState::Saved,
                    focus_warning: false,
                    personal_best: None,
                    result_animation_ms: 0,
                },
            )
        );
    }

    #[test]
    fn undersized_terminal_gets_an_actionable_message() {
        let rendered = render_at(40, 10);
        assert!(rendered.contains("mais espaço, por favor"));
        assert!(rendered.contains("50×14"));
        assert!(rendered.contains("40×10"));
    }

    #[test]
    fn compact_result_prioritizes_the_metrics_instead_of_clipping_the_chart() {
        let mut engine = TestEngine::new(
            TestConfig::default(),
            ["olá ".into(), "mundo ".into(), "prática ".into()],
        );
        engine.update(InputEvent::Key {
            action: KeyAction::Text("olá mundo prática ".into()),
            at_ms: 100,
        });
        engine.update(InputEvent::Tick { at_ms: 30_100 });

        for (width, height) in [(50, 14), (50, 18), (50, 20), (70, 20), (70, 22)] {
            let rendered = render_engine_at(width, height, &engine);
            assert!(rendered.contains("wpm"));
            assert!(rendered.contains("precisão"));
            assert!(rendered.contains("caracteres"));
            assert!(!rendered.contains("wpm ao longo do tempo"));
            insta::assert_snapshot!(format!("test_result_{width}x{height}"), rendered);
        }
    }

    #[test]
    fn resultado_explica_salvamento_e_retry() {
        let mut engine = TestEngine::new(TestConfig::default(), ["olá ".into()]);
        engine.update(InputEvent::Key {
            action: KeyAction::Text("olá ".into()),
            at_ms: 10,
        });
        engine.update(InputEvent::Tick { at_ms: 30_010 });

        let saving = render_engine_with_persistence(
            100,
            28,
            &engine,
            false,
            SessionKind::Practice,
            PersistenceUiState::Saving,
        );
        let failed = render_engine_with_persistence(
            100,
            28,
            &engine,
            false,
            SessionKind::Practice,
            PersistenceUiState::Failed,
        );

        assert!(saving.contains("salvando resultado"));
        assert!(failed.contains("r tentar novamente"));
    }

    #[test]
    fn statistics_overview_remains_readable() {
        insta::assert_snapshot!("statistics_180x40", render_statistics_at(180, 40));
        insta::assert_snapshot!("statistics_100x28", render_statistics_at(100, 28));
        insta::assert_snapshot!("statistics_50x14", render_statistics_at(50, 14));
    }

    #[test]
    fn progresso_e_historico_sao_telas_proprias_e_responsivas() {
        let progress = render_statistics_page_at(100, 28, StatisticsPage::Progress);
        let compact_progress = render_statistics_page_at(50, 14, StatisticsPage::Progress);
        let history = render_statistics_page_at(100, 28, StatisticsPage::History);
        let compact_history = render_statistics_page_at(50, 14, StatisticsPage::History);

        assert!(progress.contains("distribuição de wpm"));
        assert!(progress.contains("atividade diária"));
        assert!(compact_progress.contains("distribuição de wpm"));
        assert!(history.contains("#42"));
        assert!(history.contains("concluído"));
        assert!(compact_history.contains("#41"));
        assert!(!history.contains("palavras prioritárias"));
        insta::assert_snapshot!("statistics_progress_100x28", progress);
        insta::assert_snapshot!("statistics_progress_50x14", compact_progress);
        insta::assert_snapshot!("statistics_history_100x28", history);
        insta::assert_snapshot!("statistics_history_50x14", compact_history);
    }

    #[test]
    fn estatisticas_preservam_layout_nos_breakpoints_intermediarios() {
        insta::assert_snapshot!("statistics_80x28", render_statistics_at(80, 28));
        insta::assert_snapshot!("statistics_88x28", render_statistics_at(88, 28));
        insta::assert_snapshot!("statistics_95x28", render_statistics_at(95, 28));
    }

    #[test]
    fn mouse_alcanca_todos_os_controles_visiveis_das_estatisticas() {
        let statistics = statistics_fixture();
        let viewport = Rect::new(0, 0, 100, 28);
        let find = |expected| {
            (0..viewport.height).any(|y| {
                (0..viewport.width).any(|x| {
                    statistics_action_at(
                        viewport,
                        &statistics,
                        StatisticsPage::Overview,
                        0,
                        HistoryFilter::All,
                        Position::new(x, y),
                    ) == Some(expected)
                })
            })
        };
        assert!(find(StatisticsAction::ResetModel));
        assert!(find(StatisticsAction::Back));
        assert!((0..viewport.width).any(|x| {
            statistics_action_at(
                viewport,
                &statistics,
                StatisticsPage::History,
                0,
                HistoryFilter::All,
                Position::new(x, viewport.bottom() - 1),
            ) == Some(StatisticsAction::FilterHistory)
        }));
        assert!((0..viewport.width).any(|x| {
            statistics_detail_action_at(
                viewport,
                true,
                false,
                Position::new(x, viewport.bottom() - 1),
            ) == Some(StatisticsAction::ResetWord)
        }));
        assert!((0..viewport.height).any(|y| {
            (0..viewport.width).any(|x| {
                reset_confirmation_action_at(viewport, Position::new(x, y))
                    == Some(StatisticsAction::ConfirmReset)
            })
        }));

        let content = page_content(viewport);
        let footer = "↑↓ selecionar   enter detalhes   R zerar modelo   esc voltar";
        let visual_x = footer[..footer.find("R zerar").unwrap()].width() as u16;
        assert_eq!(
            statistics_action_at(
                viewport,
                &statistics,
                StatisticsPage::Overview,
                0,
                HistoryFilter::All,
                Position::new(content.x + visual_x, viewport.bottom() - 1),
            ),
            Some(StatisticsAction::ResetModel)
        );
    }

    #[test]
    fn resultado_compacto_e_mouse_usam_o_mesmo_breakpoint() {
        let viewport = Rect::new(0, 0, 65, 20);
        let keymap = Keymap::default();
        let content = page_content(viewport);
        assert!(result_actions_are_compact(content.width, &keymap, true));
        assert!((0..viewport.width).any(|x| {
            result_action_at(
                viewport,
                &keymap,
                true,
                Position::new(x, viewport.bottom() - 1),
            ) == Some(ResultAction::Favorite)
        }));
    }

    #[test]
    fn mouse_navega_as_paginas_e_o_historico() {
        let statistics = statistics_fixture();
        let viewport = Rect::new(0, 0, 100, 28);
        let content = statistics_content_area(viewport);
        assert_eq!(
            statistics_action_at(
                viewport,
                &statistics,
                StatisticsPage::Overview,
                0,
                HistoryFilter::All,
                Position::new(50, 2),
            ),
            Some(StatisticsAction::Page(StatisticsPage::Progress))
        );
        assert_eq!(
            statistics_action_at(
                viewport,
                &statistics,
                StatisticsPage::History,
                0,
                HistoryFilter::All,
                Position::new(
                    content.x + 1,
                    content.y + statistics_navigation_height(content.width) + 2,
                ),
            ),
            Some(StatisticsAction::Session(0))
        );
    }

    #[test]
    fn detalhe_da_palavra_explica_dificuldade_sem_expor_pesos_internos() {
        for (width, height) in [(50, 14), (100, 28)] {
            let rendered = render_word_detail_at(width, height);
            assert!(rendered.contains("através"));
            assert!(rendered.contains("prioridade"));
            assert!(rendered.contains("ritmo"));
            assert!(rendered.contains("tentativas recentes"));
            assert!(!rendered.contains("difficulty"));
            assert!(!rendered.contains("peso"));
            insta::assert_snapshot!(format!("statistics_word_{width}x{height}"), rendered);
        }
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

    #[test]
    fn erros_no_grafico_usam_a_escala_propria() {
        let (ceiling, points) = result_error_points(&[1, 0, 4], 120.0);

        assert_eq!(ceiling, 4);
        assert_eq!(points, vec![(0.0, 30.0), (2.0, 120.0)]);
    }

    #[test]
    fn reconhece_os_nomes_comuns_de_nerd_fonts() {
        assert!(is_nerd_font_family("JetBrainsMono Nerd Font"));
        assert!(is_nerd_font_family("font_family: Hack NF\n"));
        assert!(is_nerd_font_family("MesloLGSNFM-Regular"));
        assert!(!is_nerd_font_family("JetBrains Mono"));
    }

    #[test]
    fn reconhece_kitty_como_cliente_do_tmux() {
        assert!(terminal_name_is_kitty("xterm-kitty\n"));
        assert!(terminal_name_is_kitty("KITTY"));
        assert!(!terminal_name_is_kitty("tmux-256color"));
    }

    #[test]
    fn theme_colors_degrade_to_the_terminal_capability() {
        assert_eq!(
            color_with_profile("#123456", ColorProfile::TrueColor),
            Color::Rgb(0x12, 0x34, 0x56)
        );
        assert!(matches!(
            color_with_profile("#123456", ColorProfile::Ansi256),
            Color::Indexed(_)
        ));
        assert_eq!(
            color_with_profile("#ff0000", ColorProfile::Ansi16),
            Color::LightRed
        );
        assert_eq!(
            color_with_profile("#ffffff", ColorProfile::None),
            Color::Reset
        );
        assert_eq!(rgb_to_ansi256(0, 0, 0), 16);
        assert_eq!(rgb_to_ansi256(255, 255, 255), 231);
    }

    #[test]
    fn semantic_theme_roles_keep_a_legible_contrast() {
        let catalog = ContentCatalog::bundled().unwrap();
        for name in catalog.theme_names() {
            let theme = catalog.theme(name).unwrap();
            let background = parse_rgb(&theme.bg).unwrap();
            for (role, value, minimum) in [
                ("principal", theme.main.as_str(), 3.0),
                ("secundária", theme.sub.as_str(), 2.0),
                ("texto", theme.text.as_str(), 4.5),
                ("erro", theme.error.as_str(), 3.0),
                ("erro extra", theme.error_extra.as_str(), 3.0),
            ] {
                for profile in [
                    ColorProfile::TrueColor,
                    ColorProfile::Ansi256,
                    ColorProfile::Ansi16,
                ] {
                    let resolved =
                        contrasting_color(parse_rgb(value).unwrap(), background, minimum, profile);
                    let resolved = color_rgb(resolved).unwrap();
                    let background = color_rgb(color_from_rgb(background, profile)).unwrap();
                    assert!(
                        contrast_ratio(resolved, background) >= minimum - 0.02,
                        "tema {name}, papel {role}, perfil {profile:?} ficou abaixo do contraste mínimo"
                    );
                }
            }
        }
    }

    fn color_rgb(color: Color) -> Option<(u8, u8, u8)> {
        match color {
            Color::Rgb(red, green, blue) => Some((red, green, blue)),
            Color::Indexed(index) => Some(ansi256_rgb(index)),
            other => ansi16_rgb(other),
        }
    }
}
