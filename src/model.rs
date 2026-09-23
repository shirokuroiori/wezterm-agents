//! Data structures used for display, and the logic for deriving state.
//!
//! State definitions follow spec §2.1.
//!   working … the agent is generating a response (detected via the pane title spinner)
//!   waiting … waiting for approval/input (not cleared even once read)
//!   done    … response finished and unread
//!   idle    … everything else (including a done that has been read)

use ratatui::style::Color;

use crate::lang::Lang;

/// Matches the voltwave color scheme (.config/wezterm/colors/voltwave.toml).
pub const COLOR_WAITING: Color = Color::Rgb(0xFE, 0x44, 0x50);
pub const COLOR_DONE: Color = Color::Rgb(0x50, 0xfa, 0x7b);
pub const COLOR_WORKING: Color = Color::Rgb(0xFF, 0xCC, 0x00);
pub const COLOR_ACCENT: Color = Color::Rgb(0x38, 0xda, 0xff);
pub const COLOR_DIM: Color = Color::Rgb(0x6B, 0x7A, 0x8F);
/// For column headers and DETAIL field names (e.g. "agent", "cwd").
/// COLOR_DIM is exactly voltwave's comment color, intentionally low-contrast,
/// so it doesn't suit labels meant to always be legible. Use palette.lua's
/// blue instead so they stay clearly readable.
pub const COLOR_LABEL: Color = Color::Rgb(0x7B, 0x9C, 0xFF);
pub const COLOR_FG: Color = Color::Rgb(0x72, 0xF1, 0xB8);
/// DETAIL pane's dedicated color. Paired with LIST using cyan (COLOR_ACCENT),
/// DETAIL always gets voltwave's purple (whether it's focused is shown by
/// the border_type, double vs. single line — see glow_block in ui.rs).
pub const COLOR_DETAIL: Color = Color::Rgb(0xAF, 0x6D, 0xF9);
/// Background of the row the cursor sits on in LIST. palette.lua's line_hl
/// (CursorLine, #2D1745) nearly disappeared into bg and was hard to see,
/// so use the brighter surface color (#4F326A) instead.
pub const COLOR_CURSOR_LINE: Color = Color::Rgb(0x4F, 0x32, 0x6A);
/// Text color for filled chips such as the footer key hints. Matches
/// voltwave's background color so it reads clearly even on a bright
/// ACCENT fill.
pub const COLOR_BADGE_FG: Color = Color::Rgb(0x20, 0x09, 0x33);

/// Background for the STATUS badge. Painting each state's color directly
/// made the text (COLOR_BADGE_FG) unreadable, so we invert it: keep the
/// text in state.color()'s vivid color, and darken the background by
/// mixing bg (#200933) in at 7:3, producing a "same hue but muted" color.
/// Dropping straight to black would pull it away from voltwave's tone, so
/// the mix target must always be voltwave's bg.
///
/// (A version darkened almost to black, similar to diff_add_bg/diff_delete_bg,
/// was rejected because all four states' backgrounds became indistinguishable,
/// each swallowed by black. Confirmed on real hardware 2026-08-17.)
pub const COLOR_WAITING_BG: Color = Color::Rgb(0x63, 0x1B, 0x3C);
pub const COLOR_DONE_BG: Color = Color::Rgb(0x2E, 0x51, 0x49);
pub const COLOR_WORKING_BG: Color = Color::Rgb(0x63, 0x44, 0x24);
pub const COLOR_IDLE_BG: Color = Color::Rgb(0x37, 0x2B, 0x4F);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Working,
    Waiting,
    Done,
    Idle,
}

