//! Drawing. Reads [`App`], changes nothing but scroll limits.

use apibreak_core::body::{self, BodyView};
use apibreak_core::store::{Captured, Headers};
use apibreak_core::{Exchange, Outcome};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, Focus, Mode};
use super::{highlight, style};

/// Below this list width the time column gives its room to the path.
const WIDE_LIST: u16 = 56;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, scenarios, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let [list, details] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body);

    draw_header(frame, app, header);
    draw_list(frame, app, list);
    draw_details(frame, app, details);
    frame.render_widget(scenario_line(app), scenarios);
    draw_footer(frame, app, footer);
    if app.mode == Mode::Help {
        draw_help(frame);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled("apibreak ", style::heading()),
        Span::raw(app.title.clone()),
    ];
    if app.paused {
        spans.push(Span::styled("  paused", style::warning()));
    }
    frame.render_widget(Line::from(spans), area);
}

fn scenario_line(app: &App) -> Line<'static> {
    let summary = app.active.summary();
    if summary.is_empty() {
        return Line::styled("no active scenarios", style::muted());
    }
    Line::styled(format!("⚠ {}", summary.join(" · ")), style::warning())
}

fn draw_list(frame: &mut Frame, app: &App, area: Rect) {
    let visible = app.visible();
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(style::border(app.focus == Focus::List))
        .title(format!(" Requests {} ", visible.len()));
    let inner = area.width.saturating_sub(2);
    let items: Vec<ListItem> = visible
        .iter()
        .map(|entry| ListItem::new(list_row(entry, inner)))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(style::selected());
    let mut state = ListState::default().with_selected(app.selected_index(&visible));
    frame.render_stateful_widget(list, area, &mut state);
}

fn list_row(entry: &Exchange, width: u16) -> Line<'static> {
    let (status, status_style) = match &entry.outcome {
        Outcome::Pending => ("···".to_owned(), style::muted()),
        Outcome::Failed(_) => ("ERR".to_owned(), style::error()),
        Outcome::Response(response) => {
            (response.status.to_string(), style::status(response.status))
        }
    };
    let mark = if entry.scenario.is_some() { "⚠" } else { " " };
    let duration = entry.duration.map_or_else(String::new, format_duration);
    let mut spans = Vec::new();
    let mut used = 7 + 4 + 2 + 7;
    if width >= WIDE_LIST {
        spans.push(Span::styled(
            format!("{} ", entry.started_at.format("%H:%M:%S")),
            style::muted(),
        ));
        used += 9;
    }
    spans.push(Span::styled(
        format!("{:<7}", entry.method),
        style::accent(),
    ));
    spans.push(Span::styled(format!("{status} "), status_style));
    spans.push(Span::styled(format!("{mark} "), style::warning()));
    let path_width = usize::from(width).saturating_sub(used);
    spans.push(Span::raw(format!(
        "{:<path_width$}",
        shorten(&entry.uri, path_width)
    )));
    spans.push(Span::styled(format!("{duration:>7}"), style::muted()));
    Line::from(spans)
}

/// Cuts `text` to `width` characters, marking the cut.
fn shorten(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(width.saturating_sub(1)).collect();
    short.push('…');
    short
}

fn format_duration(duration: std::time::Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1000 {
        format!("{millis}ms")
    } else {
        format!("{:.1}s", duration.as_secs_f64())
    }
}

fn draw_details(frame: &mut Frame, app: &mut App, area: Rect) {
    let lines = app.detail_lines();
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(style::border(app.focus == Focus::Details))
        .title(" Details ");
    let inner = block.inner(area);

    let rows: usize = lines
        .iter()
        .map(|line| wrapped_rows(line, inner.width))
        .sum();
    let limit = rows.saturating_sub(usize::from(inner.height));
    app.detail_max_scroll = u16::try_from(limit).unwrap_or(u16::MAX);
    app.detail_scroll = app.detail_scroll.min(app.detail_max_scroll);

    let paragraph = Paragraph::new(lines.to_vec())
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0));
    frame.render_widget(paragraph, area);
}

