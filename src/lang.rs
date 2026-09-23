//! Dashboard display language.
//!
//! Selected via the `WEZTERM_AGENTS_LANG` environment variable, which
//! plugin/init.lua's `apply_to_config(config, { lang = ... })` sets globally
//! (spec: README's "Language" section). Defaults to English so a fresh
//! install needs no configuration; set `lang = 'ja'` to get the original
//! Japanese strings back.
//!
//! Both the TUI (main.rs/app.rs/ui.rs/model.rs) and the hook binary
//! (hook.rs, run as a separate `wezterm-agents hook ...` invocation) read
//! this independently at their own startup — there's no IPC between them,
//! so a hook's already-written `.jsonl` notification text stays in
//! whichever language was active when it fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Ja,
}

impl Lang {
    pub fn from_env() -> Lang {
        match std::env::var("WEZTERM_AGENTS_LANG") {
            Ok(v) if v.eq_ignore_ascii_case("ja") => Lang::Ja,
            _ => Lang::En,
        }
    }
}
