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
const CONFIG_CARD_GAP: u16 = 2;
const CONFIG_MODIFIER_WIDTH: u16 = 26;
// O grupo central é o mais largo. A largura inclui as bordas e precisa caber
// tanto com Nerd Font quanto com o fallback Unicode, cujos rótulos não têm a
// mesma largura de célula.
const CONFIG_MODE_WIDTH: u16 = 31;
const CONFIG_COMPACT_VALUE_WIDTH: u16 = 20;
const CONFIG_QUOTE_VALUE_WIDTH: u16 = 26;
const RESULT_WIDE_WIDTH: u16 = 90;
const RESULT_MEDIUM_WIDTH: u16 = 54;
const RESULT_GROUP_HEIGHT: u16 = 4;
const RESULT_CHART_HEIGHT: u16 = 12;
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
    avaliacao: &'static str,
    transferencia: &'static str,
    retencao: &'static str,
    repeticao: &'static str,
    proximo: &'static str,
    estatisticas: &'static str,
    sair: &'static str,
    mouse: &'static str,
    favorito: &'static str,
    nao_favorito: &'static str,
}

const ICONES_UNICODE: Icons = Icons {
    teclado: "⌨",
    configuracoes: "⚙",
    tempo: "◷",
    palavras: "Aa",
    citacao: "❝",
    idioma: "◎",
    dificuldade: "★",
    avaliacao: "↗",
    transferencia: "⇄",
    retencao: "↺",
    repeticao: "↻",
    proximo: "›",
    estatisticas: "⌁",
    sair: "×",
    mouse: "↖",
    favorito: "♥",
    nao_favorito: "♡",
};

const ICONES_NERD: Icons = Icons {
    teclado: "",
    configuracoes: "",
    tempo: "",
    palavras: "",
    citacao: "",
    idioma: "",
    dificuldade: "",
    avaliacao: "",
    transferencia: "",
    retencao: "",
    repeticao: "",
    proximo: "",
    estatisticas: "",
    sair: "",
    mouse: "",
    favorito: "",
    nao_favorito: "",
};

fn icones_do_terminal() -> Icons {
    *ICON_PROFILE.get_or_init(|| match env::var("TUIPE_ICONS").ok().as_deref() {
        Some("unicode") => ICONES_UNICODE,
        Some("nerd") => ICONES_NERD,
        _ if active_terminal_uses_nerd_font() => ICONES_NERD,
        _ => ICONES_UNICODE,
    })
}

fn active_terminal_uses_nerd_font() -> bool {
    let kitty = env::var("KITTY_WINDOW_ID").is_ok_and(|window| !window.is_empty())
        || env::var("TERM").is_ok_and(|term| term.contains("kitty"));
    if !kitty {
        return false;
    }
    let Ok(mut query) = Command::new("kitten")
        .args(["query_terminal", "--wait-for", "0.15", "font_family"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        match query.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return false;
                }
                let mut family = String::new();
                return query
                    .stdout
                    .take()
                    .is_some_and(|mut stdout| stdout.read_to_string(&mut family).is_ok())
                    && is_nerd_font_family(&family);
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = query.kill();
                let _ = query.wait();
                return false;
            }
            Err(_) => return false,
        }
    }
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
}

#[derive(Clone, Copy)]
struct FooterContext<'a> {
    persistence: PersistenceUiState,
    quote: Option<QuoteRenderState<'a>>,
    keymap: &'a Keymap,
    icones: Icons,
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
    TogglePunctuation,
    ToggleNumbers,
    ModeTime,
    ModeWords,
    ModeQuote,
    Value(usize),
    Difficulty(Difficulty),
    ToggleAdaptive,
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
}

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
    let content = page_content(viewport);
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
    let sections = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(content);
    render_statistics_navigation(frame, sections[0], statistics, state.page, theme);
    match state.page {
        StatisticsPage::Overview => render_statistics_overview(
            frame,
            sections[1],
            statistics,
            state.selected_word,
            viewport.width < 80 || viewport.height < 24,
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
        Constraint::Length(11.min(content.height.saturating_sub(9))),
        Constraint::Length(3),
        Constraint::Min(7),
        Constraint::Length(1),
    ])
    .split(content);
    render_statistics_chart(frame, sections[0], &statistics.recent_tests, theme);
    render_statistics_summary(frame, sections[1], statistics, theme);
    let details = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
        .spacing(1)
        .split(sections[2]);
    render_priority_words(
        frame,
        details[0],
        &statistics.priority_words,
        selected_word,
        theme,
    );
    render_priority_patterns(frame, details[1], &statistics.priority_patterns, theme);
    frame.render_widget(
        Paragraph::new("↑↓ selecionar   enter detalhes   R zerar modelo   esc voltar")
            .style(Style::default().fg(theme_color(theme, &theme.sub, 2.0))),
        sections[3],
    );
}

