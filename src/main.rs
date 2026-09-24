//! A TUI for listing and monitoring AI agents on WezTerm, and jumping to
//! the pane you pick.
//! Design: docs/plans/wezterm-multi-agent-spec.md §4.
//!
//! Usage:
//!   wezterm-agents                 Launcher. Exits once you jump
//!   wezterm-agents --watch         Persistent dashboard. Stays up after jumping
//!   wezterm-agents --resident      Persistent dashboard reused by the plugin's dashboard key
//!   wezterm-agents --print         Print the list once, no interaction
//!   wezterm-agents --interval 3    Set the tick interval (seconds) explicitly
//!   wezterm-agents hook ...        Agent hook entry point (hook.rs)
//!   wezterm-agents status-dir      Print the state directory path (for agents.lua)
//!   wezterm-agents init <shell>    Print the Claude Code shell function (setup.rs)
//!   wezterm-agents install claude  Append the above shell function to ~/.zshenv (setup.rs)
//!   wezterm-agents install copilot Write the Copilot CLI hook file (setup.rs)

mod app;
mod hook;
mod lang;
mod memo;
mod model;
mod paths;
mod setup;
mod store;
mod ui;
mod util;
mod wezterm;

use std::io::{self, Stdout};
use std::process::Command;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableFocusChange, EnableFocusChange, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::CrosstermBackend;
use ratatui::Terminal;

use app::App;
use model::State;

/// Tick interval (spec §4.4).
/// The launcher disappears within seconds, so 1Hz is fine. --watch stays
/// resident, so it backs off further. It backs off even more while
/// unfocused so we're not waking the CPU for a screen no one is looking
/// at, keeping a laptop's deep idle undisturbed.
const TICK_LAUNCHER: Duration = Duration::from_secs(1);
const TICK_WATCH_FOCUSED: Duration = Duration::from_secs(2);
const TICK_WATCH_BLURRED: Duration = Duration::from_secs(5);

struct Options {
    watch: bool,
    /// Implies `watch`. See `App::resident`.
    resident: bool,
    print: bool,
    interval: Option<Duration>,
}

fn parse_args(lang: lang::Lang) -> Result<Options, String> {
    let mut opts = Options {
        watch: false,
        resident: false,
        print: false,
        interval: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--watch" | "-w" => opts.watch = true,
            "--resident" => {
                opts.watch = true;
                opts.resident = true;
            }
            "--print" | "-p" => opts.print = true,
            "--interval" | "-i" => {
                let v = args.next().ok_or_else(|| match lang {
                    lang::Lang::En => "--interval requires a number of seconds".to_string(),
                    lang::Lang::Ja => "--interval には秒数が要ります".to_string(),
                })?;
                let secs: u64 = v.parse().map_err(|_| match lang {
                    lang::Lang::En => format!("Invalid number of seconds: {v}"),
                    lang::Lang::Ja => format!("秒数が不正です: {v}"),
                })?;
                if secs == 0 {
                    return Err(match lang {
                        lang::Lang::En => "--interval must be at least 1".to_string(),
                        lang::Lang::Ja => "--interval は1以上にしてください".to_string(),
                    });
                }
                opts.interval = Some(Duration::from_secs(secs));
            }
            "--help" | "-h" => {
                print_help(lang);
                std::process::exit(0);
            }
            other => {
                return Err(match lang {
                    lang::Lang::En => format!("Unknown option: {other}"),
                    lang::Lang::Ja => format!("不明なオプション: {other}"),
                })
            }
        }
    }
    Ok(opts)
}

