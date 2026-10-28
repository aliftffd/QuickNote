use crate::app::{AiStatus, App, Mode, Status};
use crate::editor::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

const EMPTY_EDITOR: &str = "No notes yet. Start typing. · Ctrl+L notes · Ctrl+F search · Ctrl+N new · Ctrl+K category · Esc quit";
const CONFLICT: &str = "Conflict: r reload · o overwrite · n save as new";
const QUIT_CONFIRM: &str = "Quit without saving? Q confirms · Esc cancels";
const LIST_HINT: &str = "Up/Down select · Enter open/import · Esc close";
const SEARCH_HINT: &str = "Up/Down select · Enter open · Esc close";

/// Draw without mutating the note or UI state. Viewports are derived from the
/// focused cursor/selection, so resizing also keeps the focus in view.
pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    if area.is_empty() {
        return;
    }
    let body = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
    let footer = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    f.render_widget(Clear, area);
    let padding = u16::from(body.width > 2);
    let editor = Rect::new(
        body.x + padding,
        body.y,
        body.width.saturating_sub(2 * padding),
        body.height,
    );
    if app.buf.text().is_empty() {
        f.render_widget(
            Paragraph::new(EMPTY_EDITOR).wrap(Wrap { trim: false }),
            editor,
        );
        if matches!(app.mode, Mode::Edit) && !editor.is_empty() {
            f.set_cursor_position((editor.x, editor.y));
        }
    } else {
        editor_view(f, editor, &app.buf, matches!(app.mode, Mode::Edit));
    }
    draw_footer(f, footer, app);
    match &app.mode {
        Mode::List { sel } => draw_list(f, body, app, *sel),
        Mode::Search { query, sel } => draw_search(f, body, app, query, *sel),
        Mode::CategoryPrompt { input } => draw_category(f, body, input),
        _ => {}
    }
}

fn width(text: &str) -> usize {
    Span::raw(text).width()
}

