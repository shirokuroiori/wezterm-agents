//! Rendering via ratatui (see spec §4.2 for the layout).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, LayoutMode, Row};
use crate::memo;
use crate::model::*;

/// Width threshold below which the detail pane collapses (spec §4.2).
pub const NARROW_COLS: u16 = 100;

/// LIST column widths. The header and each row share the same constants, so they
/// can't drift apart (this prevents the mistake of adjusting only the header and
/// having the rows not follow).
/// STATUS column = icon(1) + space(1) + badge(9) = 11.
const COL_STATUS: usize = 11;
const COL_UNREAD: usize = 3;
const COL_WS: usize = 9;
const COL_WIN: usize = 4;
const COL_TAB: usize = 4;
const COL_PANE: usize = 5;
const COL_AGENT: usize = 8;
/// BRANCH width used only in the layout that shows the TASK column
/// (LayoutMode::ListFull / narrow terminal). In the normal layout, BRANCH is the
/// final, flexible column, but in TASK-column mode that role goes to TASK instead,
/// so BRANCH gets a fixed width here.
const COL_BRANCH: usize = 18;
/// Minimum inner LIST width at which BRANCH can still be shown alongside TASK.
/// Below this, drop BRANCH and keep TASK as the priority.
const TASKVIEW_BRANCH_MIN: u16 = 74;

/// Truncates to exactly the display width, then pads the right with spaces.
///
/// Don't pad by character count (`{:<n}`) — CJK characters in workspace or branch
/// names take up 2 columns each, which breaks column alignment. This actually broke
/// in the bash version; the history is recorded in docs/wezterm-ai-agent-ideas.md.
pub fn fit(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let total = s.width();
    let mut out = String::new();
    let mut w = 0;
    if total > width {
        // Leave one column free at the end for the … we're about to add.
        for c in s.chars() {
            let cw = UnicodeWidthChar::width(c).unwrap_or(0);
            if w + cw > width - 1 {
                break;
            }
            out.push(c);
            w += cw;
        }
        out.push('…');
        w += 1;
    } else {
        out.push_str(s);
        w = total;
    }
    out.push_str(&" ".repeat(width - w));
    out
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);

    let narrow = area.width < NARROW_COLS;
    let body = chunks[0];
    match app.layout {
        LayoutMode::DetailFull => draw_detail(f, body, app),
        LayoutMode::ListFull => draw_list(f, body, app, true),
        // A narrow terminal can't split into two panes, so Split falls back to
        // "list only (with the TASK column)". narrow doesn't route through
        // cycle_layout, but Split is the default right after startup, so the
        // draw side has to handle it too.
        LayoutMode::Split if narrow => draw_list(f, body, app, true),
        LayoutMode::Split => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                .split(body);
            draw_list(f, cols[0], app, false);
            draw_detail(f, cols[1], app);
        }
    }
    draw_footer(f, chunks[1], app);
}

/// Always renders with a double border for a "glow" look. The color is the
/// panel-specific one the caller passes in (LIST=COLOR_ACCENT, DETAIL=COLOR_DETAIL).
fn glow_block(title: &str, color: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(color))
        .title(Span::styled(format!(" {title} "), Style::default().fg(color).add_modifier(Modifier::BOLD)))
}