fn print_help(lang: lang::Lang) {
    let text = match lang {
        lang::Lang::En => "\
AI agent list on WezTerm

  wezterm-agents                 Launcher (exits once you jump)
  wezterm-agents --watch         Persistent dashboard
  wezterm-agents --resident      Persistent version for the plugin's dashboard key
                                 (q/Esc returns to the original workspace; Ctrl+C quits)
  wezterm-agents --print         Print once, no interaction
  wezterm-agents --interval <sec> Set the tick interval explicitly
  wezterm-agents --version       Print the version

Subcommands:
  wezterm-agents hook --agent <name> <pretool|waiting|done|working>
                                 Entry point called from an agent's hook.
                                 Pass the agent's JSON payload on stdin
  wezterm-agents status-dir      Print the state directory path
  wezterm-agents init <zsh|bash|fish>
                                 Print the shell function that injects Claude
                                 Code's hooks. To set up by hand, add
                                 `eval \"$(wezterm-agents init zsh)\"` to ~/.zshenv
  wezterm-agents install claude  Append the line above to ~/.zshenv (zsh only;
                                 re-run to update. Only touches its marker block)
  wezterm-agents install copilot Write the Copilot CLI hook file to
                                 ~/.copilot/hooks/wezterm-agents.json

Keys:
  ↑/↓ k/j  move      ⏎  jump    e  edit memo
  r  mark read    R  mark all read   /  filter    g  refresh now
  Tab  toggle view (list⇄detail)           q/Esc  quit",
        lang::Lang::Ja => "\
WezTerm 上のAIエージェント一覧

  wezterm-agents                 ランチャー（ジャンプで終了）
  wezterm-agents --watch         常駐ダッシュボード
  wezterm-agents --resident      プラグインのダッシュボードキー用の常駐版
                                 （q/Esc で元のワークスペースへ戻る。終了は Ctrl+C）
  wezterm-agents --print         対話なしで1回表示して終了
  wezterm-agents --interval <秒> ティック間隔を明示指定
  wezterm-agents --version       バージョンを出力する

サブコマンド:
  wezterm-agents hook --agent <name> <pretool|waiting|done|working>
                                 エージェントの hook から呼ばれる本体。
                                 stdin にエージェントの JSON payload を渡す
  wezterm-agents status-dir      状態ディレクトリのパスを出力する
  wezterm-agents init <zsh|bash|fish>
                                 Claude Code の hooks を注入するシェル関数を
                                 出力する。手で設定するなら ~/.zshenv に
                                 `eval \"$(wezterm-agents init zsh)\"` を追加する
  wezterm-agents install claude  上の1行を ~/.zshenv に追記する（zsh 用。
                                 再実行で更新。マーカー区画だけを触る）
  wezterm-agents install copilot Copilot CLI 用の hook ファイルを
                                 ~/.copilot/hooks/wezterm-agents.json に書く

キー:
  ↑/↓ k/j  移動      ⏎  ジャンプ    e  メモ編集
  r 既読    R 全既読   /  絞り込み    g  即時更新
  Tab 表示切替(一覧⇄詳細)           q/Esc 終了",
    };
    println!("{text}");
}

/// Dispatches subcommands. Anything other than the TUI is handled and
/// exited here entirely.
/// Returns `false` when nothing matched, falling through to the usual
/// option parsing.
fn dispatch_subcommand() -> bool {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("hook") => {
            run_hook(args.collect());
            true
        }
        Some("status-dir") => {
            // agents.lua calls this once at startup. Both deciding the path
            // and creating it are centralized here on the Rust side, on
            // purpose, so Lua never hardcodes a path.
            //
            // We only print once creation and validation have succeeded, so
            // a successful print also guarantees "that location can be
            // safely read and written." On failure we exit non-zero and the
            // Lua side gives up on reading/writing state files.
            match paths::ensure_status_dir() {
                Ok(dir) => println!("{}", dir.display()),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            true
        }
        Some("init") => {
            run_init(args.collect());
            true
        }
        Some("install") => {
            run_install(args.collect());
            true
        }
        Some("--version" | "-V") => {
            // install.sh compares this against the version it wants, to
            // decide whether an auto-installed binary is out of date.
            println!("wezterm-agents {}", env!("CARGO_PKG_VERSION"));
            true
        }
        _ => false,
    }
}

/// The `init` subcommand. Prints the shell function that injects Claude
/// Code's hooks to stdout (meant to be loaded via
/// `eval "$(wezterm-agents init zsh)"` — see setup.rs).
fn run_init(args: Vec<String>) {
    let lang = lang::Lang::from_env();
    let Some(shell) = args.first() else {
        eprintln!(
            "{}",
            match lang {
                lang::Lang::En => "wezterm-agents init: a shell name is required (zsh|bash|fish)",
                lang::Lang::Ja => "wezterm-agents init: シェル名が要ります (zsh|bash|fish)",
            }
        );
        std::process::exit(1);
    };
    let result = setup::current_bin().and_then(|bin| setup::shell_init(shell, &bin));
    match result {
        Ok(snippet) => print!("{snippet}"),
        Err(e) => {
            eprintln!("wezterm-agents init: {e}");
            std::process::exit(1);
        }
    }
}

