//! Drawing — tab bar on top, board list filling the frame, status
//! bar at the bottom. Preview mode swaps the list for the item
//! detail pane (pm's `Enter` full-pane preview). Everything degrades
//! per frame: rows truncate with `…` on narrow screens, chrome bars
//! yield to the list on short ones, and `?` folds into columns.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{Board, Mode, Model, Row};

const ACCENT: Color = Color::Indexed(69); // pm's nav blue
const MUTED: Color = Color::Indexed(241);

/// Chrome budget — which fixed rows the screen can afford, so the
/// list keeps a real viewport on short terminals. `(tabs, status)`:
/// tabs cost a row at height ≥3, the status bar only at height ≥5
/// (at 4 rows it yields to a three-row viewport).
fn chrome(height: u16) -> (bool, bool) {
    (height >= 3, height >= 5)
}

/// One full frame. Also records the tab/board geometry on `m` so
/// the next mouse event can hit-test against where rows landed.
pub fn draw(f: &mut Frame, m: &mut Model) {
    let area = f.area();
    let (show_tabs, show_status) = chrome(area.height);
    let mut constraints = Vec::with_capacity(3);
    if show_tabs {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(0));
    if show_status {
        constraints.push(Constraint::Length(1));
    }
    let chunks = Layout::vertical(constraints).split(area);

    let mut next = 0;
    if show_tabs {
        draw_tabs(f, m, chunks[next]);
        m.tabs_area = Some(chunks[next]);
        next += 1;
    } else {
        m.tabs_area = None;
    }
    match m.mode {
        Mode::Preview => draw_preview(f, m, chunks[next]),
        _ => draw_board(f, m, chunks[next]),
    }
    if show_status {
        draw_status(f, m, chunks[next + 1]);
    }
    if m.mode == Mode::Help {
        draw_help(f, area);
    }
}

/// Tab captions for `width` columns — full names when they fit,
/// initials when they don't (the `1`/`2`/`h`/`l` binds still work).
fn tab_labels(width: usize) -> Vec<String> {
    let full: Vec<String> = Board::ALL
        .iter()
        .map(|b| format!(" {} ", b.label()))
        .collect();
    // leading space + one separator per tab
    let need: usize = full.iter().map(|s| s.chars().count()).sum::<usize>() + full.len() + 1;
    if need <= width {
        full
    } else {
        Board::ALL
            .iter()
            .map(|b| b.label()[..1].to_string())
            .collect()
    }
}