/// When `task_view` = true, drop the three numeric columns (WIN/TAB/PANE) and show
/// the TASK column instead (LayoutMode::ListFull / narrow terminal). When false, use
/// the normal layout that sits side by side with DETAIL.
fn draw_list(f: &mut Frame, area: Rect, app: &App, task_view: bool) {
    let block = glow_block("LIST", COLOR_ACCENT);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.rows.is_empty() {
        let msg = if app.snapshot.is_empty() {
            "対象のペインがありません"
        } else {
            "絞り込みに一致するペインがありません"
        };
        f.render_widget(Paragraph::new(msg).style(Style::default().fg(COLOR_DIM)), inner);
        return;
    }

    // Column widths in TASK-column mode are derived from inner.width and are shared
    // by every row.
    // prefix(12) = bar(1) + icon(1) + space-after-icon(1) + badge(9).
    // The separating spaces inside rest_text number 5 with BRANCH, 4 without.
    let show_branch = task_view && inner.width >= TASKVIEW_BRANCH_MIN;
    let branch_w = if show_branch { COL_BRANCH } else { 0 };
    let task_w = {
        let seps = if show_branch { 5 } else { 4 };
        let fixed = 12 + seps + COL_UNREAD + COL_WS + COL_AGENT + branch_w;
        (inner.width as usize).saturating_sub(fixed).max(8)
    };

    // Header row. Its column widths share the COL_* constants above (and task_w)
    // with row rendering, so there's no way for only the header to get adjusted
    // while rows don't follow.
    let header = if task_view && show_branch {
        format!(
            " {} {} {} {} {} {}",
            fit("", COL_STATUS),
            fit("", COL_UNREAD),
            fit("WS", COL_WS),
            fit("AGENT", COL_AGENT),
            fit("BRANCH", branch_w),
            fit("TASK", task_w),
        )
    } else if task_view {
        format!(
            " {} {} {} {} {}",
            fit("", COL_STATUS),
            fit("", COL_UNREAD),
            fit("WS", COL_WS),
            fit("AGENT", COL_AGENT),
            fit("TASK", task_w),
        )
    } else {
        format!(
            " {} {} {} {} {} {} {} {}",
            fit("", COL_STATUS),
            fit("", COL_UNREAD),
            fit("WS", COL_WS),
            fit("WIN", COL_WIN),
            fit("TAB", COL_TAB),
            fit("PANE", COL_PANE),
            fit("AGENT", COL_AGENT),
            "BRANCH",
        )
    };
    let header_line = Line::from(Span::styled(
        header,
        Style::default().fg(COLOR_LABEL).add_modifier(Modifier::BOLD),
    ));
    f.render_widget(Paragraph::new(vec![header_line]), Rect { height: 1, ..inner });
    let inner = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };

    let height = inner.height as usize;
    let offset = app.scroll_offset(height);
    let mut lines: Vec<Line> = Vec::new();

    for (idx, row) in app.rows.iter().enumerate().skip(offset).take(height) {
        match row {
            Row::Blank => {
                lines.push(Line::from(""));
            }
            Row::Header { group } => {
                let g = &app.snapshot.groups[*group];
                let unread = g.unread();
                let mut spans = vec![Span::styled(
                    g.label().to_string(),
                    Style::default().fg(COLOR_FG).add_modifier(Modifier::BOLD),
                )];
                if unread > 0 {
                    spans.push(Span::styled(
                        format!("  ●{unread}"),
                        Style::default().fg(COLOR_ACCENT).add_modifier(Modifier::BOLD),
                    ));
                }
                lines.push(Line::from(spans));
            }
            Row::Pane { group, pane } => {
                let p = &app.snapshot.groups[*group].panes[*pane];
                let selected = idx == app.cursor;
                // Paint the cursor row with voltwave's CursorLine color. Only the
                // part from the STATUS column onward (through the text, to the
                // end of the row) gets it — not the cursor (▶), the icon, or the
                // space right after it (the rest of the row's width is filled in
                // by the trailing padding span).
                let row_bg = if selected { Some(COLOR_CURSOR_LINE) } else { None };
                let with_row_bg = |mut style: Style| {
                    if let Some(bg) = row_bg {
                        style = style.bg(bg);
                    }
                    style
                };
                // The cursor row is marked with an 8-bit-game-style menu cursor
                // (▶). Unread rows get an accent-colored bar on the left edge
                // (spec §4.2). CursorLine painting only applies from the STATUS
                // column onward, so the cursor itself never gets row_bg.
                let bar = if selected {
                    Span::styled("▶", Style::default().fg(COLOR_ACCENT))
                } else if p.unread > 0 {
                    Span::styled("▎", Style::default().fg(COLOR_ACCENT))
                } else {
                    Span::raw(" ")
                };
                let unread = if p.unread > 0 {
                    format!("●{:<2}", p.unread)
                } else {
                    "   ".to_string()
                };
                let mut rest = with_row_bg(Style::default());
                if p.unread > 0 || selected {
                    rest = rest.add_modifier(Modifier::BOLD);
                }
                // Each value gets its own column. Alignment uses display width, not
                // character count (see fit). In TASK-column mode, WIN/TAB/PANE are
                // dropped and the freed-up width goes to TASK (the trailing,
                // flexible column).
                let agent = p.agent.as_deref().unwrap_or("-");
                let branch = p.branch.as_deref().unwrap_or("-");
                let rest_text = if task_view && show_branch {
                    format!(
                        " {} {} {} {} {}",
                        fit(&unread, COL_UNREAD),
                        fit(&p.workspace, COL_WS),
                        fit(agent, COL_AGENT),
                        fit(branch, branch_w),
                        fit(&p.task, task_w),
                    )
                } else if task_view {
                    format!(
                        " {} {} {} {}",
                        fit(&unread, COL_UNREAD),
                        fit(&p.workspace, COL_WS),
                        fit(agent, COL_AGENT),
                        fit(&p.task, task_w),
                    )
                } else {
                    format!(
                        " {} {} {} {} {} {} {}",
                        fit(&unread, COL_UNREAD),
                        fit(&p.workspace, COL_WS),
                        fit(&p.window_id.to_string(), COL_WIN),
                        fit(&p.tab_id.to_string(), COL_TAB),
                        fit(&p.pane_id.to_string(), COL_PANE),
                        fit(agent, COL_AGENT),
                        branch,
                    )
                };
                let mut spans = vec![
                    bar,
                    // The cursor, icon, and the space right after the icon are
                    // excluded from row highlighting (everything before the
                    // STATUS column). row_bg is not applied up to this point.
                    Span::styled(
                        p.state.icon().to_string(),
                        Style::default().fg(p.state.color()),
                    ),
                    Span::raw(" "),
                    // Status is shown as a filled-background badge. Color alone
                    // doesn't distinguish waiting/done/working clearly enough.
                    // The text uses state.color()'s vivid color; the background
                    // uses its darker tone (bg_color()). Not overridden by the
                    // cursor row's CursorLine (if the badge got buried in it,
                    // you couldn't read the state anymore).
                    Span::styled(
                        p.state.badge(),
                        Style::default()
                            .bg(p.state.bg_color())
                            .fg(p.state.color())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(rest_text.clone(), rest),
                ];
                if let Some(bg) = row_bg {
                    // Fill the remainder — after subtracting bar(1) + icon(1) +
                    // space-after-icon(1) + badge(9) + rest_text's width — so
                    // CursorLine reaches all the way to the end of the row.
                    let used = 1 + 1 + 1 + 9 + rest_text.width();
                    let pad = (inner.width as usize).saturating_sub(used);
                    if pad > 0 {
                        spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
                    }
                }
                lines.push(Line::from(spans));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let block = glow_block("DETAIL", COLOR_DETAIL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(p) = app.selected_pane() else {
        f.render_widget(
            Paragraph::new("選択中のペインがありません").style(Style::default().fg(COLOR_DIM)),
            inner,
        );
        return;
    };

    let label = |k: &str| Span::styled(format!("{k:<9}: "), Style::default().fg(COLOR_LABEL));
    let mut lines = vec![
        Line::from(Span::styled(
            p.project().to_string(),
            Style::default().fg(COLOR_FG).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            label("status"),
            Span::styled(
                format!("{} ", p.state.icon()),
                Style::default().fg(p.state.color()),
            ),
            Span::styled(
                p.state.badge(),
                Style::default()
                    .bg(p.state.bg_color())
                    .fg(p.state.color())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", p.state.label()),
                Style::default().fg(COLOR_DIM),
            ),
        ]),
        Line::from(vec![
            label("agent"),
            Span::raw(p.agent.clone().unwrap_or_else(|| "-".into())),
        ]),
        Line::from(vec![label("cwd"), Span::raw(shorten_home(&p.cwd))]),
        Line::from(vec![
            label("branch"),
            Span::raw(p.branch.clone().unwrap_or_else(|| "-".into())),
        ]),
        Line::from(vec![label("task"), Span::raw(p.task.clone())]),
        Line::from(""),
        section("位置", inner.width),
        // workspace/window/tab/pane are a different granularity from the other
        // fields (four values that together describe "where this pane is"), so
        // they get their own section, each printed with the same label()
        // format as the other fields.
        Line::from(vec![label("workspace"), Span::raw(p.workspace.clone())]),
        Line::from(vec![label("window"), Span::raw(p.window_id.to_string())]),
        Line::from(vec![label("tab"), Span::raw(p.tab_id.to_string())]),
        Line::from(vec![label("pane"), Span::raw(p.pane_id.to_string())]),
        Line::from(""),
        section("通知", inner.width),
    ];

    if p.notifications.is_empty() {
        lines.push(Line::from(Span::styled(
            "  （まだありません）",
            Style::default().fg(COLOR_DIM),
        )));
    } else {
        for n in &p.notifications {
            let marker = if n.unread { "●" } else { " " };
            let style = if n.unread {
                Style::default().fg(COLOR_ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(COLOR_DIM)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{marker} {} ", n.hhmm()), style),
                Span::raw(n.text.clone()),
            ]));
            // A notification's text is fixed wording per state, so on its own
            // it carries little information. Attach the task at the moment it
            // fired (the pane title) so you can trace "what was it doing"
            // (this is the purpose of the task field in spec §2.2).
            let task = crate::model::task_from_title(&n.task);
            if task != "-" {
                lines.push(Line::from(Span::styled(
                    format!("    └ {task}"),
                    Style::default().fg(COLOR_DIM),
                )));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(section("メモ", inner.width));
    // Show only the "# メモ" (memo) section — the human's free-edit area. "## ログ"
    // (log) is where the hook writes and isn't shown here (see the role split in
    // spec §5.1). File I/O happens here, but draw() is called at most once per
    // input event or tick, so this is a different situation from format-tab-title
    // (called every frame, on the GUI render thread) — the constraint in §3.2.1
    // doesn't apply here.
    match memo::read_preview(p.tab_id) {
        Some(body) => {
            const MAX_LINES: usize = 8;
            let body_lines: Vec<&str> = body.lines().collect();
            for line in body_lines.iter().take(MAX_LINES) {
                lines.push(Line::from(Span::raw(format!("  {line}"))));
            }
            if body_lines.len() > MAX_LINES {
                lines.push(Line::from(Span::styled(
                    format!("  … 他 {} 行（`e` で全文編集）", body_lines.len() - MAX_LINES),
                    Style::default().fg(COLOR_DIM),
                )));
            }
        }
        None => {
            lines.push(Line::from(Span::styled(
                "  （まだメモはありません。`e` で編集）",
                Style::default().fg(COLOR_DIM),
            )));
        }
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn section(title: &str, width: u16) -> Line<'static> {
    let dashes = (width as usize).saturating_sub(title.width() + 4);
    Line::from(vec![
        Span::styled("── ", Style::default().fg(COLOR_DIM)),
        Span::styled(title.to_string(), Style::default().fg(COLOR_LABEL)),
        Span::styled(format!(" {}", "─".repeat(dashes)), Style::default().fg(COLOR_DIM)),
    ])
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    if let Some(msg) = &app.message {
        f.render_widget(
            Paragraph::new(Span::styled(
                format!(" {msg}"),
                Style::default().fg(COLOR_WAITING),
            )),
            area,
        );
        return;
    }
    if app.filter_active {
        f.render_widget(
            Paragraph::new(Span::styled(
                format!(" /{}", app.filter),
                Style::default().fg(COLOR_ACCENT),
            )),
            area,
        );
        return;
    }

    // Keys are shown as filled badges, like arcade button labels. Same color
    // scheme as the STATUS badge (p.state.badge()) for visual consistency. Tab
    // cycles the layout (Split / full-width list / full-width detail). Always
    // shown, since on a narrow terminal it also works as a list⇄detail toggle.
    let hints: [(&str, &str); 9] = [
        ("↑/↓", "移動"),
        ("⏎", "ジャンプ"),
        ("Tab", "表示"),
        ("e", "メモ"),
        ("r", "既読"),
        ("R", "全既読"),
        ("/", "絞込"),
        ("g", "更新"),
        ("q", "終了"),
    ];

    let mut spans = vec![Span::raw(" ")];
    for (i, (key, label)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!(" {key} "),
            Style::default()
                .bg(COLOR_ACCENT)
                .fg(COLOR_BADGE_FG)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}"), Style::default().fg(COLOR_DIM)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn shorten_home(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_string(),
    }
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;