/// The `install` subcommand. Writes each agent's hook wiring to a file.
///
/// - `claude`: appends the `init zsh` shell function to the marker block
///   in `~/.zshenv` (`install_claude` in setup.rs). It defines the same
///   function as the wezterm plugin's ZDOTDIR injection, so using both
///   doesn't double up (the plugin's `.zshenv` skips defining it if it's
///   already defined).
/// - `copilot`: writes a drop-in file under `~/.copilot/hooks/`.
fn run_install(args: Vec<String>) {
    let lang = lang::Lang::from_env();
    let target = args.first().map(String::as_str);
    let result = match target {
        Some("claude") => setup::current_bin().and_then(|bin| setup::install_claude(&bin)),
        Some("copilot") => setup::current_bin().and_then(|bin| setup::install_copilot(&bin)),
        Some(other) => {
            eprintln!(
                "{}",
                match lang {
                    lang::Lang::En =>
                        format!("wezterm-agents install: unknown target: {other} (claude | copilot)"),
                    lang::Lang::Ja =>
                        format!("wezterm-agents install: 不明なターゲットです: {other}（claude | copilot）"),
                }
            );
            std::process::exit(1);
        }
        None => {
            eprintln!(
                "{}",
                match lang {
                    lang::Lang::En => "wezterm-agents install: a target is required (claude | copilot)",
                    lang::Lang::Ja => "wezterm-agents install: ターゲットが要ります（claude | copilot）",
                }
            );
            std::process::exit(1);
        }
    };
    match result {
        Ok(path) => {
            println!(
                "{}",
                match lang {
                    lang::Lang::En => format!("Wrote: {}", path.display()),
                    lang::Lang::Ja => format!("書き込みました: {}", path.display()),
                }
            );
            if target == Some("claude") {
                println!(
                    "{}",
                    match lang {
                        lang::Lang::En =>
                            "Takes effect in new shells (already-open shells won't pick it up)",
                        lang::Lang::Ja =>
                            "新しいシェルから有効になります（既に開いているシェルには反映されません）",
                    }
                );
            }
        }
        Err(e) => {
            eprintln!("wezterm-agents install {}: {e}", target.unwrap_or(""));
            std::process::exit(1);
        }
    }
}

/// The `hook` subcommand.
///
/// **The exit code is always 0.** Claude Code interprets a hook exit code
/// of 2 as "block this tool call," and treats any other non-zero code as
/// an error too. Failing to emit a notification is not, by itself, a
/// reason to halt the agent's work, so we only write failures to stderr
/// and always exit 0.
fn run_hook(args: Vec<String>) {
    let lang = lang::Lang::from_env();
    let mut agent = String::new();
    let mut event: Option<hook::Event> = None;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--agent" | "-a" => match it.next() {
                Some(v) => agent = v,
                None => {
                    eprintln!(
                        "{}",
                        match lang {
                            lang::Lang::En =>
                                "wezterm-agents hook: --agent requires an agent name",
                            lang::Lang::Ja => "wezterm-agents hook: --agent にはエージェント名が要ります",
                        }
                    );
                    return;
                }
            },
            other => match hook::Event::parse(other) {
                Some(e) => event = Some(e),
                None => {
                    eprintln!(
                        "{}",
                        match lang {
                            lang::Lang::En => format!("wezterm-agents hook: unknown argument: {other}"),
                            lang::Lang::Ja => format!("wezterm-agents hook: 不明な引数: {other}"),
                        }
                    );
                    return;
                }
            },
        }
    }

    let Some(event) = event else {
        eprintln!(
            "{}",
            match lang {
                lang::Lang::En =>
                    "wezterm-agents hook: an event name is required (pretool|waiting|done|working)",
                lang::Lang::Ja =>
                    "wezterm-agents hook: イベント名が要ります (pretool|waiting|done|working)",
            }
        );
        return;
    };
    if agent.is_empty() {
        eprintln!(
            "{}",
            match lang {
                lang::Lang::En => "wezterm-agents hook: --agent <name> is required",
                lang::Lang::Ja => "wezterm-agents hook: --agent <name> が要ります",
            }
        );
        return;
    }

    if let Err(e) = hook::run(&agent, event) {
        eprintln!("wezterm-agents hook: {e}");
    }
}