fn draw_tabs(f: &mut Frame, m: &Model, area: Rect) {
    let mut spans = vec![Span::raw(" ")];
    for (b, label) in Board::ALL.iter().zip(tab_labels(area.width as usize)) {
        let style = if *b == m.board {
            Style::default().fg(Color::White).bg(ACCENT)
        } else {
            Style::default().fg(MUTED)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Click hit-test for the tab row: which tab does column `x` land
/// on in a `width`-wide bar? The leading and separator columns
/// belong to no tab. Mirrors the span layout `draw_tabs` emits.
pub fn tab_hit(width: u16, x: u16) -> Option<usize> {
    let mut col = 1u16; // leading space
    for (i, label) in tab_labels(width as usize).iter().enumerate() {
        let w = label.chars().count() as u16;
        if (col..col + w).contains(&x) {
            return Some(i);
        }
        col += w + 1; // the separator after each tab
    }
    None
}

/// Fit `spans` into `width` columns; when they overflow, keep the
/// longest prefix that leaves one column for a muted `…`. Widths are
/// char counts — wide glyphs may still let ratatui clip the tail.
fn ellipsize<'a>(spans: Vec<Span<'a>>, width: usize) -> Vec<Span<'a>> {
    let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= width {
        return spans;
    }
    if width == 0 {
        return Vec::new();
    }
    let keep = width - 1; // reserve a column for the ellipsis
    let mut out = Vec::new();
    let mut used = 0;
    for span in spans {
        let n = span.content.chars().count();
        if used + n <= keep {
            used += n;
            out.push(span);
        } else {
            let take = keep - used;
            if take > 0 {
                let cut: String = span.content.chars().take(take).collect();
                out.push(Span::styled(cut, span.style));
            }
            break;
        }
    }
    out.push(Span::styled("…", Style::default().fg(MUTED)));
    out
}

fn draw_board(f: &mut Frame, m: &mut Model, area: Rect) {
    // the highlight symbol eats two columns of every row
    let row_w = (area.width as usize).saturating_sub(2);
    let items: Vec<ListItem> = m
        .rows
        .iter()
        .map(|r| match r {
            Row::Header(name) => ListItem::new(Line::from(ellipsize(
                vec![Span::styled(
                    format!("## {name}"),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )],
                row_w,
            ))),
            Row::Item { item, depth } => {
                let indent = "  ".repeat(*depth);
                let mut spans = vec![Span::raw(indent)];
                if m.show_ids {
                    spans.push(Span::styled(
                        item.id.to_string(),
                        Style::default().fg(ACCENT),
                    ));
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::raw(item.title.clone()));
                let mut meta = format!(" {}", item.status);
                if !matches!(item.priority, arca_core::Priority::Medium) {
                    meta += &format!(" pri:{}", item.priority.token());
                }
                for l in &item.labels {
                    meta += &format!(" #{l}");
                }
                spans.push(Span::styled(meta, Style::default().fg(MUTED)));
                ListItem::new(Line::from(ellipsize(spans, row_w)))
            }
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::Indexed(238)))
        .highlight_symbol("▸ ");
    let mut state = ListState::default().with_selected(Some(m.sel));
    f.render_stateful_widget(list, area, &mut state);
    // remember where the rows landed for mouse hit-testing
    m.board_area = area;
    m.viewport_top = state.offset();
}

fn draw_preview(f: &mut Frame, m: &Model, area: Rect) {
    let Some(item) = m.selected() else {
        return;
    };
    let mut lines = vec![
        Line::from(Span::styled(
            item.id.to_string(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            item.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("status ", Style::default().fg(MUTED)),
            Span::raw(item.status.to_string()),
        ]),
        Line::from(vec![
            Span::styled("pri    ", Style::default().fg(MUTED)),
            Span::raw(item.priority.token().to_string()),
        ]),
    ];
    if let Some(o) = &item.owner {
        lines.push(Line::from(vec![
            Span::styled("owner  ", Style::default().fg(MUTED)),
            Span::raw(o.to_string()),
        ]));
    }
    if !item.labels.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("labels ", Style::default().fg(MUTED)),
            Span::raw(
                item.labels
                    .iter()
                    .map(|l| format!("#{l}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
        ]));
    }
    for (key, val) in [("epic   ", &item.epic), ("ms     ", &item.milestone)] {
        if let Some(v) = val {
            lines.push(Line::from(vec![
                Span::styled(key, Style::default().fg(MUTED)),
                Span::raw(v.clone()),
            ]));
        }
    }
    if let Some(d) = item.created {
        lines.push(Line::from(vec![
            Span::styled("created", Style::default().fg(MUTED)),
            Span::raw(format!(" {d}")),
        ]));
    }
    if !item.children.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "children",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )));
        for c in &item.children {
            lines.push(Line::from(format!("  {}", arca_ops::items::compact(c))));
        }
    }
    if let Some(note) = &item.note {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "note",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(note.clone()));
    }
    // File-backed items render their body (pm's note preview).
    if let Some(body) = m.file_body(item.id) {
        lines.push(Line::from(""));
        for l in body.lines() {
            let style = if l.starts_with('#') {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(l.to_string(), style)));
        }
    }
    // wrap rather than clip long titles/notes on narrow screens
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// Normal-mode hint sets, widest first — `pick_hints` takes the
/// first that fits so narrow terminals degrade instead of cropping.
const HINTS: [&str; 3] = [
    "j/k move · 1/2/3 boards · space done · D defer · p promote · alt+↑/↓ reorder · a add · / filter · : cmd · enter preview · E edit · R reload · ? help · q quit",
    "j/k move · space done · a add · / filter · : cmd · enter · E edit · ? help · q quit",
    "? help · q quit",
];

/// The hint line for `mode` that fits `width` columns (including the
/// leading space). The minimal set always wins as the fallback.
fn pick_hints(mode: Mode, width: usize) -> &'static str {
    match mode {
        Mode::Add => "enter add · esc cancel",
        Mode::Filter => "enter keep · esc clear",
        Mode::Command => "enter run · esc cancel",
        Mode::Preview => "esc back",
        Mode::Help => "esc/?/q close",
        Mode::Normal => HINTS
            .iter()
            .copied()
            .find(|h| h.chars().count() < width)
            .unwrap_or(HINTS[HINTS.len() - 1]),
    }
}