fn wrapped_rows(line: &Line, width: u16) -> usize {
    line.width().div_ceil(usize::from(width.max(1))).max(1)
}

pub fn detail_lines(entry: &Exchange) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{} ", entry.method), style::heading()),
        Span::raw(entry.uri.clone()),
    ])];
    match &entry.outcome {
        Outcome::Pending => lines.push(Line::styled("waiting for response", style::muted())),
        Outcome::Failed(reason) => lines.push(Line::styled(reason.clone(), style::error())),
        Outcome::Response(response) => {
            let mut summary = vec![Span::styled(
                response.status.to_string(),
                style::status(response.status),
            )];
            if let Some(duration) = entry.duration {
                summary.push(Span::styled(
                    format!(" · {}", format_duration(duration)),
                    style::muted(),
                ));
            }
            lines.push(Line::from(summary));
        }
    }
    if let Some(scenario) = &entry.scenario {
        lines.push(Line::styled(
            format!("⚠ scenario: {scenario}"),
            style::warning(),
        ));
    }

    section(&mut lines, "Request headers");
    headers(&mut lines, &entry.request_headers);
    section(&mut lines, "Request body");
    body_lines(&mut lines, &entry.request_headers, &entry.request_body);
    if let Outcome::Response(response) = &entry.outcome {
        section(&mut lines, "Response headers");
        headers(&mut lines, &response.headers);
        section(&mut lines, "Response body");
        body_lines(&mut lines, &response.headers, &response.body);
    }
    lines
}

fn section(lines: &mut Vec<Line<'static>>, title: &'static str) {
    lines.push(Line::raw(""));
    lines.push(Line::styled(title, style::heading()));
}

fn headers(lines: &mut Vec<Line<'static>>, headers: &Headers) {
    if headers.is_empty() {
        lines.push(Line::styled("(none)", style::muted()));
    }
    for (name, value) in headers {
        lines.push(Line::from(vec![
            Span::styled(format!("{name}: "), style::accent()),
            Span::raw(value.clone()),
        ]));
    }
}

fn body_lines(lines: &mut Vec<Line<'static>>, headers: &Headers, captured: &Captured) {
    let truncated = match body::view(headers, captured) {
        BodyView::Empty => {
            lines.push(Line::styled("(empty)", style::muted()));
            captured.truncated
        }
        BodyView::Binary { len } => {
            lines.push(Line::styled(
                format!("(binary, {len} bytes)"),
                style::muted(),
            ));
            captured.truncated
        }
        BodyView::Json { text, truncated } => {
            lines.extend(highlight::json(&text));
            truncated
        }
        BodyView::Text { text, truncated } => {
            lines.extend(text.lines().map(|line| Line::raw(line.to_owned())));
            truncated
        }
    };
    if truncated {
        lines.push(Line::styled("… truncated", style::warning()));
    }
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let line = if app.mode == Mode::Filter {
        Line::from(vec![
            Span::styled("/", style::accent()),
            Span::raw(app.filter.clone()),
            Span::styled("▏", style::accent().add_modifier(Modifier::SLOW_BLINK)),
        ])
    } else {
        let mut spans = Vec::new();
        if !app.filter.is_empty() {
            spans.push(Span::styled(
                format!("filter: {}  ", app.filter),
                style::accent(),
            ));
        }
        spans.push(Span::styled(
            "5/4/n/e/d/o break  x fix  / filter  Tab pane  p pause  ? help  q quit",
            style::muted(),
        ));
        Line::from(spans)
    };
    frame.render_widget(line, area);
}

const HELP: &[(&str, &str)] = &[
    ("j / k", "move, or scroll details"),
    ("g / G", "first / last"),
    ("/", "filter by path"),
    ("Tab", "switch pane"),
    ("5", "return 500"),
    ("4", "return 401 (token expired)"),
    ("n", "return 404"),
    ("e", "empty response, [] for arrays"),
    ("d", "delay the response"),
    ("o", "offline: drop the connection"),
    ("x", "remove scenarios from endpoint"),
    ("c", "clear list"),
    ("p", "pause recording"),
    ("q", "quit"),
];