impl State {
    /// Status icon (Nerd Font). Code points looked up from nerd-fonts'
    /// glyphnames.json. Confirmed on real hardware that waiting/done/idle
    /// render correctly with `Bizin Gothic Discord NF`.
    ///   waiting  nf-cod-stop_circle  U+EBA5
    ///   working  nf-md-dots_circle   U+F1978 (kept consistent with the tab bar's working indicator in wezterm.lua)
    ///   done     nf-fa-ok_sign       U+F058
    ///   idle     nf-md-sleep         U+F04B2
    ///
    /// All of these are in a private-use area, so `unicode-width` counts
    /// them as width 1. Even if a terminal renders them as 2 cells, every
    /// row has exactly one icon, so the offset is uniform and column
    /// alignment across rows still holds.
    pub fn icon(self) -> &'static str {
        match self {
            State::Working => "\u{f1978}",
            State::Waiting => "\u{eba5}",
            State::Done => "\u{f058}",
            State::Idle => "\u{f04b2}",
        }
    }

    /// The text for the status badge, shown with a filled background.
    /// Padded with spaces on both sides for breathing room in the fill,
    /// and to keep a uniform width (all 9 columns).
    pub fn badge(self) -> &'static str {
        match self {
            State::Working => " WORKING ",
            State::Waiting => " WAITING ",
            State::Done => " DONE    ",
            State::Idle => " IDLE    ",
        }
    }

    pub fn label(self, lang: Lang) -> &'static str {
        match (self, lang) {
            (State::Working, Lang::En) => "Working",
            (State::Working, Lang::Ja) => "応答生成中",
            (State::Waiting, Lang::En) => "Waiting for input",
            (State::Waiting, Lang::Ja) => "承認/入力待ち",
            (State::Done, Lang::En) => "Done",
            (State::Done, Lang::Ja) => "応答完了",
            (State::Idle, Lang::En) => "Idle",
            (State::Idle, Lang::Ja) => "待機中",
        }
    }

    pub fn color(self) -> Color {
        match self {
            State::Working => COLOR_WORKING,
            State::Waiting => COLOR_WAITING,
            State::Done => COLOR_DONE,
            State::Idle => COLOR_DIM,
        }
    }

    /// Background color for the STATUS badge (same hue as color(), darkened).
    pub fn bg_color(self) -> Color {
        match self {
            State::Working => COLOR_WORKING_BG,
            State::Waiting => COLOR_WAITING_BG,
            State::Done => COLOR_DONE_BG,
            State::Idle => COLOR_IDLE_BG,
        }
    }
}

/// A notification event corresponding to one line of `.jsonl`.
#[derive(Debug, Clone)]
pub struct Notification {
    pub at: String,
    pub agent: String,
    pub kind: String,
    pub text: String,
    pub task: String,
    /// Whether this is newer than `.read`. The unread count is a derived
    /// value counting these (spec §1).
    pub unread: bool,
}

impl Notification {
    /// "2026-08-16T21:40:03+09:00" -> "21:40"
    pub fn hhmm(&self) -> &str {
        self.at.get(11..16).unwrap_or("--:--")
    }
}

#[derive(Debug, Clone)]
pub struct Pane {
    pub pane_id: u64,
    pub window_id: u64,
    pub tab_id: u64,
    pub workspace: String,
    pub cwd: String,
    pub branch: Option<String>,
    pub agent: Option<String>,
    pub state: State,
    pub unread: usize,
    /// Newest first, capped at 10 entries.
    pub notifications: Vec<Notification>,
    /// The most recent task content (derived from the pane title).
    pub task: String,
}

impl Pane {
    pub fn project(&self) -> &str {
        self.cwd.rsplit('/').find(|s| !s.is_empty()).unwrap_or(&self.cwd)
    }
}

#[derive(Debug, Clone)]
pub struct Group {
    pub cwd: String,
    pub panes: Vec<Pane>,
}

impl Group {
    pub fn label(&self) -> &str {
        self.cwd.rsplit('/').find(|s| !s.is_empty()).unwrap_or(&self.cwd)
    }