fn draw_status(f: &mut Frame, m: &Model, area: Rect) {
    if matches!(m.mode, Mode::Add | Mode::Filter | Mode::Command) {
        let (prompt, buf) = match m.mode {
            Mode::Add => ("add> ", &m.input),
            Mode::Filter => ("/ ", &m.filter),
            _ => (":", &m.cmdline),
        };
        let line = Line::from(vec![
            Span::styled(prompt, Style::default().fg(ACCENT)),
            Span::raw(buf.clone()),
            Span::styled("█", Style::default().fg(ACCENT)),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }
    // The status message eats columns first; hints get what remains.
    let status_w = if m.status.is_empty() {
        0
    } else {
        m.status.chars().count() + 2
    };
    let hints = pick_hints(m.mode, (area.width as usize).saturating_sub(status_w));
    let line = if m.status.is_empty() {
        Line::from(Span::styled(
            format!(" {hints}"),
            Style::default().fg(MUTED),
        ))
    } else {
        Line::from(vec![
            Span::styled(format!(" {} ", m.status), Style::default().fg(Color::White)),
            Span::styled(format!(" {hints}"), Style::default().fg(MUTED)),
        ])
    };
    f.render_widget(Paragraph::new(line), area);
}

/// A `width`×`height` rect centered in `area`, clamped to fit.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// The `?` overlay — every bind, grouped like the status bar hints.
fn help_lines() -> Vec<Line<'static>> {
    fn group(name: &'static str) -> Line<'static> {
        Line::from(Span::styled(
            name,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
    }
    fn bind(keys: &'static str, desc: &'static str) -> Line<'static> {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{keys:<12}"), Style::default().fg(ACCENT)),
            Span::styled(desc, Style::default().fg(MUTED)),
        ])
    }
    // No blank separators — the whole list must fit a 24-row screen.
    vec![
        group("nav"),
        bind("j/k, ↑/↓", "move"),
        bind("gg / G", "first / last"),
        bind("enter", "preview item"),
        group("tabs"),
        bind("1 / 2", "TODO / BACKLOG"),
        bind("h/l, ←/→", "switch board"),
        group("items"),
        bind("space, dd", "mark done"),
        bind("D", "defer → BACKLOG"),
        bind("p", "promote → TODO"),
        bind("alt+j/k, ↑/↓", "reorder"),
        group("edit"),
        bind("a", "add item"),
        bind("E", "open in $EDITOR"),
        bind("R", "reload"),
        group("mouse"),
        bind("click, wheel", "select, scroll"),
        group("misc"),
        bind(": / ?", "command line / help"),
        bind("q, ctrl+c", "quit"),
        Line::from(Span::styled(
            "esc / ? / q to close",
            Style::default().fg(MUTED),
        )),
    ]
}

/// Break `lines` into side-by-side columns: as many as the screen
/// height needs (one when everything fits vertically), capped by
/// what the width affords. `inner` is the box content area.
fn help_columns(lines: &[Line<'static>], area: Rect) -> Vec<Vec<Line<'static>>> {
    let inner_h = area.height.saturating_sub(2).max(1) as usize;
    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    let col_w = lines.iter().map(Line::width).max().unwrap_or(0) + 3;
    let fits = (inner_w / col_w.max(1)).max(1); // columns the width allows
    let needs = lines.len().div_ceil(inner_h).max(1); // columns the height wants
    let n = fits.min(needs);
    let per = lines.len().div_ceil(n);
    lines.chunks(per).map(<[Line]>::to_vec).collect()
}

fn draw_help(f: &mut Frame, area: Rect) {
    let cols = help_columns(&help_lines(), area);
    let col_ws: Vec<u16> = cols
        .iter()
        .map(|c| c.iter().map(Line::width).max().unwrap_or(0) as u16 + 3)
        .collect();
    let width = col_ws.iter().sum::<u16>() + 2;
    let height = cols.iter().map(Vec::len).max().unwrap_or(0) as u16 + 2;
    let popup = centered(area, width, height);
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(" keys ", Style::default().fg(ACCENT)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let rects = Layout::horizontal(col_ws.iter().map(|w| Constraint::Length(*w))).split(inner);
    for (lines, rect) in cols.iter().zip(rects.iter()) {
        f.render_widget(Paragraph::new(lines.clone()), *rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rendered text of a span set, for truncation assertions.
    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn chrome_yields_to_the_viewport() {
        assert_eq!(chrome(24), (true, true));
        assert_eq!(chrome(5), (true, true));
        assert_eq!(chrome(4), (true, false)); // status yields: 3-row list
        assert_eq!(chrome(3), (true, false));
        assert_eq!(chrome(2), (false, false)); // everything yields
    }

    #[test]
    fn tab_labels_shrink_to_initials() {
        assert_eq!(tab_labels(80), vec![" TODO ", " BACKLOG ", " ITEMS "]);
        assert_eq!(tab_labels(26), vec![" TODO ", " BACKLOG ", " ITEMS "]);
        assert_eq!(tab_labels(25), vec!["T", "B", "I"]);
        assert_eq!(tab_labels(2), vec!["T", "B", "I"]);
    }

    #[test]
    fn tab_hit_matches_drawn_columns() {
        // full labels: " " + " TODO " + " " + " BACKLOG " + " " + " ITEMS "
        assert_eq!(tab_hit(80, 0), None); // leading separator
        assert_eq!(tab_hit(80, 1), Some(0));
        assert_eq!(tab_hit(80, 6), Some(0));
        assert_eq!(tab_hit(80, 7), None); // separator between tabs
        assert_eq!(tab_hit(80, 8), Some(1));
        assert_eq!(tab_hit(80, 16), Some(1));
        assert_eq!(tab_hit(80, 18), Some(2));
        assert_eq!(tab_hit(80, 24), Some(2));
        assert_eq!(tab_hit(80, 40), None); // past the last tab
                                           // narrow terminal: initials "T"/"B"/"I" at cols 1, 3, 5
        assert_eq!(tab_hit(10, 1), Some(0));
        assert_eq!(tab_hit(10, 3), Some(1));
        assert_eq!(tab_hit(10, 5), Some(2));
    }

    #[test]
    fn ellipsize_cuts_with_ellipsis() {
        let spans = || vec![Span::raw("hello world")];
        assert_eq!(text(&ellipsize(spans(), 20)), "hello world");
        assert_eq!(text(&ellipsize(spans(), 11)), "hello world");
        assert_eq!(text(&ellipsize(spans(), 8)), "hello w…");
        assert_eq!(text(&ellipsize(spans(), 1)), "…");
        assert!(ellipsize(spans(), 0).is_empty());
        // styles survive the cut: an accent id keeps its color
        let out = ellipsize(
            vec![
                Span::styled("T-001", Style::default().fg(ACCENT)),
                Span::raw(" a long title here"),
            ],
            10,
        );
        assert_eq!(text(&out), "T-001 a l…");
        assert_eq!(out[0].style.fg, Some(ACCENT));
    }

    #[test]
    fn help_columns_only_when_short_and_wide() {
        let lines = help_lines();
        // plenty of room: one column
        assert_eq!(help_columns(&lines, Rect::new(0, 0, 80, 24)).len(), 1);
        // short but narrow: stays one column, crops
        assert_eq!(help_columns(&lines, Rect::new(0, 0, 30, 10)).len(), 1);
        // short and wide: folds into columns that fit the height
        let cols = help_columns(&lines, Rect::new(0, 0, 120, 10));
        assert!(cols.len() >= 2);
        assert!(cols.iter().all(|c| c.len() <= 8));
        assert_eq!(cols.iter().map(Vec::len).sum::<usize>(), lines.len());
    }

    #[test]
    fn hints_degrade_full_to_minimal() {
        let len = |h: &str| h.chars().count();
        assert_eq!(pick_hints(Mode::Normal, len(HINTS[0]) + 1), HINTS[0]);
        assert_eq!(pick_hints(Mode::Normal, len(HINTS[0])), HINTS[1]);
        assert_eq!(pick_hints(Mode::Normal, len(HINTS[1])), HINTS[2]);
        // tiny terminals still get the minimal set (cropped, not absent)
        assert_eq!(pick_hints(Mode::Normal, 0), HINTS[2]);
        assert_eq!(pick_hints(Mode::Preview, 0), "esc back");
    }
}