fn draw_help(frame: &mut Frame) {
    let screen = frame.area();
    let height = (HELP.len() as u16 + 2).min(screen.height);
    let width = 46.min(screen.width);
    let area = Rect {
        x: (screen.width - width) / 2,
        y: (screen.height - height) / 2,
        width,
        height,
    };
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(key, text)| {
            Line::from(vec![
                Span::styled(format!(" {key:<8}"), style::accent()),
                Span::raw(*text),
            ])
        })
        .collect();
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(style::accent())
        .title(" Help ");
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block).style(Style::new()), area);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apibreak_core::store::{NewExchange, ResponseRecord};
    use apibreak_core::{ScenarioHandle, ScenarioSet, Store};
    use bytes::Bytes;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::super::keymap::{Action, Break};
    use super::*;

    fn new_app(store: Arc<Store>, title: &str) -> App {
        let scenarios = ScenarioHandle::new(ScenarioSet::default());
        App::new(
            store,
            scenarios,
            std::time::Duration::from_secs(3),
            title.into(),
        )
    }

    fn app() -> App {
        let store = Arc::new(Store::new(8));
        let mut events = store.subscribe();
        let id = store.begin(NewExchange {
            method: "GET".into(),
            path: "/api/users".into(),
            uri: "/api/users?page=1".into(),
            request_headers: vec![("accept".into(), "*/*".into())],
            request_body: Captured::default(),
            scenario: None,
        });
        let response = ResponseRecord {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: Captured::new(Bytes::from_static(br#"[{"name":"Ada"}]"#)),
        };
        store.finish(
            id,
            Outcome::Response(response),
            std::time::Duration::from_millis(12),
        );
        let mut app = new_app(store, ":4000 -> http://backend");
        while let Ok(event) = events.try_recv() {
            app.apply_event(event);
        }
        app
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal.draw(|frame| draw(frame, app)).expect("draw");
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn shows_row_and_formatted_body_at_80_columns() {
        let screen = render(&mut app(), 80, 24);
        assert!(screen.contains("GET"), "{screen}");
        assert!(screen.contains("200"), "{screen}");
        assert!(screen.contains("12ms"), "{screen}");
        assert!(screen.contains("\"name\": \"Ada\""), "{screen}");
    }

    #[test]
    fn survives_tiny_terminals_and_empty_state() {
        let mut empty = new_app(Arc::new(Store::new(1)), "");
        for (width, height) in [(1, 1), (10, 3), (20, 5), (80, 24), (200, 60)] {
            render(&mut app(), width, height);
            render(&mut empty, width, height);
            let mut help = app();
            help.mode = Mode::Help;
            render(&mut help, width, height);
        }
    }

    #[test]
    fn wide_lists_show_the_time_column() {
        let wide = list_row(&app().visible()[0].clone(), 80).to_string();
        let narrow = list_row(&app().visible()[0].clone(), 38).to_string();
        assert!(wide.len() > narrow.len());
        assert!(wide.contains(':') && !narrow.contains(':') || narrow.contains("/api"));
    }

    #[test]
    fn status_line_lists_active_scenarios() {
        let mut app = app();
        assert!(render(&mut app, 80, 24).contains("no active scenarios"));
        app.handle(Action::Break(Break::Status500));
        app.active = app.scenarios_for_test();
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("⚠ GET /api/users → 500"), "{screen}");
    }

    #[test]
    fn shorten_marks_the_cut() {
        assert_eq!(shorten("/api/users", 6), "/api/…");
        assert_eq!(shorten("/api", 6), "/api");
        assert_eq!(shorten("/api", 0), "…");
    }

    #[test]
    fn durations_are_compact() {
        assert_eq!(
            format_duration(std::time::Duration::from_millis(12)),
            "12ms"
        );
        assert_eq!(
            format_duration(std::time::Duration::from_millis(3200)),
            "3.2s"
        );
    }
}
