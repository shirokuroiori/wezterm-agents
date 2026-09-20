//! App state, plus building the list rows and cursor movement.

use std::time::Duration;

use crate::model::{Pane, Snapshot};
use crate::store::Store;
use crate::wezterm;

/// Upper bound on waiting for FocusLost after a jump. Measured jump latency
/// is 50-102ms, so this leaves plenty of margin while still not feeling
/// like a hang if it fails.
const JUMP_EXIT_TIMEOUT: Duration = Duration::from_millis(800);

/// One row of the list. The cursor can only land on a Pane row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Blank,
    Header { group: usize },
    Pane { group: usize, pane: usize },
}

/// Screen layout. Cycled through with `Tab` (design spec §4.2).
///
///   Split      … LIST left 45% / DETAIL right 55%. The default. Only
///                meaningful at 100+ columns wide (below that it looks the
///                same as ListFull)
///   ListFull   … LIST at full width. Drops the three numeric columns
///                (WIN/TAB/PANE) and gives all the freed width to the TASK
///                column. For reading "what is each agent doing right now"
///                straight from the list
///   DetailFull … DETAIL at full width. For reading the notification
///                history and notes together
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Split,
    ListFull,
    DetailFull,
}

pub struct App {
    pub snapshot: Snapshot,
    pub rows: Vec<Row>,
    pub cursor: usize,
    pub filter: String,
    pub filter_active: bool,
    pub message: Option<String>,
    pub layout: LayoutMode,
    pub should_quit: bool,
    /// In launcher mode, quit once a jump happens.
    pub exit_on_jump: bool,
    /// Deadline while waiting for focus to actually move after a jump is
    /// sent.
    ///
    /// If the process exits right after sending the OSC, the pane closes
    /// and WezTerm can end up reassigning focus to a different tab before
    /// it's done processing the OSC. This reproduced as a flaky race in
    /// testing (sometimes worked, sometimes didn't). Once a jump actually
    /// lands, our own pane is guaranteed to receive FocusLost, so that's
    /// used as the exit signal — with this deadline as a safety net in
    /// case it never arrives.
    pub pending_exit: Option<std::time::Instant>,
    /// Flag telling main.rs to launch the editor on the next tick after `e`
    /// is pressed. app.rs doesn't own a Terminal, so the actual launch
    /// happens on the main.rs side.
    pub pending_edit: Option<(u64, String)>,
    store: Store,
    self_pane: Option<u64>,
    /// The pane that was active when cmd+shift+a was pressed. Read from
    /// WEZTERM_AGENTS_ORIGIN_PANE, which keys.lua passes via
    /// SpawnCommandInNewTab's set_environment_variables. Unset for launch
    /// paths that entered this tab manually, e.g. `--watch`.
    origin_pane: Option<u64>,
}

impl App {
    pub fn new(exit_on_jump: bool) -> Self {
        let self_pane = std::env::var("WEZTERM_PANE")
            .ok()
            .and_then(|s| s.parse::<u64>().ok());
        let origin_pane = std::env::var("WEZTERM_AGENTS_ORIGIN_PANE")
            .ok()
            .and_then(|s| s.parse::<u64>().ok());
        Self {
            snapshot: Snapshot::default(),
            rows: Vec::new(),
            cursor: 0,
            filter: String::new(),
            filter_active: false,
            message: None,
            layout: LayoutMode::Split,
            should_quit: false,
            exit_on_jump,
            pending_exit: None,
            pending_edit: None,
            store: Store::new(),
            self_pane,
            origin_pane,
        }
    }

    /// Call `wezterm cli list` once and rebuild the snapshot from it.
    /// On failure, keeps the previous snapshot and shows the reason in the
    /// footer.
    pub fn refresh(&mut self) {
        match wezterm::list_panes() {
            Ok(panes) => {
                self.snapshot = wezterm::build_snapshot(panes, &mut self.store, self.self_pane);
                self.message = None;
            }
            Err(e) => {
                self.message = Some(e);
            }
        }
        self.rebuild_rows();
    }

    /// Clean up dead panes' state files, once at startup.
    pub fn gc_once(&self) {
        let live: Vec<u64> = self
            .snapshot
            .groups
            .iter()
            .flat_map(|g| g.panes.iter().map(|p| p.pane_id))
            .chain(self.self_pane)
            .collect();
        if !live.is_empty() {
            Store::gc(&live);
        }
    }