    pub fn unread(&self) -> usize {
        self.panes.iter().map(|p| p.unread).sum()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub groups: Vec<Group>,
}

impl Snapshot {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

/// Determines "generating a response" from the spinner character at the
/// start of the pane title.
///
/// This watches the cue the agent itself puts on screen, rather than a
/// hook, so it still works even when a terminating hook doesn't fire (e.g.
/// an Esc-interrupted turn) — the same approach used in wezterm.lua.
///
/// There are two variants, and which one applies depends on the Claude
/// Code version (confirmed the switch to the circular spinner in
/// `bin/wezterm-agents` at 2.1.233).
///   - Braille spinner ⠀-⣿ (U+2800-U+28FF) (older Claude Code)
///   - Circular spinner ◐◓◑◒ (U+25D0-U+25D3) (current Claude Code)
///
///
/// Copilot CLI doesn't put a spinner in the title; it draws a status line
/// on the last row of the screen instead. Reading that would require
/// calling `wezterm cli get-text` per pane, which violates the
/// one-subprocess-per-tick policy (spec §4.4), so this function (spinner
/// detection at the start of the title) can't be used for Copilot's
/// working state. Copilot's working state is instead inferred in
/// `derive_state` from `working_since` (the send time written by the
/// `userPromptSubmitted` hook).
pub fn is_working_title(title: &str) -> bool {
    matches!(title.chars().next(), Some(c) if
        ('\u{2800}'..='\u{28FF}').contains(&c) || ('\u{25D0}'..='\u{25D3}').contains(&c))
}

/// Builds the text shown as the task from the title.
/// Strips the leading spinner character and markers, and reduces anything
/// that's just a process name to "-".
pub fn task_from_title(title: &str) -> String {
    let trimmed = title
        .trim_start_matches(|c: char| {
            ('\u{2800}'..='\u{28FF}').contains(&c)
                || ('\u{25D0}'..='\u{25D3}').contains(&c)
                || c == '✳'
                || c.is_whitespace()
        })
        .trim();
    match trimmed {
        "" | "zsh" | "bash" | "nvim" | "vim" | "node" | "claude" | "copilot" | "wezterm-gui" => {
            "-".to_string()
        }
        other => other.to_string(),
    }
}

/// Builds a fallback display name for the AGENT column from the title.
///
/// The notification log (claude/copilot hooks) only tells us about AI
/// agents — nvim and shells never write notifications, so `agent` was
/// always `None` for them and LIST showed nothing but "-". Unless the pane
/// itself has set a title via OSC, WezTerm shows the foreground process
/// name ("nvim", "zsh", etc.) directly in the title (equivalent to
/// wezterm.lua's foreground_process_name), so we use that as a stand-in
/// for the AGENT column.
///
/// The spinner (working) / ✳ (idle) at the start of the title is a marker
/// Claude Code prepends to the task text, and what follows it is task
/// wording, not a process name (e.g. "✳ automate video editing"). If we
/// stripped that marker and treated the rest as a process name, the task
/// text itself would end up in the AGENT column, so whenever this marker
/// is found we don't pass it through — we return "claude" instead. Only
/// titles without the marker are used as-is as a process name. If what's
/// left after stripping is empty, or it's wezterm-agents' own default
/// title, there's nothing worth showing on the tab, so return None.
pub fn agent_from_title(title: &str) -> Option<String> {
    let trimmed = title.trim();
    if matches!(trimmed.chars().next(), Some(c) if
        ('\u{2800}'..='\u{28FF}').contains(&c) || ('\u{25D0}'..='\u{25D3}').contains(&c) || c == '✳')
    {
        return Some("claude".to_string());
    }
    match trimmed {
        "" | "wezterm-gui" => None,
        other => Some(other.to_string()),
    }
}

/// Determines state from the notification history and the working signal.
///
/// `working` is a signal that can be confirmed on the spot, such as the
/// title spinner (Claude Code). `working_since` is the most recent send
/// time written by the `userPromptSubmitted` hook (Copilot CLI). If it's
/// newer than the latest notification event, we infer that turn hasn't
/// reached done/waiting yet — i.e. it's still generating a response.
/// Since one of agentStop/notification/errorOccurred/sessionEnd always
/// writes a new done/waiting afterward, this inference never stays wrong
/// indefinitely.
///
/// waiting is not cleared even once read. It reflects an actual state of
/// waiting for input, and once the user responds the agent starts working
/// again and gets picked up by working detection, so it resolves on its
/// own if left alone (spec §2.1).
pub fn derive_state(working: bool, working_since: Option<&str>, notifications: &[Notification]) -> State {
    if working {
        return State::Working;
    }
    if let Some(since) = working_since {
        let superseded = notifications.first().is_some_and(|n| n.at.as_str() >= since);
        if !superseded {
            return State::Working;
        }
    }
    match notifications.first() {
        Some(n) if n.kind == "waiting" => State::Waiting,
        Some(n) if n.kind == "done" => {
            if n.unread {
                State::Done
            } else {
                State::Idle
            }
        }
        _ => State::Idle,
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
