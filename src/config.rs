//! `config.toml`: defaults, tolerant loading, commented saving and a file watcher.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use crossbeam_channel::RecvTimeoutError;
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteMethod {
    CtrlV,
    ShiftInsert,
    Type,
}

impl PasteMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            PasteMethod::CtrlV => "ctrl_v",
            PasteMethod::ShiftInsert => "shift_insert",
            PasteMethod::Type => "type",
        }
    }
}

/// Both hotkeys are active at the same time. An empty string disables one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyCfg {
    /// Press once to start recording, press again to stop.
    pub toggle: String,
    /// Hold to record, release to stop.
    pub push_to_talk: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioCfg {
    /// Empty = default device; otherwise a (partial) device name.
    pub input_device: String,
    pub max_record_seconds: u32,
    pub min_record_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SttCfg {
    pub model: String,
    pub use_gpu: bool,
    /// "auto", "ru" or "en".
    pub language: String,
    pub beam_size: i32,
    pub initial_prompt: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputCfg {
    /// false = clipboard only.
    pub paste: bool,
    pub paste_method: PasteMethod,
    pub paste_delay_ms: u32,
    pub trailing_space: bool,
    pub strip_final_period: bool,
    /// Start the text with a lowercase letter (Whisper capitalises sentences).
    pub lowercase_first: bool,
    /// Press Enter after pasting, e.g. to send a chat message.
    pub press_enter: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiCfg {
    pub show_overlay: bool,
    pub sounds: bool,
    /// Beep volume, 0..=100 percent.
    pub sound_volume: u8,
    pub autostart: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hotkey: HotkeyCfg,
    pub audio: AudioCfg,
    pub stt: SttCfg,
    pub output: OutputCfg,
    pub ui: UiCfg,
}

impl Default for HotkeyCfg {
    fn default() -> Self {
        Self {
            toggle: "Ctrl+Alt+D".into(),
            push_to_talk: "Ctrl+Alt+S".into(),
        }
    }
}

impl Default for AudioCfg {
    fn default() -> Self {
        Self {
            input_device: String::new(),
            max_record_seconds: 300,
            min_record_ms: 300,
        }
    }
}

impl Default for SttCfg {
    fn default() -> Self {
        // The CPU build gets a light model and greedy decoding, so that a dictation
        // takes seconds instead of minutes without a graphics card.
        let gpu = cfg!(feature = "cuda");
        Self {
            model: if gpu {
                "ggml-large-v3-turbo-q5_0.bin"
            } else {
                "ggml-small-q5_1.bin"
            }
            .into(),
            use_gpu: gpu,
            language: "auto".into(),
            beam_size: if gpu { 5 } else { 1 },
            initial_prompt: crate::stt::DEFAULT_PROMPT.into(),
        }
    }
}

impl Default for OutputCfg {
    fn default() -> Self {
        Self {
            paste: true,
            paste_method: PasteMethod::CtrlV,
            paste_delay_ms: 40,
            trailing_space: true,
            strip_final_period: false,
            lowercase_first: false,
            press_enter: false,
        }
    }
}

impl Default for UiCfg {
    fn default() -> Self {
        Self {
            show_overlay: true,
            sounds: true,
            sound_volume: 70,
            autostart: false,
        }
    }
}

impl Config {
    /// Clamps values to sane ranges so a typo cannot break the app.
    pub fn sanitized(mut self) -> Self {
        self.audio.max_record_seconds = self.audio.max_record_seconds.clamp(5, 3600);
        self.audio.min_record_ms = self.audio.min_record_ms.min(5_000);
        self.stt.beam_size = self.stt.beam_size.clamp(1, 10);
        self.output.paste_delay_ms = self.output.paste_delay_ms.min(2_000);
        self.ui.sound_volume = self.ui.sound_volume.min(100);
        if !matches!(self.stt.language.as_str(), "auto" | "ru" | "en") {
            self.stt.language = "auto".into();
        }
        self
    }
}

/// A TOML string literal with proper escaping.
fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

/// Renders the config as a commented `config.toml`. Used for the first-run file
/// and by the settings window, so saving never strips the explanations.
pub fn render(c: &Config) -> String {
    format!(
        r#"# VoicePaste settings. Changes are applied while the app is running.

[hotkey]
# Two hotkeys, both active at the same time. An empty string disables one.
# A hotkey needs a modifier (Ctrl/Alt/Shift/Win) or must be an F-key.
toggle = {toggle}          # press once to start recording, press again to stop
push_to_talk = {ptt}     # hold to record, release to stop

[audio]
input_device = {device}               # "" = Windows default; or part of a device name, e.g. "fifine"
max_record_seconds = {max_s}
min_record_ms = {min_ms}             # shorter recordings are ignored

[stt]
model = {model}   # file in the models folder
use_gpu = {gpu}
language = {lang}               # "auto" | "ru" | "en"
beam_size = {beam}                   # 1 = fastest, 5 = recommended, up to 10 = most accurate (slower)
initial_prompt = {prompt}

[output]
paste = {paste}                    # false = clipboard only
paste_method = {method}         # "ctrl_v" | "shift_insert" (terminals) | "type"
paste_delay_ms = {delay}
trailing_space = {trailing}           # add a space after the text (ignored when press_enter is on)
strip_final_period = {strip}
lowercase_first = {lower}             # start the text with a lowercase letter
press_enter = {enter}                 # press Enter after pasting (sends a chat message)

[ui]
show_overlay = {overlay}
sounds = {sounds}
sound_volume = {volume}            # 0..100 percent
autostart = {autostart}
"#,
        toggle = toml_str(&c.hotkey.toggle),
        ptt = toml_str(&c.hotkey.push_to_talk),
        device = toml_str(&c.audio.input_device),
        max_s = c.audio.max_record_seconds,
        min_ms = c.audio.min_record_ms,
        model = toml_str(&c.stt.model),
        gpu = c.stt.use_gpu,
        lang = toml_str(&c.stt.language),
        beam = c.stt.beam_size,
        prompt = toml_str(&c.stt.initial_prompt),
        paste = c.output.paste,
        method = toml_str(c.output.paste_method.as_str()),
        delay = c.output.paste_delay_ms,
        trailing = c.output.trailing_space,
        strip = c.output.strip_final_period,
        lower = c.output.lowercase_first,
        enter = c.output.press_enter,
        overlay = c.ui.show_overlay,
        sounds = c.ui.sounds,
        volume = c.ui.sound_volume,
        autostart = c.ui.autostart,
    )
}

/// Creates the config file with defaults if it does not exist.
pub fn ensure_exists(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    fs::write(path, render(&Config::default()))
        .with_context(|| format!("cannot write {}", path.display()))
}

/// Parses config text. Unknown keys (including the old `mode`) are ignored,
/// missing ones use defaults.
pub fn parse(text: &str) -> Result<Config> {
    let cfg: Config = toml::from_str(text).context("invalid config.toml")?;
    Ok(cfg.sanitized())
}

/// Reads the config. On any error returns defaults plus the error text; the
/// broken file is never overwritten.
pub fn load(path: &Path) -> (Config, Option<String>) {
    match fs::read_to_string(path)
        .with_context(|| format!("cannot read {}", path.display()))
        .and_then(|t| parse(&t))
    {
        Ok(cfg) => (cfg, None),
        Err(e) => (Config::default(), Some(format!("{e:#}"))),
    }
}

/// Writes through a temporary file and renames it, so readers never see a half-written file.
pub fn save_atomic(path: &Path, cfg: &Config) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, render(cfg)).with_context(|| format!("cannot write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))
}

/// Watches `config.toml` and calls `on_change` once per burst of file events.
/// The returned watcher must be kept alive.
pub fn watch<F>(path: &Path, on_change: F) -> Result<notify::RecommendedWatcher>
where
    F: Fn() + Send + 'static,
{
    let file_name = path.file_name().map(|n| n.to_owned());
    let dir: PathBuf = path.parent().context("config path has no parent")?.into();
    let (tx, rx) = crossbeam_channel::unbounded::<()>();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            if ev
                .paths
                .iter()
                .any(|p| p.file_name() == file_name.as_deref())
            {
                let _ = tx.send(());
            }
        }
    })
    .context("cannot create file watcher")?;
    // Watch the directory: editors replace files instead of writing them in place.
    watcher
        .watch(&dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("cannot watch {}", dir.display()))?;

    // Debounce: a single save usually produces several events.
    std::thread::Builder::new()
        .name("config-watch".into())
        .spawn(move || {
            while rx.recv().is_ok() {
                loop {
                    match rx.recv_timeout(Duration::from_millis(250)) {
                        Ok(()) => continue,
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                on_change();
            }
        })
        .context("cannot spawn watcher thread")?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_defaults_parse_back_to_defaults() {
        assert_eq!(
            parse(&render(&Config::default())).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn render_roundtrips_awkward_strings() {
        let mut cfg = Config::default();
        cfg.audio.input_device = "Mic \"Pro\" \\ test".into();
        cfg.stt.initial_prompt = "Строка 1\nСтрока 2 с «кавычками» и \"двойными\"".into();
        cfg.hotkey.toggle = String::new();
        cfg.output.paste_method = PasteMethod::ShiftInsert;
        cfg.ui.autostart = true;
        assert_eq!(parse(&render(&cfg)).unwrap(), cfg);
    }

    #[test]
    fn rendered_file_keeps_explanatory_comments() {
        let text = render(&Config::default());
        assert!(text.contains("press once to start recording"));
        assert!(text.contains("hold to record"));
    }

    #[test]
    fn sound_volume_roundtrips_and_is_clamped() {
        let mut cfg = Config::default();
        cfg.ui.sound_volume = 35;
        assert_eq!(parse(&render(&cfg)).unwrap().ui.sound_volume, 35);
        assert_eq!(
            parse("[ui]\nsound_volume = 200\n").unwrap().ui.sound_volume,
            100
        );
        // Old config files without the key get the default.
        assert_eq!(parse("[ui]\nsounds = true\n").unwrap().ui.sound_volume, 70);
    }

    #[test]
    fn new_output_options_default_to_off_and_roundtrip() {
        let defaults = Config::default();
        assert!(!defaults.output.lowercase_first && !defaults.output.press_enter);
        // Old files without the keys keep working.
        let old = parse("[output]\npaste = true\n").unwrap();
        assert!(!old.output.lowercase_first && !old.output.press_enter);

        let mut cfg = Config::default();
        cfg.output.lowercase_first = true;
        cfg.output.press_enter = true;
        assert_eq!(parse(&render(&cfg)).unwrap(), cfg);
    }

    #[test]
    fn defaults_use_two_different_hotkeys() {
        let h = HotkeyCfg::default();
        assert_ne!(h.toggle, h.push_to_talk);
    }

    #[test]
    fn partial_file_fills_in_defaults() {
        let cfg = parse("[hotkey]\ntoggle = \"F9\"\n").unwrap();
        assert_eq!(cfg.hotkey.toggle, "F9");
        assert_eq!(cfg.hotkey.push_to_talk, HotkeyCfg::default().push_to_talk);
        assert_eq!(cfg.stt, SttCfg::default());
    }

    #[test]
    fn old_mode_key_is_ignored() {
        let cfg = parse("[hotkey]\nmode = \"push_to_talk\"\ntoggle = \"F8\"\n").unwrap();
        assert_eq!(cfg.hotkey.toggle, "F8");
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let cfg = parse("[audio]\nfuture_option = 1\nmin_record_ms = 500\n").unwrap();
        assert_eq!(cfg.audio.min_record_ms, 500);
    }

    #[test]
    fn broken_file_gives_defaults_and_error() {
        let dir = std::env::temp_dir().join("voicepaste_cfg_broken_test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, "[hotkey\nmode = ").unwrap();
        let (cfg, err) = load(&path);
        assert_eq!(cfg, Config::default());
        assert!(err.is_some());
        // The broken file must be left untouched.
        assert_eq!(fs::read_to_string(&path).unwrap(), "[hotkey\nmode = ");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn values_are_clamped() {
        let cfg =
            parse("[stt]\nbeam_size = 99\nlanguage = \"xx\"\n[audio]\nmax_record_seconds = 1\n")
                .unwrap();
        assert_eq!(cfg.stt.beam_size, 10);
        assert_eq!(cfg.stt.language, "auto");
        assert_eq!(cfg.audio.max_record_seconds, 5);
    }

    #[test]
    fn save_atomic_roundtrip() {
        let dir = std::env::temp_dir().join("voicepaste_cfg_save_test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut cfg = Config::default();
        cfg.hotkey.push_to_talk = "F9".into();
        cfg.audio.input_device = "fifine".into();
        save_atomic(&path, &cfg).unwrap();
        assert_eq!(load(&path).0, cfg);
        assert!(!dir.join("config.toml.tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