    fn matches_filter(&self, p: &Pane) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        let needle = self.filter.to_lowercase();
        let hay = format!(
            "{} {} {} {}",
            p.cwd,
            p.branch.as_deref().unwrap_or(""),
            p.task,
            p.agent.as_deref().unwrap_or("")
        )
        .to_lowercase();
        hay.contains(&needle)
    }

    /// Remember which pane the cursor was on, and return to that same pane
    /// after rebuilding. Without this, resorting on every 1-second rebuild
    /// would make the selection jump around.
    pub fn rebuild_rows(&mut self) {
        let anchor = self.selected_pane().map(|p| p.pane_id);
        let mut rows = Vec::new();
        for (gi, group) in self.snapshot.groups.iter().enumerate() {
            let visible: Vec<usize> = group
                .panes
                .iter()
                .enumerate()
                .filter(|(_, p)| self.matches_filter(p))
                .map(|(i, _)| i)
                .collect();
            if visible.is_empty() {
                continue;
            }
            if !rows.is_empty() {
                rows.push(Row::Blank);
            }
            rows.push(Row::Header { group: gi });
            for pi in visible {
                rows.push(Row::Pane { group: gi, pane: pi });
            }
        }
        self.rows = rows;

        self.cursor = anchor
            .and_then(|id| self.row_index_of_pane(id))
            .or_else(|| self.first_pane_row())
            .unwrap_or(0);
    }

    fn row_index_of_pane(&self, pane_id: u64) -> Option<usize> {
        self.rows.iter().position(|r| match r {
            Row::Pane { group, pane } => self.snapshot.groups[*group].panes[*pane].pane_id == pane_id,
            Row::Header { .. } | Row::Blank => false,
        })
    }

    fn first_pane_row(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(r, Row::Pane { .. }))
    }

    pub fn selected_pane(&self) -> Option<&Pane> {
        match self.rows.get(self.cursor)? {
            Row::Pane { group, pane } => Some(&self.snapshot.groups[*group].panes[*pane]),
            Row::Header { .. } | Row::Blank => None,
        }
    }

    /// Skip header rows, landing on the next Pane row.
    pub fn move_cursor(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as isize;
        let mut i = self.cursor as isize;
        for _ in 0..len {
            i += delta;
            if i < 0 {
                i = len - 1;
            } else if i >= len {
                i = 0;
            }
            if matches!(self.rows[i as usize], Row::Pane { .. }) {
                self.cursor = i as usize;
                return;
            }
        }
    }

    /// Scroll offset needed to keep the cursor within view.
    pub fn scroll_offset(&self, height: usize) -> usize {
        if height == 0 || self.cursor < height {
            return 0;
        }
        self.cursor + 1 - height
    }

    pub fn jump_to_selected(&mut self) {
        let Some(pane_id) = self.selected_pane().map(|p| p.pane_id) else {
            return;
        };
        self.begin_jump(pane_id);
    }

    /// Send a jump to pane_id, and in launcher mode, prepare to wait for
    /// FocusLost before quitting (shared by jump_to_selected and quit).
    fn begin_jump(&mut self, pane_id: u64) {
        match wezterm::jump(pane_id) {
            Ok(()) => {
                if self.exit_on_jump {
                    // Don't quit right away. Wait for the focus-moved
                    // signal (FocusLost) first — see pending_exit's doc
                    // comment for why.
                    self.pending_exit =
                        Some(std::time::Instant::now() + JUMP_EXIT_TIMEOUT);
                    self.message = Some("ジャンプ中…".into());
                }
            }
            Err(e) => self.message = Some(e),
        }
    }

    /// Esc/q. When closing without selecting anything, jump back to the
    /// pane that was active before cmd+shift+a was pressed (origin_pane)
    /// before quitting. Without this, WezTerm's default behavior (closing
    /// a tab moves focus to the tab on its right) would leave you unable
    /// to get back to the tab you were on before cmd+shift+a. When there's
    /// no origin_pane (e.g. `--watch`), quits immediately as before.
    pub fn quit(&mut self) {
        match self.origin_pane {
            Some(pane_id) if self.exit_on_jump => self.begin_jump(pane_id),
            _ => self.should_quit = true,
        }
    }

    /// Safety net preventing us from getting stuck unable to quit if
    /// FocusLost never arrives.
    pub fn tick_pending_exit(&mut self) {
        if let Some(deadline) = self.pending_exit {
            if std::time::Instant::now() >= deadline {
                self.should_quit = true;
            }
        }
    }

    /// Lost focus == the jump succeeded.
    pub fn on_focus_lost(&mut self) {
        if self.pending_exit.is_some() {
            self.should_quit = true;
        }
    }

    pub fn mark_selected_read(&mut self) {
        if let Some(p) = self.selected_pane() {
            Store::mark_read(p.pane_id);
            self.refresh();
        }
    }

    pub fn mark_all_read(&mut self) {
        let ids: Vec<u64> = self
            .snapshot
            .groups
            .iter()
            .flat_map(|g| g.panes.iter().map(|p| p.pane_id))
            .collect();
        for id in ids {
            Store::mark_read(id);
        }
        self.refresh();
    }

    /// The `Tab` key. Cycles Split → ListFull → DetailFull → Split.
    ///
    /// On a narrow terminal (`narrow`), Split and ListFull look identical,
    /// so Split is skipped, making it a 2-state toggle between ListFull
    /// and DetailFull.
    pub fn cycle_layout(&mut self, narrow: bool) {
        self.layout = match (self.layout, narrow) {
            (LayoutMode::Split, false) => LayoutMode::ListFull,
            (LayoutMode::Split, true) => LayoutMode::DetailFull,
            (LayoutMode::ListFull, _) => LayoutMode::DetailFull,
            (LayoutMode::DetailFull, false) => LayoutMode::Split,
            (LayoutMode::DetailFull, true) => LayoutMode::ListFull,
        };
    }

    /// The `e` key. Defers the actual editor launch to main.rs (which owns
    /// the Terminal) via a flag (design spec §5.3).
    pub fn request_edit(&mut self) {
        if let Some(p) = self.selected_pane() {
            self.pending_edit = Some((p.tab_id, p.cwd.clone()));
        }
    }
}