fn main() {
    if dispatch_subcommand() {
        return;
    }

    let lang = lang::Lang::from_env();
    let opts = match parse_args(lang) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            eprintln!(
                "{}",
                match lang {
                    lang::Lang::En => "Run `--help` to see usage",
                    lang::Lang::Ja => "`--help` で使い方を表示します",
                }
            );
            std::process::exit(2);
        }
    };

    if opts.print {
        print_once();
        return;
    }

    if let Err(e) = run(opts) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

/// The status badge used for `--print`. Built directly with ANSI escapes
/// since it doesn't go through ratatui.
/// Uses the same colors and same strings as the TUI side (ui.rs) so the
/// look stays consistent.
fn ansi_badge(state: State) -> String {
    let (r, g, b) = match state {
        State::Working => (0xFF, 0xCC, 0x00),
        State::Waiting => (0xFE, 0x44, 0x50),
        State::Done => (0x50, 0xfa, 0x7b),
        State::Idle => (0x6B, 0x7A, 0x8F),
    };
    format!(
        "\x1b[48;2;{r};{g};{b}m\x1b[38;2;32;9;51m\x1b[1m{}\x1b[0m",
        state.badge()
    )
}

/// A static, non-interactive display. For environments without /dev/tty,
/// or for debugging.
fn print_once() {
    let mut app = App::new(false);
    app.refresh();
    if let Err(e) = paths::ensure_status_dir() {
        eprintln!("{e}");
    }
    if let Some(msg) = &app.message {
        eprintln!("{msg}");
    }
    for group in &app.snapshot.groups {
        let unread = group.unread();
        let badge = if unread > 0 {
            format!("  ●{unread}")
        } else {
            String::new()
        };
        println!("\n{} ({}){}", group.label(), group.cwd, badge);
        // Build the header by reusing the row's format string as-is.
        // Writing the width numbers in two separate places risks fixing
        // one and forgetting the other, throwing the columns out of
        // alignment — so it's all funneled through a single row(), called
        // from both the header and the body.
        // One column per positional arg; a struct would just move the same
        // 8 fields without reducing them, so the warning is allowed here.
        #[allow(clippy::too_many_arguments)]
        fn row(icon: &str, unread: &str, ws: &str, win: &str, tab: &str, pane: &str, agent: &str, branch: &str) -> String {
            format!(
                "  {icon} {} {} {} {} {} {} {branch}",
                ui::fit(unread, 3),
                ui::fit(ws, 9),
                ui::fit(win, 4),
                ui::fit(tab, 4),
                ui::fit(pane, 5),
                ui::fit(agent, 8),
            )
        }
        // The icon column occupies icon(1) + space(1) + badge(9) = 11
        // columns in the body, so the header is given a blank of the same
        // width to line up with it.
        println!(
            "{}",
            row(&" ".repeat(11), "", "WS", "WIN", "TAB", "PANE", "AGENT", "BRANCH")
        );
        for p in &group.panes {
            let unread = if p.unread > 0 {
                format!("●{}", p.unread)
            } else {
                String::new()
            };
            println!(
                "{}",
                row(
                    &format!("{} {}", p.state.icon(), ansi_badge(p.state)),
                    &unread,
                    &p.workspace,
                    &p.window_id.to_string(),
                    &p.tab_id.to_string(),
                    &p.pane_id.to_string(),
                    p.agent.as_deref().unwrap_or("-"),
                    p.branch.as_deref().unwrap_or("-"),
                )
            );
            println!("       task: {}", p.task);
        }
    }
    if app.snapshot.is_empty() {
        println!(
            "{}",
            match app.lang {
                lang::Lang::En => "No agent panes",
                lang::Lang::Ja => "対象のペインがありません",
            }
        );
    }
}

