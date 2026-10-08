//! Application core: a pure state machine (`transition`) plus the winit event
//! loop that executes its effects.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tray_icon::menu::MenuEvent;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

use crate::audio::Recorder;
use crate::autostart;
use crate::config::{self, Config};
use crate::hotkey::{self, Mode, PressFilter};
use crate::output::{self, DeliverOptions};
use crate::overlay::{Overlay, View};
use crate::postprocess;
use crate::sound::{self, Sound};
use crate::stt::{self, Job, SttConfig, WorkerMsg};
use crate::tray::{IconKind, MenuAction, Tray};

// ---------------------------------------------------------------- state machine

#[derive(Debug, Clone)]
pub struct Settings {
    pub min_record: Duration,
    pub max_record: Duration,
    /// Empty = default device; otherwise a (partial) device name.
    pub input_device: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_config(&Config::default(), None)
    }
}

impl Settings {
    pub fn from_config(cfg: &Config, device_override: Option<&str>) -> Self {
        Self {
            min_record: Duration::from_millis(cfg.audio.min_record_ms as u64),
            max_record: Duration::from_secs(cfg.audio.max_record_seconds as u64),
            input_device: device_override
                .map(str::to_owned)
                .unwrap_or_else(|| cfg.audio.input_device.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Loading,
    Idle,
    /// `mode` is the hotkey that started the recording; only that hotkey stops it.
    Recording {
        started: Instant,
        mode: Mode,
    },
    Transcribing,
    Error(String),
}

#[derive(Debug, Clone)]
pub enum Event {
    ModelReady,
    /// The worker started reloading the model after a settings change.
    ModelReloading,
    ModelFailed(String),
    HotkeyPressed(Mode, Instant),
    HotkeyReleased(Mode, Instant),
    MaxDurationReached(Instant),
    StartFailed(String),
    Transcribed(Result<String, String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    StartRecording,
    StopAndTranscribe,
    StopAndDiscard,
    Paste(String),
    Log(String),
    Sound(Sound),
}

fn log(msg: impl Into<String>) -> Effect {
    Effect::Log(msg.into())
}

/// Pure: no I/O, no clocks. Time comes in through the events.
pub fn transition(state: &State, event: Event, s: &Settings) -> (State, Vec<Effect>) {
    use Event::*;
    let stay = || (state.clone(), vec![]);

    // Stops a recording that started at `started`; short ones are discarded.
    let stop = |started: Instant, now: Instant| {
        if now.saturating_duration_since(started) < s.min_record {
            (
                State::Idle,
                vec![
                    Effect::StopAndDiscard,
                    log("recording too short, discarded"),
                ],
            )
        } else {
            (State::Transcribing, vec![Effect::StopAndTranscribe])
        }
    };

    match (state, event) {
        (State::Loading, ModelReady) => (State::Idle, vec![log("model ready")]),
        (State::Idle | State::Error(_), ModelReloading) => {
            (State::Loading, vec![log("reloading model")])
        }
        (State::Loading, ModelFailed(e)) => (
            State::Error(e.clone()),
            vec![log(format!("model failed to load: {e}"))],
        ),
        (State::Loading, HotkeyPressed(..)) => (
            State::Loading,
            vec![
                log("model is still loading, hotkey ignored"),
                Effect::Sound(Sound::Error),
            ],
        ),

        // Either hotkey starts a recording and remembers which one did.
        (State::Idle, HotkeyPressed(mode, now)) => (
            State::Recording { started: now, mode },
            vec![Effect::StartRecording],
        ),

        // Only the hotkey that started the recording can stop it. The other one,
        // and key autorepeat of a held push-to-talk key, are ignored.
        (
            State::Recording {
                started,
                mode: Mode::Toggle,
            },
            HotkeyPressed(Mode::Toggle, now),
        ) => stop(*started, now),
        (
            State::Recording {
                started,
                mode: Mode::PushToTalk,
            },
            HotkeyReleased(Mode::PushToTalk, now),
        ) => stop(*started, now),
        (State::Recording { started, .. }, MaxDurationReached(now)) => {
            let (st, mut fx) = stop(*started, now);
            fx.insert(0, log("max recording length reached"));
            (st, fx)
        }
        (State::Recording { .. }, StartFailed(e)) => (
            State::Idle,
            vec![
                log(format!("cannot start recording: {e}")),
                Effect::Sound(Sound::Error),
            ],
        ),

        (State::Transcribing, HotkeyPressed(..)) => (
            State::Transcribing,
            vec![
                log("busy transcribing, hotkey ignored"),
                Effect::Sound(Sound::Error),
            ],
        ),
        (State::Transcribing, Transcribed(Ok(text))) if !text.is_empty() => {
            (State::Idle, vec![Effect::Paste(text)])
        }
        (State::Transcribing, Transcribed(Ok(_))) => (
            State::Idle,
            vec![
                log("no speech recognized, nothing pasted"),
                Effect::Sound(Sound::Error),
            ],
        ),
        (State::Transcribing, Transcribed(Err(e))) => (
            State::Idle,
            vec![
                log(format!("recognition failed: {e}")),
                Effect::Sound(Sound::Error),
            ],
        ),

        (State::Error(e), HotkeyPressed(..)) => (
            State::Error(e.clone()),
            vec![
                log(format!("unavailable: {e}")),
                Effect::Sound(Sound::Error),
            ],
        ),

        _ => stay(),
    }
}

// ------------------------------------------------------------------- event loop

/// Everything the app needs from the command line.
pub struct RunOptions {
    pub config_path: PathBuf,
    pub models_dir: PathBuf,
    pub logs_dir: PathBuf,
    /// `--model <path>`: wins over `stt.model` from the config.
    pub model_override: Option<PathBuf>,
    /// `--device <name>`: wins over `audio.input_device`.
    pub device_override: Option<String>,
    /// `--dump-audio <dir>`: debugging aid.
    pub dump_dir: Option<PathBuf>,
}

#[derive(Debug)]
enum UserEvent {
    Hotkey(GlobalHotKeyEvent),
    Worker(WorkerMsg),
    ConfigChanged,
    Menu(MenuEvent),
}

/// A hotkey that is currently registered with the OS.
#[derive(Debug, Clone)]
struct Registered {
    mode: Mode,
    hotkey: HotKey,
    text: String,
}

struct App {
    opts: RunOptions,
    cfg: Config,
    /// Set when the config file could not be parsed (the last good config stays active).
    config_error: Option<String>,
    state: State,
    settings: Settings,
    deliver: DeliverOptions,
    hotkeys: GlobalHotKeyManager,
    registered: Vec<Registered>,
    /// Problems with the configured hotkeys (invalid, duplicate, taken by another app).
    hotkey_errors: Vec<String>,
    recorder: Option<Recorder>,
    /// One autorepeat filter per mode: index 0 = toggle, 1 = push-to-talk.
    press_filters: [PressFilter; 2],
    /// When the current recognition job was sent, for the latency log.
    transcribe_started: Option<Instant>,
    jobs: Sender<Job>,
    tray: Option<Tray>,
    overlay: Option<Overlay>,
    shown: Option<(IconKind, String)>,
    // Must stay alive, otherwise config changes are no longer noticed.
    _watcher: Option<notify::RecommendedWatcher>,
}

fn filter_index(mode: Mode) -> usize {
    match mode {
        Mode::Toggle => 0,
        Mode::PushToTalk => 1,
    }
}

impl App {
    fn stt_config(&self) -> SttConfig {
        let mut c = SttConfig::from_config(&self.cfg, &self.opts.models_dir);
        if let Some(m) = &self.opts.model_override {
            c.model_path = m.clone();
        }
        c.dump_dir = self.opts.dump_dir.clone();
        c
    }

    fn dispatch(&mut self, event: Event) {
        let (next, effects) = transition(&self.state, event, &self.settings);
        self.state = next;
        for effect in effects {
            self.execute(effect);
        }
        self.refresh_tray();
        self.refresh_overlay();
    }

    fn beep(&self, sound: Sound) {
        if self.cfg.ui.sounds {
            sound::play(sound, self.cfg.ui.sound_volume);
        }
    }

    /// Shows the status plate for Recording / Transcribing, hides it otherwise.
    fn refresh_overlay(&mut self) {
        let view = if !self.cfg.ui.show_overlay {
            View::Hidden
        } else {
            match self.state {
                State::Recording { started, .. } => View::Recording { started },
                State::Transcribing => View::Transcribing {
                    since: self.transcribe_started.unwrap_or_else(Instant::now),
                },
                _ => View::Hidden,
            }
        };
        if let Some(overlay) = &mut self.overlay {
            overlay.set(view);
        }
    }

    fn execute(&mut self, effect: Effect) {
        match effect {
            Effect::Log(msg) => tracing::info!("{msg}"),
            Effect::Sound(sound) => self.beep(sound),
            Effect::StartRecording => {
                let max = self.settings.max_record.as_secs() as u32;
                let t = Instant::now();
                match Recorder::start(&self.settings.input_device, max) {
                    Ok(rec) => {
                        tracing::info!(
                            "recording from \"{}\" (stream opened in {} ms)",
                            rec.device_name,
                            t.elapsed().as_millis()
                        );
                        self.recorder = Some(rec);
                        self.beep(Sound::Start);
                    }
                    Err(e) => self.dispatch(Event::StartFailed(format!("{e:#}"))),
                }
            }
            Effect::StopAndDiscard => {
                drop(self.recorder.take());
                self.beep(Sound::Error);
            }
            Effect::StopAndTranscribe => {
                let Some(rec) = self.recorder.take() else {
                    return self.dispatch(Event::Transcribed(Err("no active recording".into())));
                };
                match rec.stop() {
                    Ok(captured) => {
                        tracing::info!(
                            "recording stopped ({:.1} s), transcribing",
                            captured.mono.len() as f32 / captured.rate as f32
                        );
                        self.transcribe_started = Some(Instant::now());
                        self.beep(Sound::Stop);
                        if self.jobs.send(Job::Transcribe(captured)).is_err() {
                            self.dispatch(Event::Transcribed(Err(
                                "recognition thread is gone".into()
                            )));
                        }
                    }
                    Err(e) => self.dispatch(Event::Transcribed(Err(format!("{e:#}")))),
                }
            }
            Effect::Paste(text) => {
                let opts = self.deliver.clone();
                // Waiting for modifiers and pasting must not block the event loop.
                std::thread::spawn(move || {
                    if let Err(e) = output::deliver(&text, &opts) {
                        tracing::error!("{e:#}");
                    }
                });
            }
        }
    }

    // ---- tray ----

    /// "Ctrl+Alt+D · удержание Ctrl+Alt+S" for the tooltip.
    fn hotkey_summary(&self) -> String {
        self.registered
            .iter()
            .map(|r| match r.mode {
                Mode::Toggle => r.text.clone(),
                Mode::PushToTalk => format!("удержание {}", r.text),
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    fn status(&self) -> (IconKind, String) {
        let (kind, mut text) = match &self.state {
            State::Error(e) => (IconKind::Error, format!("Ошибка: {e}")),
            _ if !self.hotkey_errors.is_empty() => (
                IconKind::Error,
                format!("Горячие клавиши: {}", self.hotkey_errors.join("; ")),
            ),
            State::Loading => (IconKind::Busy, "VoicePaste — загрузка модели…".into()),
            State::Recording { .. } => (IconKind::Recording, "VoicePaste — запись…".into()),
            State::Transcribing => (IconKind::Busy, "VoicePaste — распознаю…".into()),
            State::Idle => (
                IconKind::Idle,
                format!("VoicePaste — готово ({})", self.hotkey_summary()),
            ),
        };
        if self.config_error.is_some() {
            text.push_str(" | ошибка в config.toml, см. лог");
        }
        // The tooltip is limited to 127 UTF-16 units on Windows.
        (kind, text.chars().take(120).collect())
    }

    fn refresh_tray(&mut self) {
        let status = self.status();
        if self.shown.as_ref() == Some(&status) {
            return;
        }
        if let Some(tray) = &self.tray {
            tray.set_status(status.0, &status.1);
            self.shown = Some(status);
        }
    }

    // ---- config ----

    /// Registers both configured hotkeys. A hotkey that cannot be registered keeps
    /// the previous one of its mode, so a typo never leaves the app without keys.
    fn sync_hotkeys(&mut self) {
        let resolved = hotkey::resolve(&self.cfg.hotkey.toggle, &self.cfg.hotkey.push_to_talk);
        let mut errors = resolved.errors;
        let mut next: Vec<Registered> = Vec::new();
        let mut kept_old: Vec<Registered> = Vec::new();

        for b in resolved.bindings {
            // Already registered (maybe under the other mode): just relabel it.
            if let Some(existing) = self
                .registered
                .iter()
                .find(|r| r.hotkey.id() == b.hotkey.id())
            {
                next.push(Registered {
                    mode: b.mode,
                    hotkey: existing.hotkey,
                    text: b.text,
                });
                continue;
            }
            match self.hotkeys.register(b.hotkey) {
                Ok(()) => {
                    tracing::info!("hotkey {} ({}) registered", b.text, b.mode.label());
                    next.push(Registered {
                        mode: b.mode,
                        hotkey: b.hotkey,
                        text: b.text,
                    });
                }
                Err(e) => {
                    tracing::error!("cannot register hotkey {}: {e}", b.text);
                    errors.push(format!(
                        "{} {}: занято другим приложением?",
                        b.mode.label(),
                        b.text
                    ));
                    if let Some(old) = self.registered.iter().find(|r| r.mode == b.mode) {
                        kept_old.push(old.clone());
                    }
                }
            }
        }

        // Release the OS hotkeys that are no longer wanted.
        for old in &self.registered {
            let wanted = next
                .iter()
                .chain(kept_old.iter())
                .any(|r| r.hotkey.id() == old.hotkey.id());
            if !wanted {
                let _ = self.hotkeys.unregister(old.hotkey);
            }
        }
        for old in kept_old {
            if !next.iter().any(|r| r.hotkey.id() == old.hotkey.id()) {
                next.push(old);
            }
        }
        for e in &errors {
            tracing::error!("hotkey problem: {e}");
        }
        self.registered = next;
        self.hotkey_errors = errors;
    }

    /// Makes the Windows autostart entry match the setting (also refreshes the exe path
    /// if the program was moved).
    fn sync_autostart(&self) {
        if let Err(e) = autostart::set_enabled(self.cfg.ui.autostart) {
            tracing::warn!("autostart: {e:#}");
        }
    }

    fn apply_config(&mut self, new: Config) {
        let old = std::mem::replace(&mut self.cfg, new);
        self.settings = Settings::from_config(&self.cfg, self.opts.device_override.as_deref());
        self.deliver = deliver_options(&self.cfg);
        if old.hotkey != self.cfg.hotkey {
            self.sync_hotkeys();
        }
        if old.ui.autostart != self.cfg.ui.autostart {
            self.sync_autostart();
        }
        if old.stt != self.cfg.stt {
            let _ = self.jobs.send(Job::Reconfigure(self.stt_config()));
        }
        self.refresh_tray();
        self.refresh_overlay();
    }

    fn reload_config(&mut self) {
        let text = std::fs::read_to_string(&self.opts.config_path)
            .map_err(anyhow::Error::from)
            .and_then(|t| config::parse(&t));
        match text {
            Ok(new) => {
                self.config_error = None;
                if new != self.cfg {
                    tracing::info!("config.toml changed, applying");
                    self.apply_config(new);
                } else {
                    self.refresh_tray();
                }
            }
            Err(e) => {
                // Keep the last good settings instead of falling back to defaults.
                tracing::error!("config.toml not applied: {e:#}");
                self.config_error = Some(format!("{e:#}"));
                self.refresh_tray();
            }
        }
    }

    fn open_in_shell(program: &str, path: &std::path::Path) {
        if let Err(e) = std::process::Command::new(program).arg(path).spawn() {
            tracing::error!("cannot open {}: {e}", path.display());
        }
    }

    /// The settings window is a separate process (see AGENTS.md 3.3).
    fn open_settings() {
        let result = std::env::current_exe()
            .context("cannot find own exe")
            .and_then(|exe| {
                std::process::Command::new(exe)
                    .arg("--settings")
                    .spawn()
                    .context("cannot start settings window")
            });
        if let Err(e) = result {
            tracing::error!("{e:#}");
        }
    }

    fn on_menu(&mut self, el: &ActiveEventLoop, event: MenuEvent) {
        let Some(action) = self.tray.as_ref().and_then(|t| t.action(&event.id)) else {
            return;
        };
        match action {
            MenuAction::Settings => Self::open_settings(),
            MenuAction::OpenConfig => Self::open_in_shell("notepad", &self.opts.config_path),
            MenuAction::OpenModels => {
                let _ = std::fs::create_dir_all(&self.opts.models_dir);
                Self::open_in_shell("explorer", &self.opts.models_dir);
            }
            MenuAction::OpenLogs => {
                let _ = std::fs::create_dir_all(&self.opts.logs_dir);
                Self::open_in_shell("explorer", &self.opts.logs_dir);
            }
            MenuAction::Quit => el.exit(),
        }
    }
}

fn deliver_options(cfg: &Config) -> DeliverOptions {
    DeliverOptions {
        paste: cfg.output.paste,
        method: cfg.output.paste_method,
        paste_delay: Duration::from_millis(cfg.output.paste_delay_ms as u64),
        press_enter: cfg.output.press_enter,
        ..DeliverOptions::default()
    }
}

fn postprocess_options(cfg: &Config) -> postprocess::Options {
    postprocess::Options {
        // A message that is sent with Enter must not end with a stray space.
        trailing_space: cfg.output.trailing_space && !cfg.output.press_enter,
        strip_final_period: cfg.output.strip_final_period,
        lowercase_first: cfg.output.lowercase_first,
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, _el: &ActiveEventLoop) {
        // The tray icon must be created on the event loop thread.
        if self.tray.is_none() {
            match Tray::new("VoicePaste — загрузка модели…") {
                Ok(tray) => {
                    self.tray = Some(tray);
                    self.refresh_tray();
                }
                Err(e) => tracing::error!("tray icon unavailable: {e:#}"),
            }
        }
        if self.overlay.is_none() {
            match Overlay::new() {
                Ok(o) => {
                    tracing::debug!("recording indicator ready");
                    self.overlay = Some(o);
                }
                Err(e) => tracing::warn!("recording indicator unavailable: {e:#}"),
            }
        }
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, _e: WindowEvent) {}

    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Hotkey(e) => {
                let Some(mode) = self
                    .registered
                    .iter()
                    .find(|r| r.hotkey.id() == e.id)
                    .map(|r| r.mode)
                else {
                    return;
                };
                let now = Instant::now();
                let filter = &mut self.press_filters[filter_index(mode)];
                match e.state {
                    HotKeyState::Pressed => {
                        // Held keys auto-repeat; only the first press counts.
                        if filter.on_pressed(now) {
                            self.dispatch(Event::HotkeyPressed(mode, now));
                        }
                    }
                    HotKeyState::Released => {
                        filter.on_released();
                        self.dispatch(Event::HotkeyReleased(mode, now));
                    }
                }
            }
            UserEvent::Worker(WorkerMsg::Ready) => self.dispatch(Event::ModelReady),
            UserEvent::Worker(WorkerMsg::Reloading) => self.dispatch(Event::ModelReloading),
            UserEvent::Worker(WorkerMsg::LoadFailed(e)) => self.dispatch(Event::ModelFailed(e)),
            UserEvent::Worker(WorkerMsg::Text(r)) => {
                if let Some(t) = self.transcribe_started {
                    tracing::info!("recognized in {} ms", t.elapsed().as_millis());
                }
                // Post-processing depends on live settings, so it happens here, not in the worker.
                let opts = postprocess_options(&self.cfg);
                let result = r.map(|raw| postprocess::finalize(&raw, &opts));
                self.dispatch(Event::Transcribed(result));
                self.transcribe_started = None;
            }
            UserEvent::ConfigChanged => self.reload_config(),
            UserEvent::Menu(e) => self.on_menu(el, e),
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        // Enforce the maximum recording length without a timer thread.
        if let State::Recording { started, .. } = self.state {
            if now >= started + self.settings.max_record {
                self.dispatch(Event::MaxDurationReached(now));
            }
        }
        if let Some(overlay) = &mut self.overlay {
            overlay.tick(now);
        }
        // Sleep until the earliest of: recording deadline, next timer/animation frame.
        let mut wake: Option<Instant> = None;
        let mut want = |t: Instant| wake = Some(wake.map_or(t, |w| w.min(t)));
        if let State::Recording { started, .. } = self.state {
            want(started + self.settings.max_record);
        }
        if let Some(t) = self.overlay.as_ref().and_then(|o| o.next_wake(now)) {
            want(t);
        }
        el.set_control_flow(wake.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        // Release the microphone and the tray icon before the process ends.
        self.recorder = None;
        self.overlay = None;
        self.tray = None;
    }
}

pub fn run(opts: RunOptions) -> Result<()> {
    if let Err(e) = config::ensure_exists(&opts.config_path) {
        tracing::warn!("{e:#}");
    }
    let (cfg, config_error) = config::load(&opts.config_path);
    if let Some(e) = &config_error {
        tracing::error!("config.toml is invalid, using defaults: {e}");
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .context("cannot create event loop")?;
    let proxy = event_loop.create_proxy();

    let hotkeys = GlobalHotKeyManager::new().context("cannot create hotkey manager")?;
    let hk_proxy = proxy.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |e| {
        let _ = hk_proxy.send_event(UserEvent::Hotkey(e));
    }));
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = menu_proxy.send_event(UserEvent::Menu(e));
    }));

    let cfg_proxy = proxy.clone();
    let watcher = match config::watch(&opts.config_path, move || {
        let _ = cfg_proxy.send_event(UserEvent::ConfigChanged);
    }) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!("config changes will not be picked up automatically: {e:#}");
            None
        }
    };

    let mut app = App {
        settings: Settings::from_config(&cfg, opts.device_override.as_deref()),
        deliver: deliver_options(&cfg),
        cfg,
        config_error,
        state: State::Loading,
        hotkeys,
        registered: Vec::new(),
        hotkey_errors: Vec::new(),
        recorder: None,
        press_filters: Default::default(),
        transcribe_started: None,
        // Replaced right below, once the worker exists.
        jobs: crossbeam_channel::unbounded().0,
        tray: None,
        overlay: None,
        shown: None,
        _watcher: watcher,
        opts,
    };
    app.sync_hotkeys();
    app.sync_autostart();
    app.jobs = stt::spawn_worker(app.stt_config(), move |msg| {
        let _ = proxy.send_event(UserEvent::Worker(msg));
    })?;

    tracing::info!(
        "started: hotkeys [{}], config {}",
        app.hotkey_summary(),
        app.opts.config_path.display()
    );
    event_loop.run_app(&mut app).context("event loop failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn step(state: State, ev: Event) -> (State, Vec<Effect>) {
        transition(&state, ev, &Settings::default())
    }

    fn rec(started: Instant, mode: Mode) -> State {
        State::Recording { started, mode }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn loading_to_idle_and_error() {
        assert_eq!(step(State::Loading, Event::ModelReady).0, State::Idle);
        let (st, _) = step(State::Loading, Event::ModelFailed("boom".into()));
        assert_eq!(st, State::Error("boom".into()));
    }

    #[test]
    fn hotkey_ignored_while_loading_and_in_error() {
        for mode in [Mode::Toggle, Mode::PushToTalk] {
            let (st, fx) = step(State::Loading, Event::HotkeyPressed(mode, t0()));
            assert_eq!(st, State::Loading);
            assert!(!fx.contains(&Effect::StartRecording));
            let (st, fx) = step(State::Error("x".into()), Event::HotkeyPressed(mode, t0()));
            assert_eq!(st, State::Error("x".into()));
            assert!(!fx.contains(&Effect::StartRecording));
        }
    }

    #[test]
    fn toggle_starts_then_stops_and_transcribes() {
        let start = t0();
        let (st, fx) = step(State::Idle, Event::HotkeyPressed(Mode::Toggle, start));
        assert_eq!(st, rec(start, Mode::Toggle));
        assert_eq!(fx, vec![Effect::StartRecording]);

        let (st, fx) = step(st, Event::HotkeyPressed(Mode::Toggle, start + secs(2)));
        assert_eq!(st, State::Transcribing);
        assert_eq!(fx, vec![Effect::StopAndTranscribe]);
    }

    #[test]
    fn toggle_recording_ignores_release() {
        let start = t0();
        let st = rec(start, Mode::Toggle);
        let (next, fx) = step(
            st.clone(),
            Event::HotkeyReleased(Mode::Toggle, start + secs(2)),
        );
        assert_eq!(next, st);
        assert!(fx.is_empty());
    }

    #[test]
    fn push_to_talk_press_release_cycle() {
        let start = t0();
        let (st, fx) = step(State::Idle, Event::HotkeyPressed(Mode::PushToTalk, start));
        assert_eq!(st, rec(start, Mode::PushToTalk));
        assert_eq!(fx, vec![Effect::StartRecording]);
        let (st, fx) = step(st, Event::HotkeyReleased(Mode::PushToTalk, start + secs(2)));
        assert_eq!(st, State::Transcribing);
        assert_eq!(fx, vec![Effect::StopAndTranscribe]);
    }

    #[test]
    fn push_to_talk_ignores_a_second_press() {
        let start = t0();
        let st = rec(start, Mode::PushToTalk);
        let (next, fx) = step(
            st.clone(),
            Event::HotkeyPressed(Mode::PushToTalk, start + secs(1)),
        );
        assert_eq!(next, st);
        assert!(fx.is_empty());
    }

    #[test]
    fn toggle_recording_is_not_stopped_by_the_push_to_talk_key() {
        let start = t0();
        let st = rec(start, Mode::Toggle);
        for ev in [
            Event::HotkeyPressed(Mode::PushToTalk, start + secs(2)),
            Event::HotkeyReleased(Mode::PushToTalk, start + secs(2)),
        ] {
            let (next, fx) = step(st.clone(), ev);
            assert_eq!(next, st);
            assert!(fx.is_empty());
        }
    }

    #[test]
    fn push_to_talk_recording_is_not_stopped_by_the_toggle_key() {
        let start = t0();
        let st = rec(start, Mode::PushToTalk);
        for ev in [
            Event::HotkeyPressed(Mode::Toggle, start + secs(2)),
            Event::HotkeyReleased(Mode::Toggle, start + secs(2)),
        ] {
            let (next, fx) = step(st.clone(), ev);
            assert_eq!(next, st);
            assert!(fx.is_empty());
        }
    }

    #[test]
    fn both_hotkeys_work_one_after_another() {
        let t = t0();
        // Dictate with the toggle key...
        let (st, _) = step(State::Idle, Event::HotkeyPressed(Mode::Toggle, t));
        let (st, _) = step(st, Event::HotkeyPressed(Mode::Toggle, t + secs(2)));
        let (st, fx) = step(st, Event::Transcribed(Ok("one ".into())));
        assert_eq!(st, State::Idle);
        assert_eq!(fx, vec![Effect::Paste("one ".into())]);
        // ...then with the hold key, without switching anything.
        let (st, _) = step(st, Event::HotkeyPressed(Mode::PushToTalk, t + secs(5)));
        let (st, _) = step(st, Event::HotkeyReleased(Mode::PushToTalk, t + secs(8)));
        assert_eq!(st, State::Transcribing);
    }

    #[test]
    fn short_recording_is_discarded() {
        let start = t0();
        for (mode, ev) in [
            (
                Mode::Toggle,
                Event::HotkeyPressed(Mode::Toggle, start + Duration::from_millis(100)),
            ),
            (
                Mode::PushToTalk,
                Event::HotkeyReleased(Mode::PushToTalk, start + Duration::from_millis(100)),
            ),
        ] {
            let (st, fx) = step(rec(start, mode), ev);
            assert_eq!(st, State::Idle);
            assert!(fx.contains(&Effect::StopAndDiscard));
            assert!(!fx.contains(&Effect::StopAndTranscribe));
        }
    }

    #[test]
    fn hotkey_ignored_while_transcribing() {
        for mode in [Mode::Toggle, Mode::PushToTalk] {
            let (st, fx) = step(State::Transcribing, Event::HotkeyPressed(mode, t0()));
            assert_eq!(st, State::Transcribing);
            assert!(
                !fx.contains(&Effect::StartRecording) && !fx.contains(&Effect::StopAndTranscribe)
            );
        }
    }

    #[test]
    fn transcribed_text_is_pasted() {
        let (st, fx) = step(
            State::Transcribing,
            Event::Transcribed(Ok("Привет ".into())),
        );
        assert_eq!(st, State::Idle);
        assert_eq!(fx, vec![Effect::Paste("Привет ".into())]);
    }

    #[test]
    fn empty_or_failed_transcription_pastes_nothing() {
        for ev in [
            Event::Transcribed(Ok(String::new())),
            Event::Transcribed(Err("e".into())),
        ] {
            let (st, fx) = step(State::Transcribing, ev);
            assert_eq!(st, State::Idle);
            assert!(fx.iter().all(|e| !matches!(e, Effect::Paste(_))));
        }
    }

    #[test]
    fn max_duration_stops_either_kind_of_recording() {
        let start = t0();
        let s = Settings::default();
        for mode in [Mode::Toggle, Mode::PushToTalk] {
            let (st, fx) = transition(
                &rec(start, mode),
                Event::MaxDurationReached(start + s.max_record),
                &s,
            );
            assert_eq!(st, State::Transcribing);
            assert!(fx.contains(&Effect::StopAndTranscribe));
        }
    }

    #[test]
    fn start_failure_returns_to_idle() {
        let (st, _) = step(rec(t0(), Mode::Toggle), Event::StartFailed("no mic".into()));
        assert_eq!(st, State::Idle);
    }

    #[test]
    fn reloading_model_blocks_hotkey_until_ready() {
        let (st, _) = step(State::Idle, Event::ModelReloading);
        assert_eq!(st, State::Loading);
        let (st, fx) = step(st, Event::HotkeyPressed(Mode::Toggle, t0()));
        assert_eq!(st, State::Loading);
        assert!(!fx.contains(&Effect::StartRecording));
        assert_eq!(step(st, Event::ModelReady).0, State::Idle);
    }

    #[test]
    fn failed_model_can_be_reloaded() {
        let (st, _) = step(State::Error("x".into()), Event::ModelReloading);
        assert_eq!(st, State::Loading);
    }

    #[test]
    fn reloading_does_not_interrupt_recording() {
        let st = rec(t0(), Mode::Toggle);
        assert_eq!(step(st.clone(), Event::ModelReloading).0, st);
    }

    #[test]
    fn enter_mode_drops_the_trailing_space() {
        let mut cfg = Config::default();
        assert!(postprocess_options(&cfg).trailing_space);
        cfg.output.press_enter = true;
        assert!(!postprocess_options(&cfg).trailing_space);
        assert!(deliver_options(&cfg).press_enter);
    }

    #[test]
    fn output_options_follow_config() {
        let mut cfg = Config::default();
        cfg.output.lowercase_first = true;
        cfg.output.strip_final_period = true;
        let o = postprocess_options(&cfg);
        assert!(o.lowercase_first && o.strip_final_period);
    }

    #[test]
    fn settings_follow_config() {
        let mut cfg = Config::default();
        cfg.audio.min_record_ms = 500;
        cfg.audio.input_device = "fifine".into();
        let s = Settings::from_config(&cfg, None);
        assert_eq!(s.min_record, Duration::from_millis(500));
        assert_eq!(s.input_device, "fifine");
        assert_eq!(
            Settings::from_config(&cfg, Some("realtek")).input_device,
            "realtek"
        );
    }
}
