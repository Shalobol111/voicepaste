//! Parsing and validation of hotkey strings such as "Ctrl+Alt+Space" or "F9".

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use global_hotkey::hotkey::{Code, HotKey};

/// If no event arrives for this long while the key is "down", the release was
/// lost and the next press counts as a new one. Key autorepeat keeps sending
/// events (the first one after at most ~1 s), so a real hold never trips this.
const STALE_AFTER: Duration = Duration::from_millis(1500);

/// Drops key autorepeat. Windows sends a WM_HOTKEY for every repeat of a held
/// hotkey, which would toggle recording off again in toggle mode.
#[derive(Debug, Default)]
pub struct PressFilter {
    down_since: Option<Instant>,
    last_event: Option<Instant>,
}

impl PressFilter {
    /// Returns true for a real (first) press, false for an autorepeat.
    pub fn on_pressed(&mut self, now: Instant) -> bool {
        let stale = self
            .last_event
            .is_some_and(|t| now.saturating_duration_since(t) > STALE_AFTER);
        self.last_event = Some(now);
        if self.down_since.is_some() && !stale {
            return false;
        }
        self.down_since = Some(now);
        true
    }

    pub fn on_released(&mut self) {
        self.down_since = None;
        self.last_event = None;
    }
}

fn is_function_key(code: Code) -> bool {
    use Code::*;
    matches!(
        code,
        F1 | F2
            | F3
            | F4
            | F5
            | F6
            | F7
            | F8
            | F9
            | F10
            | F11
            | F12
            | F13
            | F14
            | F15
            | F16
            | F17
            | F18
            | F19
            | F20
            | F21
            | F22
            | F23
            | F24
    )
}

/// Parses a hotkey. It must contain at least one modifier or be an F-key, so a
/// bare letter can never be hijacked system-wide by accident.
pub fn parse(text: &str) -> Result<HotKey> {
    let hk: HotKey = text
        .trim()
        .parse()
        .map_err(|e| anyhow!("invalid hotkey \"{text}\": {e}"))?;
    if hk.mods.is_empty() && !is_function_key(hk.key) {
        bail!("hotkey \"{text}\" needs a modifier (Ctrl/Alt/Shift/Win) or must be an F-key");
    }
    Ok(hk)
}

/// What a hotkey does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Press to start, press again to stop.
    Toggle,
    /// Hold to record, release to stop.
    PushToTalk,
}

impl Mode {
    /// Short Russian name for tooltips and messages.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Toggle => "переключатель",
            Mode::PushToTalk => "удержание",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub mode: Mode,
    pub hotkey: HotKey,
    /// The text as written in the config.
    pub text: String,
}

#[derive(Debug, Default)]
pub struct Resolved {
    pub bindings: Vec<Binding>,
    /// Human-readable problems (invalid text, duplicates, nothing enabled).
    pub errors: Vec<String>,
}