fn run(opts: Options) -> Result<(), String> {
    let mut app = App::new(!opts.watch);
    app.resident = opts.resident;
    app.refresh();
    // The first show has no FocusGained to hang this on (see event_loop's
    // `focused`), so pick up the origin the plugin left right away.
    if app.resident {
        app.on_focus_gained(wezterm::take_dashboard_origin());
    }
    // Check once at startup whether the state directory is safe to use.
    // The hook side can only write to stderr (it must always exit 0 so it
    // never blocks the agent), so this message line is effectively the
    // only place a human ever sees a validation failure. refresh() clears
    // `message` on success, so this must run after it.
    if let Err(e) = paths::ensure_status_dir() {
        app.message = Some(e);
    }
    app.gc_once();
    // Stale-memo GC (spec §5.2). Runs once at startup, same as pane GC
    // (app.gc_once). tab_id can repeat when a tab holds multiple panes,
    // but gc_stale only looks at whichever cwd it finds first, so that's
    // harmless here.
    let live_tabs: Vec<(u64, String)> = app
        .snapshot
        .groups
        .iter()
        .flat_map(|g| g.panes.iter().map(|p| (p.tab_id, p.cwd.clone())))
        .collect();
    memo::gc_stale(&live_tabs);

    let lang = lang::Lang::from_env();
    let mut terminal = setup_terminal().map_err(|e| match lang {
        lang::Lang::En => format!("Failed to initialize the terminal: {e}"),
        lang::Lang::Ja => format!("端末の初期化に失敗: {e}"),
    })?;
    let result = event_loop(&mut terminal, &mut app, &opts);
    restore_terminal(&mut terminal).map_err(|e| match lang {
        lang::Lang::En => format!("Failed to restore the terminal: {e}"),
        lang::Lang::Ja => format!("端末の復元に失敗: {e}"),
    })?;
    result
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // EnableFocusChange = DECSET 1004. The only reliable signal for
    // backing off ticks while the tab is hidden or unfocused (spec §4.4;
    // measurements in §9.2).
    execute!(stdout, EnterAlternateScreen, EnableFocusChange)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    // Forgetting DisableFocusChange leaves the mode set on the terminal.
    execute!(
        terminal.backend_mut(),
        DisableFocusChange,
        LeaveAlternateScreen
    )?;
    disable_raw_mode()?;
    terminal.show_cursor()
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    opts: &Options,
) -> Result<(), String> {
    // The terminal doesn't tell us its focus state at the moment 1004 is
    // enabled, so right after startup we don't actually know whether we're
    // focused. The launcher was just started by the user, so we assume
    // it's focused and start at the fast tick rate (spec §4.4).
    let lang = lang::Lang::from_env();
    let mut focused = true;
    let mut last_tick = Instant::now();

    loop {
        // `narrow` drives the layout-cycling (Tab) branch, so it's
        // recomputed from actual measurements on every draw. `f.area()`
        // here is the same single source of truth used by `ui::draw`.
        let mut narrow = false;
        terminal
            .draw(|f| {
                narrow = f.area().width < ui::NARROW_COLS;
                ui::draw(f, app);
            })
            .map_err(|e| match lang {
                lang::Lang::En => format!("Failed to draw: {e}"),
                lang::Lang::Ja => format!("描画に失敗: {e}"),
            })?;

        let tick = current_tick(opts, focused);
        let mut timeout = tick.saturating_sub(last_tick.elapsed());
        // While waiting for a jump, poll tightly so we react to FocusLost
        // quickly.
        if app.pending_exit.is_some() {
            timeout = timeout.min(Duration::from_millis(20));
        }

        if event::poll(timeout).map_err(|e| match lang {
            lang::Lang::En => format!("Failed to wait for input: {e}"),
            lang::Lang::Ja => format!("入力待ちに失敗: {e}"),
        })? {
            match event::read().map_err(|e| match lang {
                lang::Lang::En => format!("Failed to read input: {e}"),
                lang::Lang::Ja => format!("入力の読み取りに失敗: {e}"),
            })? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_key(app, key, narrow)
                }
                Event::FocusGained => {
                    focused = true;
                    if app.resident {
                        app.on_focus_gained(wezterm::take_dashboard_origin());
                    }
                }
                Event::FocusLost => {
                    focused = false;
                    // Losing focus right after sending a jump is the
                    // signal that the move actually happened.
                    app.on_focus_lost();
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        // The editor takes over the Terminal, which handle_key doesn't
        // have access to, so it can't launch one from there. Here we check
        // the flag and launch it by temporarily leaving the alternate
        // screen (spec §5.3).
        if let Some((tab_id, cwd)) = app.pending_edit.take() {
            if let Err(e) = open_editor(terminal, tab_id, &cwd) {
                app.message = Some(e);
            }
        }

        app.tick_pending_exit();
        if app.should_quit {
            return Ok(());
        }
        // Rebuilding the list while a jump is pending would shift the
        // selection and be confusing, so skip it in that case.
        if app.pending_exit.is_none() && last_tick.elapsed() >= tick {
            app.refresh();
            last_tick = Instant::now();
        }
    }
}

/// Edits a memo with `$VISUAL`/`$EDITOR`/`nvim`/`vi` (spec §5.3).
/// Leaves the alternate screen to hand the terminal to the editor, then
/// redraws once it exits.
fn open_editor(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    tab_id: u64,
    cwd: &str,
) -> Result<(), String> {
    let lang = lang::Lang::from_env();
    let path = memo::ensure_file(tab_id, cwd).map_err(|e| match lang {
        lang::Lang::En => format!("Failed to create the memo: {e}"),
        lang::Lang::Ja => format!("メモの作成に失敗: {e}"),
    })?;

    disable_raw_mode().map_err(|e| match lang {
        lang::Lang::En => format!("Failed to disable raw mode: {e}"),
        lang::Lang::Ja => format!("raw mode 解除に失敗: {e}"),
    })?;
    execute!(
        terminal.backend_mut(),
        DisableFocusChange,
        LeaveAlternateScreen
    )
    .map_err(|e| match lang {
        lang::Lang::En => format!("Failed to leave the alternate screen: {e}"),
        lang::Lang::Ja => format!("代替スクリーンの解除に失敗: {e}"),
    })?;

    let editor = memo::resolve_editor();
    let status = Command::new(&editor).arg(&path).status();

    // Even if the editor fails, always restore the terminal to the TUI's state.
    let resume = (|| -> io::Result<()> {
        execute!(terminal.backend_mut(), EnterAlternateScreen, EnableFocusChange)?;
        enable_raw_mode()?;
        terminal.clear()
    })();

    resume.map_err(|e| match lang {
        lang::Lang::En => format!("Failed to resume the terminal: {e}"),
        lang::Lang::Ja => format!("端末の復帰に失敗: {e}"),
    })?;

    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(match lang {
            lang::Lang::En => format!("{editor} exited abnormally ({s})"),
            lang::Lang::Ja => format!("{editor} が異常終了しました ({s})"),
        }),
        Err(e) => Err(match lang {
            lang::Lang::En => format!("Failed to launch {editor}: {e}"),
            lang::Lang::Ja => format!("{editor} の起動に失敗: {e}"),
        }),
    }
}