fn render_statistics_navigation(
    frame: &mut Frame,
    area: Rect,
    statistics: &StatisticsOverview,
    active: StatisticsPage,
    theme: &Theme,
) {
    let compact = area.width < 72;
    let labels = if compact {
        ["1 visão", "2 progresso", "3 histórico"]
    } else {
        ["1 visão geral", "2 progresso", "3 histórico"]
    };
    let mut spans = if compact {
        Vec::new()
    } else {
        vec![Span::styled(
            "estatísticas  ",
            Style::default()
                .fg(theme_color(theme, &theme.text, 4.5))
                .add_modifier(Modifier::BOLD),
        )]
    };
    for (index, label) in labels.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                "  ",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ));
        }
        let selected = index
            == match active {
                StatisticsPage::Overview => 0,
                StatisticsPage::Progress => 1,
                StatisticsPage::History => 2,
            };
        spans.push(Span::styled(
            label,
            if selected {
                Style::default()
                    .fg(theme_color(theme, &theme.main, 3.0))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme_color(theme, &theme.sub, 2.0))
            },
        ));
    }
    if !compact {
        spans.push(Span::styled(
            format!(
                "  ·  nível {}  ·  sequência {} dias",
                statistics.level, statistics.streak
            ),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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
            Constraint::Length(1),
        ])
        .split(area);
        render_statistics_summary_compact(frame, sections[0], statistics, theme);
        render_wpm_distribution(frame, sections[1], &statistics.distribution, theme);
        render_activity_summary(frame, sections[2], &statistics.daily_activity, theme);
        frame.render_widget(
            Paragraph::new("tab navegar   esc voltar")
                .style(Style::default().fg(theme_color(theme, &theme.sub, 2.0))),
            sections[3],
        );
        return;
    }
    let sections = Layout::vertical([
        Constraint::Length(10),
        Constraint::Length(3),
        Constraint::Min(7),
        Constraint::Length(1),
    ])
    .split(area);
    render_statistics_chart(frame, sections[0], &statistics.recent_tests, theme);
    render_statistics_summary(frame, sections[1], statistics, theme);
    let lower = Layout::horizontal([Constraint::Percentage(48), Constraint::Percentage(52)])
        .spacing(4)
        .split(sections[2]);
    render_wpm_distribution(frame, lower[0], &statistics.distribution, theme);
    render_daily_activity(frame, lower[1], &statistics.daily_activity, theme);
    frame.render_widget(
        Paragraph::new("tab ou 1–3 navegar   esc voltar")
            .style(Style::default().fg(theme_color(theme, &theme.sub, 2.0))),
        sections[3],
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
    let mut lines = vec![Line::styled(
        "distribuição de wpm",
        Style::default().fg(theme_color(theme, &theme.text, 4.5)),
    )];
    if buckets.is_empty() {
        lines.push(Line::styled(
            "ainda sem testes na mesma configuração",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    } else {
        let maximum = buckets.iter().map(|bucket| bucket.count).max().unwrap_or(1);
        let visible = area.height.saturating_sub(1) as usize;
        let start = buckets.len().saturating_sub(visible);
        let label_width = 9;
        let bar_width = area.width.saturating_sub(label_width + 5).max(1) as usize;
        lines.extend(buckets[start..].iter().map(|bucket| {
            let filled = if maximum == 0 {
                0
            } else {
                (bucket.count as usize * bar_width).div_ceil(maximum as usize)
            };
            Line::from(vec![
                Span::styled(
                    format!("{:>3}–{:<3} ", bucket.start, bucket.end),
                    Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                ),
                Span::styled(
                    "█".repeat(filled),
                    Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                ),
                Span::styled(
                    format!(" {}", bucket.count),
                    Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                ),
            ])
        }));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_daily_activity(frame: &mut Frame, area: Rect, days: &[ActivityDay], theme: &Theme) {
    let mut lines = vec![Line::styled(
        "atividade diária  ·  últimos 14 dias",
        Style::default().fg(theme_color(theme, &theme.text, 4.5)),
    )];
    let visible = area.height.saturating_sub(1) as usize;
    let start = days.len().saturating_sub(visible);
    let maximum = days.iter().map(|day| day.active_ms).max().unwrap_or(1);
    let bar_width = area.width.saturating_sub(30).max(1) as usize;
    lines.extend(days[start..].iter().map(|day| {
        let filled = if maximum == 0 || day.active_ms == 0 {
            0
        } else {
            (day.active_ms as usize * bar_width).div_ceil(maximum as usize)
        };
        let minutes = day.active_ms as f64 / 60_000.0;
        let tests = if day.tests == 1 { "teste" } else { "testes" };
        Line::from(vec![
            Span::styled(
                format!("{}  ", day.date.format("%d/%m")),
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            Span::styled(
                if filled == 0 {
                    "·".into()
                } else {
                    "█".repeat(filled)
                },
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
            Span::styled(
                format!("  {:>2} {tests}  {:>4.1} min", day.tests, minutes),
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
        ])
    }));
    frame.render_widget(Paragraph::new(lines), area);
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
        Constraint::Length(1),
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
    frame.render_widget(
        Paragraph::new(if area.width < 72 {
            "↑↓ mover  enter abrir  f filtro  esc voltar"
        } else {
            "↑↓ selecionar   enter detalhes   f filtrar   tab navegar   esc voltar"
        })
        .style(Style::default().fg(theme_color(theme, &theme.sub, 2.0))),
        sections[2],
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
    while lines.len() < area.height.saturating_sub(1) as usize {
        lines.push(Line::from(""));
    }
    lines.truncate(area.height.saturating_sub(1) as usize);
    lines.push(Line::styled(
        "enter ou esc voltar",
        Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
    ));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
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
    let content = page_content(viewport);
    let content = Rect::new(
        content.x,
        content.y.saturating_add(1),
        content.width,
        content.height.saturating_sub(1),
    );
    if viewport.width < 80 || viewport.height < 24 {
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
        Constraint::Length(1),
        Constraint::Length(11.min(content.height.saturating_sub(9))),
        Constraint::Length(3),
        Constraint::Min(7),
        Constraint::Length(1),
    ])
    .split(content);
    let details = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
        .spacing(1)
        .split(sections[3]);
    let first_row = details[0].y.saturating_add(2);
    let visible = details[0].height.saturating_sub(2) as usize;
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
    let content = page_content(viewport);
    if position.y == content.y {
        let compact = content.width < 72;
        let mut x = content.x.saturating_add(if compact {
            0
        } else {
            "estatísticas  ".width() as u16
        });
        let labels = if compact {
            [
                ("1 visão", StatisticsPage::Overview),
                ("2 progresso", StatisticsPage::Progress),
                ("3 histórico", StatisticsPage::History),
            ]
        } else {
            [
                ("1 visão geral", StatisticsPage::Overview),
                ("2 progresso", StatisticsPage::Progress),
                ("3 histórico", StatisticsPage::History),
            ]
        };
        for (label, target) in labels {
            let right = x.saturating_add(label.width() as u16);
            if (x..right).contains(&position.x) {
                return Some(StatisticsAction::Page(target));
            }
            x = right.saturating_add(2);
        }
    }
    if page != StatisticsPage::History {
        return None;
    }
    let sessions = filtered_history(&statistics.history, filter);
    let body = Rect::new(
        content.x,
        content.y.saturating_add(1),
        content.width,
        content.height.saturating_sub(1),
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
                    "chance estimada na próxima sessão: {}",
                    estimated_chance_label(priority.estimated_session_chance)
                ),
                Style::default().fg(theme_color(theme, &theme.main, 3.0)),
            ),
            Line::styled(
                format!(
                    "falhas {:.0}%  ·  correções {:.0}%  ·  {:.0} exposições",
                    priority.uncorrected_error_rate * 100.0,
                    priority.corrected_error_rate * 100.0,
                    priority.effective_exposures
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
        while lines.len() < area.height.saturating_sub(1) as usize {
            lines.push(Line::from(""));
        }
        lines.truncate(area.height.saturating_sub(1) as usize);
        lines.push(Line::styled(
            "r zerar palavra   enter ou esc voltar",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
        frame.render_widget(Paragraph::new(lines), area);
        return;
    }

    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(8),
        Constraint::Length(1),
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
            "chance estimada na próxima sessão automática: {}",
            estimated_chance_label(priority.estimated_session_chance)
        ))
        .style(Style::default().fg(theme_color(theme, &theme.main, 3.0))),
        sections[1],
    );
    let metrics = [
        (
            "falhas",
            format!("{:.0}%", priority.uncorrected_error_rate * 100.0),
        ),
        (
            "correções",
            format!("{:.0}%", priority.corrected_error_rate * 100.0),
        ),
        ("exposições", format!("{:.0}", priority.effective_exposures)),
        ("amostras", priority.observations.to_string()),
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
    frame.render_widget(
        Paragraph::new("r zerar palavra   enter ou esc voltar")
            .style(Style::default().fg(theme_color(theme, &theme.sub, 2.0))),
        sections[5],
    );
}

fn estimated_chance_label(chance: f64) -> String {
    let chance = if chance.is_finite() {
        chance.clamp(0.0, 1.0)
    } else {
        0.0
    };
    format!("≈{:.0}%", chance * 100.0)
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
    let (status, value) = if attempt.confirmed_error {
        ("falhou", &theme.error)
    } else if attempt.corrected {
        ("corrigiu", &theme.sub)
    } else {
        ("limpa", &theme.main)
    };
    let speed = attempt.milliseconds_per_grapheme.map_or_else(
        || "sem ritmo".into(),
        |milliseconds| format!("{milliseconds:.0} ms/caractere"),
    );
    Line::from(vec![
        Span::styled(
            format!("#{:<6}", attempt.session_id),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
        Span::styled(
            format!("{status:<10}"),
            Style::default().fg(theme_color(theme, value, 3.0)),
        ),
        Span::styled(
            speed,
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
        compact_trend(&statistics.recent_tests, theme),
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
                    let chance = if area.width < 60 {
                        format!(
                            "  chance {}  ",
                            estimated_chance_label(word.estimated_session_chance)
                        )
                    } else {
                        format!(
                            "  ·  {} na próxima sessão  ·  ",
                            estimated_chance_label(word.estimated_session_chance)
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
                            chance,
                            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                        ),
                        Span::styled(
                            format!("falha {:.0}%", word.uncorrected_error_rate * 100.0),
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
                    let kind = if pattern.kind == "mecânica" {
                        "técnica"
                    } else {
                        "sequência"
                    };
                    let label = if area.width < 60 {
                        format!("{kind} {}", pattern.pattern)
                    } else {
                        format!("{} {}", pattern.kind, pattern.pattern)
                    };
                    let contexts = if area.width < 60 {
                        format!("  {} palavras  ", pattern.distinct_words)
                    } else {
                        format!("  ·  {} palavras  ·  ", pattern.distinct_words)
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
                            format!("falha {:.0}%", pattern.uncorrected_error_rate * 100.0),
                            Style::default().fg(theme_color(theme, &theme.error, 3.0)),
                        ),
                    ])
                }),
        );
    }

    while lines.len() < area.height.saturating_sub(1) as usize {
        lines.push(Line::from(""));
    }
    lines.truncate(area.height.saturating_sub(1) as usize);
    lines.push(Line::styled(
        if area.width < 64 {
            "↑↓ mover  enter detalhes  R zerar  esc voltar"
        } else {
            "↑↓ selecionar   enter detalhes   R zerar modelo   esc voltar"
        },
        Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
    ));
    frame.render_widget(Paragraph::new(lines), area);
}

fn compact_trend(sessions: &[SessionSummary], theme: &Theme) -> Line<'static> {
    if sessions.len() < 4 {
        return Line::styled(
            "tendência disponível após 4 testes na mesma configuração",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        );
    }
    let middle = sessions.len() / 2;
    let first = median_wpm(&sessions[..middle]);
    let last = median_wpm(&sessions[middle..]);
    Line::from(vec![
        Span::styled(
            format!("{} testes iguais", sessions.len()),
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
    let assessments_only = !sessions.is_empty()
        && sessions
            .iter()
            .all(|session| session.kind == crate::persistence::SessionKind::Assessment);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                if assessments_only {
                    "wpm em testes de progresso"
                } else {
                    "wpm por teste"
                },
                Style::default().fg(theme_color(theme, &theme.text, 4.5)),
            ),
            Span::styled(
                format!("  ·  {} testes mais recentes", sessions.len()),
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
                    color: theme_color(theme, &theme.main, 3.0),
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
                    .style(Style::default().fg(theme_color(theme, &theme.sub, 2.0)))
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
    let comparable_label = if statistics
        .recent_tests
        .iter()
        .all(|test| test.kind == SessionKind::Assessment)
    {
        "progresso"
    } else {
        "base comparável"
    };
    let values = [
        ("testes totais", statistics.completed_tests.to_string()),
        (comparable_label, statistics.comparable_tests.to_string()),
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
    if words.is_empty() {
        lines.push(Line::styled(
            "sem evidência suficiente",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    } else {
        lines.push(Line::styled(
            "palavra       chance  falha  correção  exposições",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
        let visible = area.height.saturating_sub(2) as usize;
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
                            format!("{:<13}", word.word),
                            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                        ),
                        Span::styled(
                            format!(
                                "{:>6}  ",
                                estimated_chance_label(word.estimated_session_chance)
                            ),
                            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                        ),
                        Span::styled(
                            format!("{:>4.0}%  ", word.uncorrected_error_rate * 100.0),
                            Style::default().fg(theme_color(theme, &theme.error, 3.0)),
                        ),
                        Span::styled(
                            format!("{:>4.0}%    ", word.corrected_error_rate * 100.0),
                            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
                        ),
                        Span::styled(
                            format!("{:>3.0}", word.effective_exposures),
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
    if patterns.is_empty() {
        lines.push(Line::styled(
            "sem evidência em palavras distintas",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    } else {
        lines.push(Line::styled(
            "tipo/padrão       falha  palavras",
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
        lines.extend(
            patterns
                .iter()
                .take(area.height.saturating_sub(2) as usize)
                .map(|pattern| {
                    let kind = if pattern.kind == "mecânica" {
                        "técnica"
                    } else {
                        "sequência"
                    };
                    let label = quote_source_label(&format!("{kind} {}", pattern.pattern), 16);
                    Line::from(vec![
                        Span::styled(
                            format!("{label:<18}"),
                            Style::default().fg(theme_color(theme, &theme.main, 3.0)),
                        ),
                        Span::styled(
                            format!("{:>4.0}% ", pattern.uncorrected_error_rate * 100.0),
                            Style::default().fg(theme_color(theme, &theme.error, 3.0)),
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
    let result = matches!(
        engine.status(),
        TestStatus::Completed { .. } | TestStatus::Failed { .. }
    );

    // O Monkeytype mantém a geometria da página enquanto o chrome desaparece
    // ao ganhar foco. Reservar as linhas impede que as palavras saltem quando
    // o primeiro caractere inicia o teste.
    if ready || result && viewport.height >= 18 {
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
            session_kind,
            quote,
            icones,
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
    let compact = viewport.width < 62 || viewport.height < 21;
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
    let inner = Rect {
        x: area.x + 2,
        y: area.y + 2,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(4),
    };
    let config = engine.config();
    let mut sections = vec![
        Line::styled(
            format!("{} configurações do teste · ↑↓ enter", icones.configuracoes),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
        if matches!(config.mode, TestMode::Quote) {
            Line::styled(
                "pontuação e números indisponíveis em citações",
                Style::default()
                    .fg(theme_color(theme, &theme.sub, 2.0))
                    .add_modifier(Modifier::DIM),
            )
        } else {
            button_group(
                &[
                    ("p pontuação", config.punctuation),
                    ("n números", config.numbers),
                ],
                theme,
            )
        },
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
        button_group(
            &[
                ("d normal", config.difficulty == Difficulty::Normal),
                ("d especialista", config.difficulty == Difficulty::Expert),
                ("d mestre", config.difficulty == Difficulty::Master),
            ],
            theme,
        ),
        button_group(&[("a adaptativo", config.adaptive)], theme),
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
            Span::styled(
                "t tema  ",
                Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
            ),
            chip(theme_name.to_owned(), true, theme),
        ]),
        Line::styled(
            format!(
                "{} fechar    {} sair",
                Keymap::label(keymap.settings),
                Keymap::label(keymap.quit)
            ),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ),
    ];
    if let Some(line) = sections.get_mut(focus.min(8) + 1) {
        *line = line
            .clone()
            .style(Style::default().bg(color(&theme.sub_alt)));
    }
    let lines = if compact {
        sections
    } else {
        sections
            .into_iter()
            .enumerate()
            .flat_map(|(index, line)| {
                if matches!(index, 1 | 3 | 5 | 7 | 9) {
                    vec![Line::from(""), line]
                } else {
                    vec![line]
                }
            })
            .collect()
    };
    frame.render_widget(Paragraph::new(lines), inner);
}

pub fn settings_area(viewport: Rect) -> Rect {
    let height = if viewport.width < 62 || viewport.height < 21 {
        14
    } else {
        21
    };
    centered_width(centered_height(viewport, height), 58)
}

pub fn settings_action_at(
    viewport: Rect,
    config: &crate::typing::TestConfig,
    theme_name: &str,
    keymap: &Keymap,
    position: Position,
) -> Option<SettingsAction> {
    let area = settings_area(viewport);
    if !area.contains(position) {
        return None;
    }
    let compact = viewport.width < 62 || viewport.height < 21;
    let inner = Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(2),
        area.width.saturating_sub(4),
        area.height.saturating_sub(4),
    );
    let row = position.y.saturating_sub(inner.y);
    let section = if compact {
        usize::from(row)
    } else {
        match row {
            0 => 0,
            2 => 1,
            3 => 2,
            5 => 3,
            6 => 4,
            8 => 5,
            9 => 6,
            11 => 7,
            12 => 8,
            14 => 9,
            _ => return None,
        }
    };
    let choice = |labels: &[&str]| hit_chip(position.x, inner.x, labels);
    match section {
        1 if !matches!(config.mode, TestMode::Quote) => {
            match choice(&["p pontuação", "n números"])? {
                0 => Some(SettingsAction::TogglePunctuation),
                _ => Some(SettingsAction::ToggleNumbers),
            }
        }
        2 => match choice(&["m tempo", "m palavras", "m citação"])? {
            0 => Some(SettingsAction::ModeTime),
            1 => Some(SettingsAction::ModeWords),
            _ => Some(SettingsAction::ModeQuote),
        },
        3 => choice(match config.mode {
            TestMode::Time { .. } => &["v 15", "v 30", "v 60", "v 120"],
            TestMode::Words { .. } => &["v 10", "v 25", "v 50", "v 100"],
            TestMode::Quote => &["todas", "curta", "média", "longa"],
        })
        .map(SettingsAction::Value),
        4 => match choice(&["d normal", "d especialista", "d mestre"])? {
            0 => Some(SettingsAction::Difficulty(Difficulty::Normal)),
            1 => Some(SettingsAction::Difficulty(Difficulty::Expert)),
            _ => Some(SettingsAction::Difficulty(Difficulty::Master)),
        },
        5 => choice(&["a adaptativo"]).map(|_| SettingsAction::ToggleAdaptive),
        6 => match choice(&["l português", "l inglês"])? {
            0 => Some(SettingsAction::LanguagePortuguese),
            _ => Some(SettingsAction::LanguageEnglish),
        },
        7 => match choice(&["k comum", "k 1k", "k 5k"])? {
            0 => Some(SettingsAction::PackCommon),
            1 => Some(SettingsAction::Pack1k),
            _ => Some(SettingsAction::Pack5k),
        },
        8 => {
            let start = inner.x + UnicodeWidthStr::width("t tema  ") as u16;
            hit_chip(position.x, start, &[theme_name]).map(|_| SettingsAction::NextTheme)
        }
        9 => {
            let close = format!("{} fechar", Keymap::label(keymap.settings));
            let quit = format!("{} sair", Keymap::label(keymap.quit));
            match hit_text(position.x, inner.x, &[&close, &quit], 4)? {
                0 => Some(SettingsAction::Close),
                _ => Some(SettingsAction::Quit),
            }
        }
        _ => None,
    }
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
    let area = config_bar_area(viewport);
    let config = engine.config();
    let Some(cards) = config_card_areas(viewport, &config.mode) else {
        let card = centered_width(area, 21.min(area.width));
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
        Span::raw(" "),
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
            &["todas", "curta", "média", "longa"],
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
    session_kind: SessionKind,
    quote: Option<QuoteRenderState<'_>>,
    icones: Icons,
) {
    let metrics = engine.metrics();
    let group_count = 7;
    let details_height = result_details_height(area.width, group_count);
    let required_height = RESULT_CHART_HEIGHT + 1 + details_height;
    if area.height < required_height {
        render_compact_result(frame, area, engine, theme, session_kind, quote, icones);
        return;
    }
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
    render_result_chart(frame, top, &metrics, theme, session_kind, quote, icones);

    let details_top = top.bottom().saturating_add(1);
    if matches!(engine.status(), TestStatus::Failed { .. }) {
        frame.render_widget(
            Paragraph::new(failed_mode_message(engine.config().difficulty))
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme_color(theme, &theme.error, 3.0))),
            Rect::new(body.x, top.bottom(), body.width, 1),
        );
    }
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

fn render_compact_result(
    frame: &mut Frame,
    area: Rect,
    engine: &TestEngine,
    theme: &Theme,
    session_kind: SessionKind,
    quote: Option<QuoteRenderState<'_>>,
    icones: Icons,
) {
    let metrics = engine.metrics();
    let stats = metrics.characters;
    let mut lines = vec![
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
        Line::from(""),
    ];
    if matches!(engine.status(), TestStatus::Failed { .. }) {
        lines[3] = Line::styled(
            failed_mode_message(engine.config().difficulty),
            Style::default().fg(theme_color(theme, &theme.error, 3.0)),
        );
    }
    let descriptor_text = result_descriptor(engine, icones);
    let mut descriptor = descriptor_text
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let Some(kind) = session_kind_descriptor(session_kind, icones) {
        descriptor.push(kind);
    }
    if let Some(quote) = quote {
        let heart = if quote.favorite {
            icones.favorito
        } else {
            icones.nao_favorito
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
    frame.render_widget(
        Paragraph::new(lines.into_iter().chain(descriptor).collect::<Vec<_>>())
            .alignment(Alignment::Center),
        centered_height(
            area,
            if quote.is_some() {
                9
            } else if session_kind == SessionKind::Practice {
                7
            } else {
                8
            },
        ),
    );
}

fn failed_mode_message(difficulty: Difficulty) -> &'static str {
    match difficulty {
        Difficulty::Master => "teste encerrado pelo modo mestre",
        Difficulty::Expert => "teste encerrado pelo modo especialista",
        Difficulty::Normal => "teste encerrado",
    }
}

fn render_result_chart(
    frame: &mut Frame,
    area: Rect,
    metrics: &Metrics,
    theme: &Theme,
    session_kind: SessionKind,
    quote: Option<QuoteRenderState<'_>>,
    icones: Icons,
) {
    let sections = Layout::vertical([
        Constraint::Length(1),
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
    if area.width >= 60
        && let Some(kind) = session_kind_descriptor(session_kind, icones)
    {
        title.push(Span::styled(
            format!("   ·   {kind}"),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    }
    if area.width >= 60
        && let Some(quote) = quote
    {
        let heart = if quote.favorite {
            icones.favorito
        } else {
            icones.nao_favorito
        };
        title.push(Span::styled(
            format!("   ·   {heart} {}", quote_source_label(quote.source, 30)),
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(title)), sections[0]);

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
            key_hints(&[("comece", "a digitar"), (&settings, "config")], theme),
            key_hints(&[(&statistics_global, "estatísticas")], theme),
        ],
        TestStatus::Ready => vec![key_hints(
            &[
                ("comece", "a digitar"),
                (&settings, "configurações"),
                (&statistics_global, "estatísticas"),
            ],
            theme,
        )],
        TestStatus::Running { .. } => return,
        TestStatus::Completed { .. } | TestStatus::Failed { .. }
            if area.width < 60 && quote.is_some() =>
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
        TestStatus::Completed { .. } | TestStatus::Failed { .. } if area.width < 60 => vec![
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
        ],
        TestStatus::Completed { .. } | TestStatus::Failed { .. } if quote.is_some() => vec![
            result_action_icons(icones, quote, theme),
            key_hints(
                &[
                    (&next, "próximo"),
                    (&repeat, "repetir"),
                    (&statistics, "estatísticas"),
                    (&favorite, "favoritar"),
                    (&quit, "sair"),
                ],
                theme,
            ),
        ],
        TestStatus::Completed { .. } | TestStatus::Failed { .. } => vec![
            result_action_icons(icones, quote, theme),
            key_hints(
                &[
                    (&next, "próximo"),
                    (&repeat, "repetir"),
                    (&statistics, "estatísticas"),
                    (&quit, "sair"),
                ],
                theme,
            ),
        ],
    };
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
}

fn result_action_icons(
    icones: Icons,
    quote: Option<QuoteRenderState<'_>>,
    theme: &Theme,
) -> Line<'static> {
    let mut spans = Vec::new();
    let icons = result_icons(icones, quote.map(|quote| quote.favorite));
    for (index, icon) in icons.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("        "));
        }
        spans.push(Span::styled(
            icon,
            Style::default().fg(theme_color(theme, &theme.sub, 2.0)),
        ));
    }
    Line::from(spans)
}

fn result_icons(icones: Icons, quote_favorite: Option<bool>) -> Vec<&'static str> {
    let mut icons = vec![
        icones.proximo,
        icones.repeticao,
        icones.estatisticas,
        icones.sair,
    ];
    if let Some(favorite) = quote_favorite {
        icons.insert(
            3,
            if favorite {
                icones.favorito
            } else {
                icones.nao_favorito
            },
        );
    }
    icons
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
    let actions = [
        ResultAction::Next,
        ResultAction::Repeat,
        ResultAction::Statistics,
        ResultAction::Favorite,
        ResultAction::Quit,
    ];
    let icones = icones_do_terminal();
    if viewport.width < 60 {
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
        return centered_text_hit(content, position.x, &labels, 4);
    }
    if position.y != viewport.bottom() - 2 {
        return None;
    }
    let icons = result_icons(icones, has_quote.then_some(false));
    let width = icons
        .iter()
        .map(|icon| UnicodeWidthStr::width(*icon) as u16)
        .sum::<u16>()
        .saturating_add(8 * icons.len().saturating_sub(1) as u16);
    let mut cursor = content.x + content.width.saturating_sub(width) / 2;
    for (index, icon) in icons.iter().enumerate() {
        let icon_width = UnicodeWidthStr::width(*icon) as u16;
        if (cursor..cursor.saturating_add(icon_width.max(1))).contains(&position.x) {
            let action_index = if !has_quote && index == 3 { 4 } else { index };
            return actions.get(action_index).copied();
        }
        cursor = cursor.saturating_add(icon_width + 8);
    }
    None
}

fn centered_text_hit(
    area: Rect,
    x: u16,
    actions: &[(ResultAction, String)],
    gap: u16,
) -> Option<ResultAction> {
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

fn test_descriptor(engine: &TestEngine, session_kind: SessionKind, icones: Icons) -> String {
    let config = engine.config();
    let mut modifiers = vec![difficulty_name(config.difficulty)];
    if config.punctuation {
        modifiers.push("pontuação");
    }
    if config.numbers {
        modifiers.push("números");
    }
    let mut descriptor = format!(
        "{} {} · {} {}",
        icones.idioma,
        language_descriptor(&config.language, &config.word_pack),
        icones.dificuldade,
        modifiers.join(" · ")
    );
    if let Some(kind) = session_kind_descriptor(session_kind, icones) {
        descriptor.push_str(" · ");
        descriptor.push_str(&kind);
    }
    descriptor
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

fn quote_source_label(source: &str, maximum: usize) -> String {
    let graphemes = source.graphemes(true).collect::<Vec<_>>();
    if graphemes.len() <= maximum {
        source.into()
    } else {
        format!("{}…", graphemes[..maximum.saturating_sub(1)].concat())
    }
}

fn session_kind_descriptor(session_kind: SessionKind, icones: Icons) -> Option<String> {
    let descriptor = match session_kind {
        SessionKind::Practice => return None,
        SessionKind::Assessment => format!("{} avaliação de progresso", icones.avaliacao),
        SessionKind::Transfer => format!("{} palavras novas", icones.transferencia),
        SessionKind::Retention => format!("{} revisão de retenção", icones.retencao),
        SessionKind::Repeat => format!("{} repetição", icones.repeticao),
    };
    Some(descriptor)
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
            settings_open,
            session_kind,
            persistence,
            false,
        )
    }

    fn render_engine_with_state(
        width: u16,
        height: u16,
        engine: &TestEngine,
        settings_open: bool,
        session_kind: SessionKind,
        persistence: PersistenceUiState,
        focus_warning: bool,
    ) -> String {
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
                        settings_focus: 0,
                        theme_name: "arch",
                        session_kind,
                        persistence,
                        notice: None,
                        focus_warning,
                        quote: None,
                        keymap: &keymap,
                        icones: ICONES_UNICODE,
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
                estimated_session_chance: 0.18,
            }],
            priority_patterns: vec![PriorityPattern {
                language: "portuguese".into(),
                pattern: "acento agudo".into(),
                kind: "mecânica",
                difficulty: 0.3,
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
                    milliseconds_per_grapheme: Some(140.0),
                    latency_ratio: Some(1.4),
                },
                crate::persistence::WordAttemptSummary {
                    session_id: 41,
                    observed_at_unix_s: chrono::Utc::now().timestamp() - 86_400,
                    confirmed_error: false,
                    corrected: true,
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
                "15  30  60  120",
            ),
            (
                "palavras_50",
                TestConfig {
                    mode: TestMode::Words { count: 50 },
                    ..TestConfig::default()
                },
                practice_words,
                "10  25  50  100",
            ),
            (
                "citacao",
                TestConfig {
                    mode: TestMode::Quote,
                    adaptive: false,
                    ..TestConfig::default()
                },
                quote_words,
                "todas",
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
            false,
            SessionKind::Practice,
            PersistenceUiState::Saved,
            true,
        );

        assert!(rendered.contains("clique no terminal para continuar"));
        assert!(rendered.contains("configurações"));
    }

    #[test]
    fn clique_na_configuracao_escolhe_a_opcao_exata() {
        let viewport = Rect::new(0, 0, 100, 28);
        let area = settings_area(viewport);
        let inner = Position::new(area.x + 2, area.y + 2);
        let config = TestConfig::default();

        assert_eq!(
            settings_action_at(
                viewport,
                &config,
                "arch",
                &Keymap::default(),
                Position::new(inner.x + 12, inner.y + 3),
            ),
            Some(SettingsAction::ModeWords)
        );
        assert_eq!(
            settings_action_at(
                viewport,
                &config,
                "arch",
                &Keymap::default(),
                Position::new(inner.x + 1, inner.y + 6),
            ),
            Some(SettingsAction::Difficulty(Difficulty::Normal))
        );
        assert_eq!(
            settings_action_at(
                viewport,
                &config,
                "arch",
                &Keymap::default(),
                Position::new(inner.x + 9, inner.y + 12),
            ),
            Some(SettingsAction::NextTheme)
        );
        assert_eq!(
            settings_action_at(
                viewport,
                &config,
                "arch",
                &Keymap::default(),
                Position::new(inner.x + 30, inner.y + 12),
            ),
            None
        );
        assert_eq!(
            settings_action_at(
                viewport,
                &config,
                "arch",
                &Keymap::default(),
                Position::new(inner.x + 15, inner.y + 14),
            ),
            Some(SettingsAction::Quit)
        );
        assert_eq!(
            settings_action_at(
                viewport,
                &config,
                "arch",
                &Keymap::default(),
                Position::new(inner.x + 40, inner.y + 14),
            ),
            None
        );
    }

    #[test]
    fn clique_no_resultado_exige_o_controle_visivel() {
        let viewport = Rect::new(0, 0, 100, 28);
        let keymap = Keymap::default();

        assert_eq!(
            result_action_at(viewport, &keymap, false, Position::new(36, 26)),
            Some(ResultAction::Next)
        );
        assert_eq!(
            result_action_at(viewport, &keymap, false, Position::new(63, 26)),
            Some(ResultAction::Quit)
        );
        assert_eq!(
            result_action_at(viewport, &keymap, false, Position::new(95, 26)),
            None
        );
        assert_eq!(
            result_action_at(viewport, &keymap, false, Position::new(24, 27)),
            Some(ResultAction::Next)
        );
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
    fn falha_exibe_o_modo_que_realmente_a_encerrou() {
        assert_eq!(
            failed_mode_message(Difficulty::Expert),
            "teste encerrado pelo modo especialista"
        );
        assert_eq!(
            failed_mode_message(Difficulty::Master),
            "teste encerrado pelo modo mestre"
        );
    }

    #[test]
    fn clique_na_palavra_prioritaria_abre_seu_detalhe() {
        let statistics = statistics_fixture();
        assert_eq!(
            statistics_word_at(
                Rect::new(0, 0, 100, 28),
                &statistics,
                0,
                Position::new(6, 18),
            ),
            Some(0)
        );
        assert_eq!(
            statistics_word_at(Rect::new(0, 0, 50, 14), &statistics, 0, Position::new(2, 7),),
            Some(0)
        );
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
    fn automatic_session_kind_is_explained_without_becoming_a_setting() {
        let engine = TestEngine::new(
            TestConfig::default(),
            ["olá ".into(), "mundo ".into(), "prática ".into()],
        );

        for (kind, expected) in [
            (SessionKind::Assessment, "avaliação de progresso"),
            (SessionKind::Transfer, "palavras novas"),
            (SessionKind::Retention, "revisão de retenção"),
            (SessionKind::Repeat, "repetição"),
        ] {
            let rendered = render_engine_with_kind(100, 28, &engine, false, kind);
            assert!(rendered.contains(expected));
            assert!(!rendered.contains("escolher sessão"));
        }
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
        insta::assert_snapshot!(
            "settings_50x14",
            render_engine_with_settings(50, 14, &engine, true)
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
    fn mouse_navega_as_paginas_e_o_historico() {
        let statistics = statistics_fixture();
        assert_eq!(
            statistics_action_at(
                Rect::new(0, 0, 100, 28),
                &statistics,
                StatisticsPage::Overview,
                0,
                HistoryFilter::All,
                Position::new(35, 0),
            ),
            Some(StatisticsAction::Page(StatisticsPage::Progress))
        );
        assert_eq!(
            statistics_action_at(
                Rect::new(0, 0, 100, 28),
                &statistics,
                StatisticsPage::History,
                0,
                HistoryFilter::All,
                Position::new(6, 3),
            ),
            Some(StatisticsAction::Session(0))
        );
    }

    #[test]
    fn detalhe_da_palavra_explica_dificuldade_sem_expor_pesos_internos() {
        for (width, height) in [(50, 14), (100, 28)] {
            let rendered = render_word_detail_at(width, height);
            assert!(rendered.contains("através"));
            assert!(rendered.contains("chance"));
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