/// Parses both configured hotkeys. An empty string means "disabled". The same
/// combination cannot serve both modes, so the second one is reported.
pub fn resolve(toggle: &str, push_to_talk: &str) -> Resolved {
    let mut out = Resolved::default();
    for (mode, text) in [(Mode::Toggle, toggle), (Mode::PushToTalk, push_to_talk)] {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        match parse(text) {
            Ok(hotkey) => {
                if let Some(first) = out.bindings.iter().find(|b| b.hotkey.id() == hotkey.id()) {
                    out.errors.push(format!(
                        "{} {text}: то же сочетание уже занято режимом «{}»",
                        mode.label(),
                        first.mode.label()
                    ));
                } else {
                    out.bindings.push(Binding {
                        mode,
                        hotkey,
                        text: text.to_string(),
                    });
                }
            }
            Err(e) => out.errors.push(format!("{}: {e:#}", mode.label())),
        }
    }
    if out.bindings.is_empty() && out.errors.is_empty() {
        out.errors.push("обе горячие клавиши отключены".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use global_hotkey::hotkey::Modifiers;

    #[test]
    fn both_hotkeys_resolve_independently() {
        let r = resolve("Ctrl+Alt+D", "Ctrl+Alt+S");
        assert!(r.errors.is_empty());
        assert_eq!(r.bindings.len(), 2);
        assert_eq!(r.bindings[0].mode, Mode::Toggle);
        assert_eq!(r.bindings[1].mode, Mode::PushToTalk);
        assert_ne!(r.bindings[0].hotkey.id(), r.bindings[1].hotkey.id());
    }

    #[test]
    fn empty_string_disables_one_hotkey() {
        let r = resolve("", "F9");
        assert!(r.errors.is_empty());
        assert_eq!(r.bindings.len(), 1);
        assert_eq!(r.bindings[0].mode, Mode::PushToTalk);
        let r = resolve("  ", "");
        assert!(r.bindings.is_empty());
        assert_eq!(r.errors.len(), 1);
    }

    #[test]
    fn same_combination_for_both_modes_is_reported() {
        let r = resolve("Ctrl+Alt+D", "ctrl+alt+d");
        assert_eq!(r.bindings.len(), 1);
        assert_eq!(r.bindings[0].mode, Mode::Toggle);
        assert_eq!(r.errors.len(), 1);
    }

    #[test]
    fn a_bad_hotkey_does_not_disable_the_good_one() {
        let r = resolve("Ctrl++D", "Ctrl+Alt+S");
        assert_eq!(r.bindings.len(), 1);
        assert_eq!(r.bindings[0].mode, Mode::PushToTalk);
        assert_eq!(r.errors.len(), 1);
    }

    #[test]
    fn parses_combinations() {
        let hk = parse("Ctrl+Alt+Space").unwrap();
        assert_eq!(hk.key, Code::Space);
        assert_eq!(hk.mods, Modifiers::CONTROL | Modifiers::ALT);
        let hk = parse("Ctrl+Shift+D").unwrap();
        assert_eq!(hk.key, Code::KeyD);
        assert_eq!(hk.mods, Modifiers::CONTROL | Modifiers::SHIFT);
    }

    #[test]
    fn f_key_alone_is_allowed() {
        let hk = parse("F9").unwrap();
        assert_eq!(hk.key, Code::F9);
        assert!(hk.mods.is_empty());
    }

    #[test]
    fn bare_letter_is_rejected() {
        assert!(parse("D").is_err());
        assert!(parse("Space").is_err());
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse("").is_err());
        assert!(parse("Ctrl+").is_err());
        assert!(parse("Ctrl+Alt+Nonsense").is_err());
        assert!(parse("Ctrl+D+E").is_err());
    }

    #[test]
    fn autorepeat_is_ignored_until_release() {
        let mut f = PressFilter::default();
        let t = Instant::now();
        assert!(f.on_pressed(t));
        assert!(!f.on_pressed(t + Duration::from_millis(500)));
        assert!(!f.on_pressed(t + Duration::from_millis(530)));
        f.on_released();
        assert!(f.on_pressed(t + Duration::from_millis(900)));
    }

    #[test]
    fn duplicate_releases_are_harmless() {
        let mut f = PressFilter::default();
        let t = Instant::now();
        assert!(f.on_pressed(t));
        f.on_released();
        f.on_released();
        f.on_released();
        assert!(f.on_pressed(t + Duration::from_millis(100)));
    }

    #[test]
    fn lost_release_does_not_block_the_hotkey_forever() {
        let mut f = PressFilter::default();
        let t = Instant::now();
        assert!(f.on_pressed(t));
        // No release and no repeats for 3 s: the key must have been let go.
        assert!(f.on_pressed(t + Duration::from_secs(3)));
    }

    #[test]
    fn continuous_hold_stays_blocked_even_when_long() {
        let mut f = PressFilter::default();
        let t = Instant::now();
        assert!(f.on_pressed(t));
        for i in 1..200 {
            assert!(!f.on_pressed(t + Duration::from_millis(30 * i)));
        }
    }
}