fn current_tick(opts: &Options, focused: bool) -> Duration {
    if let Some(d) = opts.interval {
        return d;
    }
    if !opts.watch {
        return TICK_LAUNCHER;
    }
    if focused {
        TICK_WATCH_FOCUSED
    } else {
        TICK_WATCH_BLURRED
    }
}

fn handle_key(app: &mut App, key: KeyEvent, narrow: bool) {
    if app.filter_active {
        match key.code {
            KeyCode::Esc => {
                app.filter.clear();
                app.filter_active = false;
                app.rebuild_rows();
            }
            KeyCode::Enter => app.filter_active = false,
            KeyCode::Backspace => {
                app.filter.pop();
                app.rebuild_rows();
            }
            KeyCode::Char(c) => {
                app.filter.push(c);
                app.rebuild_rows();
            }
            _ => {}
        }
        return;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.quit(),
        KeyCode::Up | KeyCode::Char('k') => app.move_cursor(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_cursor(1),
        KeyCode::Enter => app.jump_to_selected(),
        KeyCode::Char('r') => app.mark_selected_read(),
        KeyCode::Char('R') => app.mark_all_read(),
        KeyCode::Char('g') => app.refresh(),
        KeyCode::Char('/') => {
            app.filter_active = true;
            app.message = None;
        }
        KeyCode::Tab => app.cycle_layout(narrow),
        KeyCode::Char('e') => app.request_edit(),
        _ => {}
    }
}