// All UI values stay on their assigned row, including pasted category names,
// filenames and error messages. A literal editor tab always occupies four cells.
fn display(text: &str) -> String {
    text.chars().fold(String::new(), |mut out, ch| {
        match ch {
            '\t' => out.push_str("    "),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
        out
    })
}

/// Crop on grapheme boundaries; a wide glyph intersecting an edge becomes
/// spaces so that subsequent text and the terminal cursor remain aligned.
fn window(text: &str, start: usize, cells: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    let end = start.saturating_add(cells);
    let span = Span::raw(text);
    for g in span.styled_graphemes(Style::default()) {
        let next = col + width(g.symbol);
        if col >= end {
            break;
        }
        if next > start {
            if col >= start && next <= end {
                out.push_str(g.symbol);
            } else {
                out.push_str(&" ".repeat(next.min(end) - col.max(start)));
            }
        }
        col = next;
    }
    out
}

fn clip(text: &str, cells: usize) -> String {
    if width(text) <= cells {
        text.to_owned()
    } else if cells == 0 {
        String::new()
    } else {
        format!("{}…", window(text, 0, cells - 1))
    }
}

fn editor_view(f: &mut Frame, area: Rect, buf: &Buffer, focused: bool) {
    if area.is_empty() {
        return;
    }
    let before = &buf.text()[..buf.cursor()];
    let row = before.bytes().filter(|b| *b == b'\n').count();
    let col = width(&display(before.rsplit('\n').next().unwrap_or_default()));
    let top = row.saturating_sub(usize::from(area.height) - 1);
    let left = col.saturating_sub(usize::from(area.width) - 1);
    for (y, line) in buf
        .text()
        .split('\n')
        .skip(top)
        .take(area.height.into())
        .enumerate()
    {
        let text = window(&display(line), left, area.width.into());
        row_text(f, area, y as u16, text, Style::default());
    }
    if focused {
        f.set_cursor_position((area.x + (col - left) as u16, area.y + (row - top) as u16));
    }
}

fn accent(color: Color, no_color: bool) -> Style {
    let style = Style::default().add_modifier(Modifier::BOLD);
    if no_color {
        style
    } else {
        style.fg(color)
    }
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let cells = usize::from(area.width);
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let status = app.status();
    let (state, label, color) = match &app.mode {
        Mode::Conflict => {
            let text = if width(CONFLICT) <= cells {
                CONFLICT
            } else {
                "Conflict: r · o · n"
            };
            (text.to_owned(), "Conflict", Color::Magenta)
        }
        Mode::QuitConfirm => {
            let text = if width(QUIT_CONFIRM) <= cells {
                QUIT_CONFIRM
            } else {
                "Quit without saving? Q · Esc"
            };
            (text.to_owned(), "Quit", Color::Red)
        }
        Mode::QuitError => {
            let reason = match &status {
                Status::Error(reason) => display(reason),
                _ => String::new(),
            };
            let actions = " — e export · Q quit without saving";
            let text = if cells >= width("Error: ") + width(actions) {
                format!(
                    "Error: {}{}",
                    clip(&reason, cells - width("Error: ") - width(actions)),
                    actions
                )
            } else {
                "Error: e · Q".to_owned()
            };
            (text, "Error", Color::Red)
        }
        _ => match status {
            Status::Unsaved => ("Unsaved".to_owned(), "Unsaved", Color::Yellow),
            Status::Saving => ("Saving".to_owned(), "Saving", Color::Cyan),
            Status::Saved => ("Saved".to_owned(), "Saved", Color::Green),
            Status::Conflict => ("Conflict".to_owned(), "Conflict", Color::Magenta),
            Status::Error(reason) => (
                format!(
                    "Error: {}",
                    clip(&display(&reason), cells.saturating_sub(width("Error: ")))
                ),
                "Error",
                Color::Red,
            ),
        },
    };
    let mut tail = String::new();
    if let Some(message) = app.message.as_ref().filter(|s| !s.is_empty()) {
        tail.push_str(" · ");
        tail.push_str(&display(message));
    }
    let message = tail.clone();
    if let AiStatus::Paused(reason) = &app.ai_status {
        tail.push_str(&format!(" · auto-categorize: paused ({})", display(reason)));
    }
    // Only the optional tail is clipped. Error reasons are shortened above,
    // reserving the entire action bar before allocating any diagnostic space.
    let tail = clip(&tail, cells.saturating_sub(width(&state)));
    let (message, ai) = if tail.starts_with(&message) {
        tail.split_at(message.len())
    } else {
        (tail.as_str(), "")
    };
    let ai_style = if no_color {
        Style::default()
    } else {
        Style::default().fg(Color::Yellow)
    };
    let line = Line::from(vec![
        Span::styled(label.to_owned(), accent(color, no_color)),
        Span::raw(state[label.len()..].to_owned()),
        Span::raw(message),
        Span::styled(ai, ai_style),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn row_text(f: &mut Frame, area: Rect, y: u16, text: String, style: Style) {
    if y < area.height && area.width > 0 {
        f.render_widget(
            Paragraph::new(text).style(style),
            Rect::new(area.x, area.y + y, area.width, 1),
        );
    }
}

fn panel(f: &mut Frame, body: Rect, width: u16, height: u16, title: &str) -> Rect {
    let w = width.min(body.width.saturating_sub(8).max(body.width.min(3)));
    let h = height.min(body.height);
    let area = Rect::new(
        body.x + (body.width - w) / 2,
        body.y + (body.height - h) / 2,
        w,
        h,
    );
    let border = ratatui::symbols::border::Set {
        top_left: "+",
        top_right: "+",
        bottom_left: "+",
        bottom_right: "+",
        vertical_left: "|",
        vertical_right: "|",
        horizontal_top: "-",
        horizontal_bottom: "-",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border)
        .title(format!(" {title} "));
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    inner
}

fn selection_style(selected: bool) -> Style {
    if selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    }
}

fn draw_list(f: &mut Frame, body: Rect, app: &App, sel: usize) {
    let inner = panel(f, body, 76, 18, "Notes");
    if inner.is_empty() {
        return;
    }
    let content_height = inner.height.saturating_sub(1) as usize;
    let cells = usize::from(inner.width);
    let mut rows: Vec<(String, Option<usize>)> = Vec::new();
    if !app.recovered.is_empty() {
        rows.push(("Recovered locally".to_owned(), None));
        for (index, path) in app.recovered.iter().enumerate() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            rows.push((
                format!(
                    "{} {}",
                    if sel == index { ">" } else { " " },
                    clip(&display(&name), cells.saturating_sub(2))
                ),
                Some(index),
            ));
        }
    }
    // The database supplies newest-first order within each group. Stable sorting
    // groups preserves it, and original indices preserve App's Enter mapping.
    let mut items: Vec<_> = app.list.iter().enumerate().collect();
    items.sort_by(|(_, a), (_, b)| {
        (a.group == "Uncategorized")
            .cmp(&(b.group == "Uncategorized"))
            .then(a.group.cmp(&b.group))
    });
    let mut group = None;
    for (index, item) in items {
        if group != Some(item.group.as_str()) {
            if !rows.is_empty() {
                rows.push((String::new(), None));
            }
            rows.push((clip(&display(&item.group), cells), None));
            group = Some(&item.group);
        }
        let index = app.recovered.len() + index;
        let available = cells.saturating_sub(2);
        let title = display(&item.title);
        let text = if item.stale {
            let suffix = " (stale)";
            let label_width = (available / 2).max(width(suffix)).min(available);
            let label = format!(
                "{}{}",
                clip(
                    &display(&item.group),
                    label_width.saturating_sub(width(suffix))
                ),
                suffix
            );
            let title = clip(&title, available.saturating_sub(width(&label) + 1));
            format!(
                "{}{}{}",
                title,
                " ".repeat(available.saturating_sub(width(&title) + width(&label))),
                label
            )
        } else {
            clip(&title, available)
        };
        rows.push((
            format!("{} {}", if sel == index { ">" } else { " " }, text),
            Some(index),
        ));
    }
    if rows.is_empty() {
        rows.push(("No notes yet. Esc returns to the editor.".to_owned(), None));
    }
    let selected_row = rows
        .iter()
        .position(|(_, index)| *index == Some(sel))
        .unwrap_or(0);
    let top = selected_row.saturating_sub(content_height.saturating_sub(1));
    for (y, (text, index)) in rows.into_iter().skip(top).take(content_height).enumerate() {
        row_text(
            f,
            inner,
            y as u16,
            text,
            selection_style(index == Some(sel)),
        );
    }
    row_text(
        f,
        inner,
        inner.height - 1,
        clip(LIST_HINT, cells),
        Style::default(),
    );
}

fn input_line(f: &mut Frame, area: Rect, prefix: &str, input: &Buffer) {
    if area.is_empty() {
        return;
    }
    let prefix_width = width(prefix).min(usize::from(area.width.saturating_sub(1)));
    row_text(
        f,
        area,
        0,
        window(prefix, 0, prefix_width),
        Style::default(),
    );
    let field = Rect::new(
        area.x + prefix_width as u16,
        area.y,
        area.width - prefix_width as u16,
        1,
    );
    let col = width(&display(&input.text()[..input.cursor()]));
    let left = col.saturating_sub(usize::from(field.width) - 1);
    row_text(
        f,
        field,
        0,
        window(&display(input.text()), left, field.width.into()),
        Style::default(),
    );
    f.set_cursor_position((field.x + (col - left) as u16, field.y));
}

fn snippet_lines(snippet: &str, cells: usize) -> Vec<String> {
    if cells == 0 {
        return vec![String::new()];
    }
    let text = display(snippet);
    let span = Span::raw(&text);
    let mut lines = vec![String::new()];
    let mut used = 0;
    for g in span.styled_graphemes(Style::default()) {
        let w = width(g.symbol);
        if used + w > cells {
            if lines.len() == 2 {
                let last = lines.last_mut().unwrap();
                *last = format!("{}…", window(last, 0, cells - 1));
                break;
            }
            lines.push(String::new());
            used = 0;
        }
        lines.last_mut().unwrap().push_str(g.symbol);
        used += w;
    }
    lines
}

fn draw_search(f: &mut Frame, body: Rect, app: &App, query: &Buffer, sel: usize) {
    let inner = panel(f, body, 76, 14, "Search");
    if inner.is_empty() {
        return;
    }
    input_line(f, Rect::new(inner.x, inner.y, inner.width, 1), "> ", query);
    row_text(
        f,
        inner,
        1,
        "Searches saved content only".to_owned(),
        Style::default(),
    );
    let content_height = inner.height.saturating_sub(4) as usize;
    let mut rows = Vec::new();
    let mut selected_start = 0;
    let mut selected_end = 0;
    if query.text().trim().is_empty() {
        rows.push(("Type to search saved notes.".to_owned(), false));
    } else if app.results.is_empty() {
        rows.push(("No matches in saved notes.".to_owned(), false));
    } else {
        for (index, (_, snippet)) in app.results.iter().take(50).enumerate() {
            if index == sel {
                selected_start = rows.len();
            }
            for (line, text) in snippet_lines(snippet, inner.width.saturating_sub(2).into())
                .into_iter()
                .enumerate()
            {
                rows.push((
                    format!(
                        "{} {}",
                        if index == sel && line == 0 { ">" } else { " " },
                        text
                    ),
                    index == sel,
                ));
            }
            if index == sel {
                selected_end = rows.len();
            }
        }
    }
    let top = selected_end
        .saturating_sub(content_height)
        .min(selected_start);
    for (y, (text, selected)) in rows.into_iter().skip(top).take(content_height).enumerate() {
        row_text(f, inner, y as u16 + 3, text, selection_style(selected));
    }
    // At very small heights keep the focused input visible ahead of help text.
    if inner.height > 2 {
        row_text(
            f,
            inner,
            inner.height - 1,
            clip(SEARCH_HINT, inner.width.into()),
            Style::default(),
        );
    }
}

fn draw_category(f: &mut Frame, body: Rect, input: &Buffer) {
    let inner = panel(f, body, 64, 7, "Category");
    if inner.is_empty() {
        return;
    }
    input_line(
        f,
        Rect::new(inner.x, inner.y, inner.width, 1),
        "Category: ",
        input,
    );
    row_text(
        f,
        inner,
        2,
        "Enter applies · Esc cancels".to_owned(),
        Style::default(),
    );
    row_text(
        f,
        inner,
        3,
        "Empty input clears manual category".to_owned(),
        Style::default(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::{Backend, TestBackend};
    use ratatui::Terminal;

    fn screen(term: &Terminal<TestBackend>) -> String {
        let buffer = term.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn status_bar_shows_conflict_and_ai_paused() {
        let (_d, _p, mut a, _rx) = crate::app::tests::app_p();
        a.mode = Mode::Conflict;
        a.ai_status = AiStatus::Paused("no model yet, run quicknote train".into());
        // The plan's two full strings require 104 cells. The binding Decisions
        // require a single row, so use 120 for the full-copy assertion.
        let mut term = Terminal::new(TestBackend::new(120, 5)).unwrap();
        term.draw(|f| draw(f, &a)).unwrap();
        let s = screen(&term);
        assert!(s.contains(CONFLICT));
        assert!(s.contains("auto-categorize: paused (no model yet, run quicknote train)"));

        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, &a)).unwrap();
        let s = screen(&term);
        let footer = s.lines().last().unwrap();
        assert!(footer.starts_with(CONFLICT));
        assert!(footer.ends_with('…'));
        assert!(!s
            .lines()
            .take(23)
            .any(|line| line.contains("auto-categorize")));
    }

    #[test]
    fn empty_note_renders_placeholder_hint() {
        let (_d, _p, a, _rx) = crate::app::tests::app_p();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, &a)).unwrap();
        let s = screen(&term);
        let normalized = s.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(normalized.contains(EMPTY_EDITOR));
        assert_eq!(a.buf.text(), "");
        assert_eq!(
            term.backend_mut().get_cursor_position().unwrap(),
            (1, 0).into()
        );
    }

    #[test]
    fn editor_scrolls_by_display_cells_and_keeps_cursor_visible() {
        let (_d, _p, mut a, _rx) = crate::app::tests::app_p();
        let text = format!("{}界\te\u{301}{}", "line\n".repeat(30), "x".repeat(90));
        a.buf = Buffer::new(text.clone(), text.len());
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, &a)).unwrap();
        assert_eq!(
            term.backend_mut().get_cursor_position().unwrap(),
            (78, 22).into()
        );
        assert!(screen(&term)
            .lines()
            .nth(22)
            .unwrap()
            .contains(&"x".repeat(77)));
        a.buf = Buffer::new("界\te\u{301}".into(), "界\te\u{301}".len());
        term.draw(|f| draw(f, &a)).unwrap();
        assert_eq!(
            term.backend_mut().get_cursor_position().unwrap(),
            (8, 0).into()
        );
        assert!(screen(&term).starts_with(" 界     e\u{301}"));
    }

    #[test]
    fn quit_error_reserves_actions_and_messages_precede_ai() {
        let (_d, _p, mut a, _rx) = crate::app::tests::app_p();
        a.on_reply(
            crate::save::WriteReply::Error {
                note_id: Some(a.note_id),
                msg: "界".repeat(100),
            },
            std::time::Instant::now(),
        );
        a.mode = Mode::QuitError;
        a.message = Some("Exported to /tmp/note.txt".into());
        a.ai_status = AiStatus::Paused("offline".into());
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, &a)).unwrap();
        let s = screen(&term);
        let footer = s.lines().last().unwrap();
        assert!(footer.starts_with("Error: "));
        assert!(footer.ends_with("… — e export · Q quit without saving"));
        a.mode = Mode::QuitConfirm;
        term.draw(|f| draw(f, &a)).unwrap();
        let s = screen(&term);
        let footer = s.lines().last().unwrap();
        assert!(footer.starts_with(QUIT_CONFIRM));
        assert!(footer.contains("Exported to /tmp/note.txt"));
    }

    #[test]
    fn overlays_show_recovery_stale_labels_and_focused_inputs() {
        let (_d, _p, mut a, _rx) = crate::app::tests::app_p();
        a.recovered.push("/tmp/recovery/rescued.txt".into());
        a.list.push(crate::db::ListItem {
            id: 2,
            title: "Long title ".repeat(10),
            group: "Work".into(),
            stale: true,
        });
        a.mode = Mode::List { sel: 1 };
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, &a)).unwrap();
        let s = screen(&term);
        assert!(s.find("Recovered locally").unwrap() < s.find("Work").unwrap());
        assert!(s.contains("rescued.txt") && !s.contains("/tmp/recovery"));
        assert!(s.contains("Work (stale)") && s.contains(LIST_HINT));
        assert!(s.contains("> Long title"));
        a.results = (0..30)
            .map(|i| (i, format!("result {i} [match] {}", "snippet ".repeat(20))))
            .collect();
        a.mode = Mode::Search {
            query: Buffer::new("界\tfind".into(), "界\tfind".len()),
            sel: 29,
        };
        term.draw(|f| draw(f, &a)).unwrap();
        let s = screen(&term);
        assert!(s.contains("> result 29 [match]"));
        assert!(s.contains("Searches saved content only") && s.contains(SEARCH_HINT));
        assert_eq!(
            term.backend_mut().get_cursor_position().unwrap(),
            (17, 5).into()
        );
        a.mode = Mode::CategoryPrompt {
            input: Buffer::new("Work".into(), 4),
        };
        term.draw(|f| draw(f, &a)).unwrap();
        let s = screen(&term);
        assert!(s.contains("Category: Work"));
        assert!(s.contains("Enter applies · Esc cancels"));
        assert!(s.contains("Empty input clears manual category"));
    }

    #[test]
    fn tiny_terminals_and_monochrome_styles_are_safe() {
        let (_d, _p, mut a, _rx) = crate::app::tests::app_p();
        for mode in [
            Mode::Edit,
            Mode::Conflict,
            Mode::QuitError,
            Mode::QuitConfirm,
            Mode::List { sel: 0 },
            Mode::Search {
                query: Buffer::new("界".into(), 3),
                sel: 0,
            },
            Mode::CategoryPrompt {
                input: Buffer::new("x".into(), 1),
            },
        ] {
            a.mode = mode;
            for (w, h) in [(0, 0), (1, 1), (2, 3), (10, 4), (80, 24)] {
                let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
                term.draw(|f| draw(f, &a)).unwrap();
            }
        }
        assert_eq!(accent(Color::Red, true).fg, None);
        assert!(accent(Color::Red, true)
            .add_modifier
            .contains(Modifier::BOLD));
        assert_eq!(accent(Color::Red, false).fg, Some(Color::Red));
        a.mode = Mode::Edit;
        a.ai_status = AiStatus::Paused("offline".into());
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, &a)).unwrap();
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let buffer = term.backend().buffer();
        assert_eq!(
            buffer[(0, 23)].fg,
            if no_color { Color::Reset } else { Color::Green }
        );
        assert_eq!(
            buffer[(8, 23)].fg,
            if no_color {
                Color::Reset
            } else {
                Color::Yellow
            }
        );
        if no_color {
            assert!(buffer
                .content
                .iter()
                .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset));
        }
        for status in [AiStatus::Idle, AiStatus::Working] {
            a.ai_status = status;
            term.draw(|f| draw(f, &a)).unwrap();
            assert!(!screen(&term).contains("auto-categorize"));
        }
    }
}
