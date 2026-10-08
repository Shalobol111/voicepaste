//! Settings window (`voicepaste.exe --settings`).
//!
//! A separate process, so the tray app keeps its own event loop (AGENTS.md 3.3).
//! It only reads and writes `config.toml`; the running app notices the change
//! through its file watcher and applies it. Dark retro look: thin outlines, green
//! captions sitting on the frame lines, a log panel at the bottom.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Margin, Pos2, Rect, RichText, Sense, Shape,
    Stroke, StrokeKind, Vec2,
};
use global_hotkey::GlobalHotKeyManager;
use windows::core::w;

use crate::audio::{self, LevelMeter};
use crate::config::{self, Config, PasteMethod};
use crate::model_download::{self, Download};
use crate::sound::{self, Sound};
use crate::{hotkey, platform};

pub const TITLE: &str = "VoicePaste — настройки";

const BG: Color32 = Color32::from_rgb(29, 29, 29);
const BG_DEEP: Color32 = Color32::from_rgb(22, 22, 22);
const BG_BAR: Color32 = Color32::from_rgb(38, 38, 38);
const BORDER: Color32 = Color32::from_rgb(207, 207, 207);
const BORDER_DIM: Color32 = Color32::from_rgb(110, 110, 110);
const TEXT: Color32 = Color32::from_rgb(225, 225, 225);
const TEXT_DIM: Color32 = Color32::from_rgb(140, 140, 140);
const GREEN: Color32 = Color32::from_rgb(127, 179, 86);
const GREEN_DARK: Color32 = Color32::from_rgb(42, 61, 28);
const RED: Color32 = Color32::from_rgb(230, 90, 90);
const AMBER: Color32 = Color32::from_rgb(232, 176, 60);
const BLUE: Color32 = Color32::from_rgb(58, 110, 165);

const WINDOW_SIZE: [f32; 2] = [560.0, 820.0];

pub fn run(config_path: PathBuf, models_dir: PathBuf, logs_dir: PathBuf) -> Result<()> {
    if !platform::acquire_single_instance(w!("Local\\VoicePaste.Settings")) {
        platform::focus_window_by_title(TITLE);
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size(WINDOW_SIZE)
            .with_resizable(false)
            .with_decorations(false)
            .with_maximize_button(false)
            .with_icon(window_icon()),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        TITLE,
        options,
        Box::new(move |cc| {
            apply_style(&cc.egui_ctx);
            Ok(Box::new(SettingsApp::new(
                config_path,
                models_dir,
                logs_dir,
            )))
        }),
    )
    .map_err(|e| anyhow!("settings window failed: {e}"))
}

/// The app icon (64x64 RGBA, generated into assets/icons/app-64.rgba) for the taskbar.
fn window_icon() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!("../assets/icons/app-64.rgba").to_vec(),
        width: 64,
        height: 64,
    }
}

fn apply_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = BG;
    v.extreme_bg_color = BG_DEEP;
    v.faint_bg_color = BG_BAR;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = Color32::from_rgb(60, 90, 40);
    v.selection.stroke = Stroke::new(1.0, GREEN);
    v.window_corner_radius = CornerRadius::ZERO;
    v.menu_corner_radius = CornerRadius::ZERO;

    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_DIM);
    w.noninteractive.corner_radius = CornerRadius::ZERO;
    w.inactive.bg_fill = BG_BAR;
    w.inactive.weak_bg_fill = BG_BAR;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER_DIM);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = CornerRadius::ZERO;
    w.hovered.bg_fill = BG_BAR;
    w.hovered.weak_bg_fill = BG_BAR;
    w.hovered.bg_stroke = Stroke::new(1.0, GREEN);
    w.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    w.hovered.corner_radius = CornerRadius::ZERO;
    w.active.bg_fill = GREEN_DARK;
    w.active.weak_bg_fill = GREEN_DARK;
    w.active.bg_stroke = Stroke::new(1.0, GREEN);
    w.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    w.active.corner_radius = CornerRadius::ZERO;
    w.open.bg_fill = BG_BAR;
    w.open.weak_bg_fill = BG_BAR;
    w.open.bg_stroke = Stroke::new(1.0, GREEN);
    w.open.corner_radius = CornerRadius::ZERO;
    // By default a hovered widget grows by 1 px, which made neighbouring buttons jump.
    for state in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        state.expansion = 0.0;
    }
    ctx.set_visuals(v);

    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(10.0, 8.0);
        s.spacing.button_padding = Vec2::new(14.0, 6.0);
        s.spacing.interact_size.y = 26.0;
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    Toggle,
    PushToTalk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogLevel {
    Info,
    Warn,
    Error,
}

struct SettingsApp {
    path: PathBuf,
    models_dir: PathBuf,
    logs_dir: PathBuf,
    /// What is on disk now.
    saved: Config,
    /// What the controls edit.
    cfg: Config,
    load_error: Option<String>,
    devices: Vec<String>,
    models: Vec<String>,
    capture: Option<Capture>,
    capture_msg: Option<String>,
    meter: Option<LevelMeter>,
    meter_error: Option<String>,
    level_shown: f32,
    checker: Option<GlobalHotKeyManager>,
    conflicts: HashMap<String, Option<String>>,
    /// Result of the last save: (message, success).
    status: Option<(String, bool)>,
    log_lines: Vec<(String, LogLevel)>,
    log_checked: Option<Instant>,
    /// The model download in progress (one at a time).
    download: Option<Download>,
    /// Result of the last download: (message, success).
    download_msg: Option<(String, bool)>,
}

/// A click in the model list, applied after the list is drawn.
enum ModelAction {
    Download(&'static model_download::KnownModel),
    Select(&'static str),
    Cancel,
}

impl SettingsApp {
    fn new(path: PathBuf, models_dir: PathBuf, logs_dir: PathBuf) -> Self {
        let (cfg, load_error) = config::load(&path);
        Self {
            devices: audio::list_input_devices().unwrap_or_default(),
            models: list_models(&models_dir),
            saved: cfg.clone(),
            cfg,
            load_error,
            path,
            models_dir,
            logs_dir,
            capture: None,
            capture_msg: None,
            meter: None,
            meter_error: None,
            level_shown: 0.0,
            checker: None,
            conflicts: HashMap::new(),
            status: None,
            log_lines: Vec::new(),
            log_checked: None,
            download: None,
            download_msg: None,
        }
    }

    /// Picks up the result of a finished download.
    fn poll_download(&mut self) {
        let Some(d) = &self.download else { return };
        let state = d.snapshot();
        if state.finished {
            let file = d.file;
            self.models = list_models(&self.models_dir);
            self.cfg.stt.model = file.to_string();
            self.download_msg = Some((
                format!("Модель {file} скачана и выбрана. Нажмите «Сохранить»."),
                true,
            ));
            self.download = None;
        } else if let Some(e) = state.error {
            self.download_msg = Some((e, false));
            self.download = None;
        }
    }

    fn apply_model_action(&mut self, action: ModelAction) {
        match action {
            ModelAction::Download(m) => {
                self.download_msg = None;
                self.download = Some(Download::start(m, self.models_dir.clone()));
            }
            ModelAction::Select(file) => self.cfg.stt.model = file.to_string(),
            ModelAction::Cancel => {
                if let Some(d) = &self.download {
                    d.cancel();
                }
            }
        }
    }

    fn models_table(&mut self, ui: &mut egui::Ui) {
        let mut action: Option<ModelAction> = None;
        let busy = self.download.is_some();
        let progress = self
            .download
            .as_ref()
            .map(|d| (d.file, d.snapshot().fraction()));
        egui::Grid::new("models_grid")
            .num_columns(3)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                for m in model_download::KNOWN_MODELS {
                    ui.vertical(|ui| {
                        ui.set_min_width(250.0);
                        ui.label(
                            RichText::new(format!(
                                "{} · {} МБ",
                                m.title,
                                model_download::size_mb(m.size)
                            ))
                            .color(TEXT)
                            .size(13.0),
                        );
                        ui.label(
                            RichText::new(model_download::note(m, cfg!(feature = "cuda")))
                                .color(TEXT_DIM)
                                .size(12.0),
                        );
                    });
                    let installed = model_download::is_installed(&self.models_dir, m);
                    let selected = self.cfg.stt.model == m.file;
                    let downloading = progress.filter(|(f, _)| *f == m.file);
                    // Status column (fixed width, so rows never shift).
                    ui.allocate_ui(Vec2::new(130.0, 24.0), |ui| {
                        if let Some((_, frac)) = downloading {
                            ui.add(
                                egui::ProgressBar::new(frac)
                                    .desired_width(124.0)
                                    .fill(GREEN)
                                    .text(format!("{} %", (frac * 100.0) as u32)),
                            );
                        } else if installed && selected {
                            ui.label(RichText::new("Выбрана").color(GREEN));
                        } else if installed {
                            ui.label(RichText::new("Установлена").color(TEXT_DIM));
                        } else {
                            ui.label(RichText::new("Не скачана").color(TEXT_DIM));
                        }
                    });
                    // Action column.
                    if downloading.is_some() {
                        if ui
                            .add_sized([100.0, 26.0], egui::Button::new("Отмена"))
                            .clicked()
                        {
                            action = Some(ModelAction::Cancel);
                        }
                    } else if installed {
                        let select = egui::Button::new("Выбрать");
                        if ui
                            .add_enabled_ui(!selected, |ui| ui.add_sized([100.0, 26.0], select))
                            .inner
                            .clicked()
                        {
                            action = Some(ModelAction::Select(m.file));
                        }
                    } else {
                        let get = egui::Button::new("Скачать");
                        if ui
                            .add_enabled_ui(!busy, |ui| ui.add_sized([100.0, 26.0], get))
                            .inner
                            .clicked()
                        {
                            action = Some(ModelAction::Download(m));
                        }
                    }
                    ui.end_row();
                }
            });
        if let Some((msg, ok)) = &self.download_msg {
            ui.label(
                RichText::new(msg)
                    .color(if *ok { GREEN } else { RED })
                    .size(12.0),
            );
        }
        if let Some(a) = action {
            self.apply_model_action(a);
        }
    }

    fn dirty(&self) -> bool {
        self.cfg != self.saved
    }

    // ---- hotkey validation ----

    /// "Taken by another app" check for a hotkey the user has just changed. Hotkeys
    /// that are unchanged are held by the running app itself, so they are skipped.
    fn conflict(&mut self, text: &str) -> Option<String> {
        if let Some(cached) = self.conflicts.get(text) {
            return cached.clone();
        }
        let result = (|| {
            let hk = hotkey::parse(text).ok()?;
            let held_by_app = [&self.saved.hotkey.toggle, &self.saved.hotkey.push_to_talk]
                .iter()
                .filter_map(|t| hotkey::parse(t).ok())
                .any(|h| h.id() == hk.id());
            if held_by_app {
                return None;
            }
            if self.checker.is_none() {
                self.checker = GlobalHotKeyManager::new().ok();
            }
            let manager = self.checker.as_ref()?;
            match manager.register(hk) {
                Ok(()) => {
                    let _ = manager.unregister(hk);
                    None
                }
                Err(_) => Some("Сочетание занято другим приложением".to_string()),
            }
        })();
        self.conflicts.insert(text.to_string(), result.clone());
        result
    }

    /// Problems of one hotkey field (empty = fine).
    fn hotkey_problems(&mut self, which: Capture) -> Vec<String> {
        let (mode, text) = match which {
            Capture::Toggle => (hotkey::Mode::Toggle, self.cfg.hotkey.toggle.clone()),
            Capture::PushToTalk => (
                hotkey::Mode::PushToTalk,
                self.cfg.hotkey.push_to_talk.clone(),
            ),
        };
        let resolved = hotkey::resolve(&self.cfg.hotkey.toggle, &self.cfg.hotkey.push_to_talk);
        let mut out: Vec<String> = resolved
            .errors
            .into_iter()
            .filter(|e| e.starts_with(mode.label()))
            .collect();
        let both_off = self.cfg.hotkey.toggle.trim().is_empty()
            && self.cfg.hotkey.push_to_talk.trim().is_empty();
        if both_off && which == Capture::Toggle {
            out.push("Отключены обе клавиши — запись нечем запускать".into());
        }
        if out.is_empty() && !text.trim().is_empty() {
            if let Some(c) = self.conflict(text.trim()) {
                out.push(c);
            }
        }
        out
    }

    fn has_blocking_problems(&mut self) -> bool {
        !self.hotkey_problems(Capture::Toggle).is_empty()
            || !self.hotkey_problems(Capture::PushToTalk).is_empty()
    }

    // ---- actions ----

    fn save(&mut self) {
        if self.has_blocking_problems() {
            self.status = Some(("Исправьте горячие клавиши".into(), false));
            return;
        }
        let cfg = self.cfg.clone().sanitized();
        match config::save_atomic(&self.path, &cfg) {
            Ok(()) => {
                self.saved = cfg.clone();
                self.cfg = cfg;
                self.status = Some((
                    "Сохранено — запущенная программа применит сама".into(),
                    true,
                ));
            }
            Err(e) => self.status = Some((format!("Не удалось сохранить: {e:#}"), false)),
        }
    }

    fn toggle_meter(&mut self) {
        if self.meter.is_some() {
            self.meter = None;
            return;
        }
        match LevelMeter::start(&self.cfg.audio.input_device) {
            Ok(m) => {
                self.meter = Some(m);
                self.meter_error = None;
            }
            Err(e) => self.meter_error = Some(format!("{e:#}")),
        }
    }

    /// Restarts a running level meter on the newly chosen device.
    fn restart_meter(&mut self) {
        if self.meter.is_some() {
            self.meter = None;
            self.toggle_meter();
        }
    }

    fn refresh_log(&mut self) {
        let due = self
            .log_checked
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
        if due {
            self.log_checked = Some(Instant::now());
            self.log_lines = read_log_tail(&self.logs_dir, 7);
        }
    }

    // ---- drawing ----

    fn title_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let (rect, drag) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), 66.0),
            Sense::click_and_drag(),
        );
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        let p = ui.painter();
        p.rect_filled(rect, 0.0, BG_BAR);
        p.line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            Stroke::new(1.0, Color32::from_rgb(52, 52, 52)),
        );

        // Logo: a ring with a microphone inside.
        let c = Pos2::new(rect.left() + 38.0, rect.center().y);
        p.circle_stroke(c, 19.0, Stroke::new(2.0, GREEN));
        p.rect_filled(
            Rect::from_center_size(c + Vec2::new(0.0, -4.0), Vec2::new(8.0, 15.0)),
            CornerRadius::same(4),
            GREEN,
        );
        let arc: Vec<Pos2> = (0..=12)
            .map(|i| {
                let a = std::f32::consts::PI * i as f32 / 12.0;
                c + Vec2::new(-9.0 * a.cos(), 2.0 + 9.0 * a.sin())
            })
            .collect();
        p.add(Shape::line(arc, Stroke::new(2.0, GREEN)));
        p.line_segment(
            [c + Vec2::new(0.0, 11.0), c + Vec2::new(0.0, 14.0)],
            Stroke::new(2.0, GREEN),
        );

        p.text(
            Pos2::new(rect.left() + 70.0, rect.center().y - 9.0),
            Align2::LEFT_CENTER,
            "VOICEPASTE",
            FontId::proportional(25.0),
            Color32::from_rgb(242, 242, 242),
        );
        p.text(
            Pos2::new(rect.left() + 72.0, rect.center().y + 15.0),
            Align2::LEFT_CENTER,
            "голос в текст, локально",
            FontId::proportional(13.0),
            TEXT_DIM,
        );

        // Window buttons.
        let button = |ui: &mut egui::Ui, x_from_right: f32, id: &str, glyph: &str| {
            let r = Rect::from_min_size(
                Pos2::new(rect.right() - x_from_right, rect.top() + 10.0),
                Vec2::new(30.0, 22.0),
            );
            let resp = ui.interact(r, ui.id().with(id), Sense::click());
            let (fill, stroke) = if resp.hovered() {
                (GREEN_DARK, GREEN)
            } else {
                (Color32::from_rgb(44, 44, 44), Color32::from_rgb(70, 70, 70))
            };
            let p = ui.painter();
            p.rect_filled(r, 0.0, fill);
            p.rect_stroke(r, 0.0, Stroke::new(1.0, stroke), StrokeKind::Inside);
            p.text(
                r.center(),
                Align2::CENTER_CENTER,
                glyph,
                FontId::proportional(14.0),
                TEXT,
            );
            resp.clicked()
        };
        if button(ui, 42.0, "close", "X") {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if button(ui, 78.0, "min", "_") {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }

        // State chip.
        let (chip, color) = if self.dirty() {
            ("! ЕСТЬ ИЗМЕНЕНИЯ", AMBER)
        } else {
            ("НАСТРОЙКИ ПРИМЕНЕНЫ", GREEN)
        };
        let galley =
            ui.painter()
                .layout_no_wrap(chip.to_owned(), FontId::proportional(12.0), color);
        let chip_rect = Rect::from_min_size(
            Pos2::new(
                rect.right() - 94.0 - galley.size().x - 20.0,
                rect.top() + 40.0,
            ),
            galley.size() + Vec2::new(16.0, 8.0),
        );
        ui.painter()
            .rect_stroke(chip_rect, 0.0, Stroke::new(1.0, color), StrokeKind::Inside);
        ui.painter()
            .galley(chip_rect.min + Vec2::new(8.0, 4.0), galley, color);
    }

    fn hotkey_row(&mut self, ui: &mut egui::Ui, title: &str, hint: &str, which: Capture) {
        ui.label(RichText::new(title).color(TEXT));
        ui.label(RichText::new(hint).color(TEXT_DIM).size(12.0));
        ui.horizontal(|ui| {
            let capturing = self.capture == Some(which);
            let field = match which {
                Capture::Toggle => &mut self.cfg.hotkey.toggle,
                Capture::PushToTalk => &mut self.cfg.hotkey.push_to_talk,
            };
            ui.add_enabled(
                !capturing,
                egui::TextEdit::singleline(field).desired_width(230.0),
            );
            let label = if capturing {
                "Нажмите сочетание…"
            } else {
                "Задать"
            };
            if ui.button(label).clicked() {
                self.capture = if capturing { None } else { Some(which) };
                self.capture_msg = None;
            }
            if ui.button("Очистить").clicked() {
                match which {
                    Capture::Toggle => self.cfg.hotkey.toggle.clear(),
                    Capture::PushToTalk => self.cfg.hotkey.push_to_talk.clear(),
                }
            }
        });
        if self.capture == Some(which) {
            if let Some(msg) = &self.capture_msg {
                ui.label(RichText::new(msg).color(AMBER).size(12.0));
            } else {
                ui.label(
                    RichText::new("Esc — отмена. Win-клавишу впишите вручную (Super+…)")
                        .color(TEXT_DIM)
                        .size(12.0),
                );
            }
        }
        for problem in self.hotkey_problems(which) {
            ui.label(RichText::new(problem).color(RED).size(12.0));
        }
    }

    /// Reads a key combination while capture is active.
    fn handle_capture(&mut self, ctx: &egui::Context) {
        let Some(which) = self.capture else { return };
        let events = ctx.input(|i| i.events.clone());
        for ev in events {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = ev
            else {
                continue;
            };
            if key == egui::Key::Escape {
                self.capture = None;
                self.capture_msg = None;
                return;
            }
            let mut parts: Vec<&str> = Vec::new();
            if modifiers.ctrl {
                parts.push("Ctrl");
            }
            if modifiers.alt {
                parts.push("Alt");
            }
            if modifiers.shift {
                parts.push("Shift");
            }
            parts.push(key.name());
            let text = parts.join("+");
            match hotkey::parse(&text) {
                Ok(_) => {
                    match which {
                        Capture::Toggle => self.cfg.hotkey.toggle = text,
                        Capture::PushToTalk => self.cfg.hotkey.push_to_talk = text,
                    }
                    self.capture = None;
                    self.capture_msg = None;
                }
                Err(_) => {
                    self.capture_msg = Some(format!(
                        "«{text}» не подходит: нужен Ctrl/Alt/Shift или клавиша F1–F24"
                    ));
                }
            }
            return;
        }
    }

    fn hotkeys_group(&mut self, ui: &mut egui::Ui) {
        group(ui, "Горячие клавиши", |ui| {
            self.hotkey_row(
                ui,
                "Переключатель",
                "нажал — запись началась, нажал ещё раз — остановилась",
                Capture::Toggle,
            );
            ui.add_space(6.0);
            self.hotkey_row(
                ui,
                "Удержание",
                "держишь — идёт запись, отпустил — остановилась",
                Capture::PushToTalk,
            );
        });
    }

    fn microphone_group(&mut self, ui: &mut egui::Ui) {
        group(ui, "Микрофон", |ui| {
            egui::Grid::new("mic_grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Устройство").color(TEXT_DIM));
                    let current = self.cfg.audio.input_device.clone();
                    let before = current.clone();
                    let shown = if current.is_empty() {
                        "Системное по умолчанию".to_string()
                    } else if self.devices.contains(&current) {
                        current.clone()
                    } else {
                        format!("{current} (не найдено)")
                    };
                    egui::ComboBox::from_id_salt("device")
                        .width(330.0)
                        .selected_text(shown)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.cfg.audio.input_device,
                                String::new(),
                                "Системное по умолчанию",
                            );
                            for d in &self.devices {
                                ui.selectable_value(
                                    &mut self.cfg.audio.input_device,
                                    d.clone(),
                                    d.as_str(),
                                );
                            }
                        });
                    if self.cfg.audio.input_device != before {
                        self.restart_meter();
                    }
                    ui.end_row();

                    ui.label(RichText::new("Уровень").color(TEXT_DIM));
                    ui.horizontal(|ui| {
                        level_bar(ui, self.level_shown, self.meter.is_some());
                        let label = if self.meter.is_some() {
                            "Стоп"
                        } else {
                            "Проверить"
                        };
                        if ui.button(label).clicked() {
                            self.toggle_meter();
                        }
                    });
                    ui.end_row();
                });
            if let Some(m) = &self.meter {
                ui.label(
                    RichText::new(format!("Слушаю: {}", m.device_name))
                        .color(TEXT_DIM)
                        .size(12.0),
                );
            }
            if let Some(e) = &self.meter_error {
                ui.label(RichText::new(e).color(RED).size(12.0));
            }
        });
    }

    fn recognition_group(&mut self, ui: &mut egui::Ui) {
        group(ui, "Распознавание", |ui| {
            egui::Grid::new("stt_grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Модель").color(TEXT_DIM));
                    let current = self.cfg.stt.model.clone();
                    let missing = !self.models.contains(&current);
                    let shown = if missing {
                        format!("{current} (нет файла)")
                    } else {
                        current.clone()
                    };
                    egui::ComboBox::from_id_salt("model")
                        .width(330.0)
                        .selected_text(shown)
                        .show_ui(ui, |ui| {
                            if missing {
                                ui.selectable_value(
                                    &mut self.cfg.stt.model,
                                    current.clone(),
                                    format!("{current} (нет файла)"),
                                );
                            }
                            for m in &self.models {
                                ui.selectable_value(&mut self.cfg.stt.model, m.clone(), m.as_str());
                            }
                        });
                    ui.end_row();

                    ui.label(RichText::new("Язык").color(TEXT_DIM));
                    combo(
                        ui,
                        "lang",
                        &mut self.cfg.stt.language,
                        &[
                            ("auto".to_string(), "Авто (русский + английский)"),
                            ("ru".to_string(), "Русский"),
                            ("en".to_string(), "English"),
                        ],
                        330.0,
                    );
                    ui.end_row();

                    ui.label(RichText::new("Точность").color(TEXT_DIM));
                    ui.add(egui::Slider::new(&mut self.cfg.stt.beam_size, 1..=10).text("лучей"));
                    ui.end_row();
                });
            ui.label(
                RichText::new("1 — быстрее · 5 — рекомендуется · 10 — самый точный, но медленнее")
                    .color(TEXT_DIM)
                    .size(12.0),
            );
            ui.checkbox(
                &mut self.cfg.stt.use_gpu,
                "Использовать видеокарту (NVIDIA CUDA)",
            );
            ui.horizontal(|ui| {
                if ui.button("Открыть папку моделей").clicked() {
                    let _ = fs::create_dir_all(&self.models_dir);
                    let _ = std::process::Command::new("explorer")
                        .arg(&self.models_dir)
                        .spawn();
                }
                if ui.button("Обновить список").clicked() {
                    self.models = list_models(&self.models_dir);
                    self.devices = audio::list_input_devices().unwrap_or_default();
                }
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new("Модели (скачиваются с huggingface.co только по кнопке)")
                    .color(TEXT_DIM)
                    .size(12.0),
            );
            self.models_table(ui);
            ui.add_space(4.0);
            ui.label(
                RichText::new("Подсказка для модели (термины, имена, стиль)")
                    .color(TEXT_DIM)
                    .size(12.0),
            );
            ui.add(
                egui::TextEdit::multiline(&mut self.cfg.stt.initial_prompt)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
        });
    }

    fn paste_group(&mut self, ui: &mut egui::Ui) {
        group(ui, "Вставка", |ui| {
            ui.checkbox(
                &mut self.cfg.output.paste,
                "Вставлять текст в активное поле",
            );
            ui.label(
                RichText::new("Если выключено, текст только попадает в буфер обмена.")
                    .color(TEXT_DIM)
                    .size(12.0),
            );
            egui::Grid::new("paste_grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Способ").color(TEXT_DIM));
                    combo(
                        ui,
                        "method",
                        &mut self.cfg.output.paste_method,
                        &[
                            (PasteMethod::CtrlV, "Обычная вставка (Ctrl+V)"),
                            (
                                PasteMethod::ShiftInsert,
                                "Для консолей и терминалов (Shift+Insert)",
                            ),
                            (
                                PasteMethod::Type,
                                "Для полей, где вставка запрещена (по буквам)",
                            ),
                        ],
                        330.0,
                    );
                    ui.end_row();

                    ui.label(RichText::new("Пауза").color(TEXT_DIM));
                    ui.add(
                        egui::Slider::new(&mut self.cfg.output.paste_delay_ms, 0..=500)
                            .suffix(" мс"),
                    );
                    ui.end_row();
                });
            let hint = match self.cfg.output.paste_method {
                PasteMethod::CtrlV => "Подходит почти везде. Остальные способы — запасные.",
                PasteMethod::ShiftInsert => {
                    "Для окон, где Ctrl+V не вставляет (старые консоли, некоторые терминалы)."
                }
                PasteMethod::Type => {
                    "Текст набирается как с клавиатуры, медленнее. Для полей, где вставка заблокирована."
                }
            };
            ui.label(RichText::new(hint).color(TEXT_DIM).size(12.0));
            ui.add_enabled(
                !self.cfg.output.press_enter,
                egui::Checkbox::new(&mut self.cfg.output.trailing_space, "Пробел после текста"),
            );
            ui.checkbox(
                &mut self.cfg.output.strip_final_period,
                "Убирать точку в конце (удобно для чатов)",
            );
            ui.checkbox(
                &mut self.cfg.output.lowercase_first,
                "Начинать с маленькой буквы",
            );
            ui.label(
                RichText::new("Аббревиатуры (GPU, VS Code) и английское I остаются как есть.")
                    .color(TEXT_DIM)
                    .size(12.0),
            );
            ui.add_enabled(
                self.cfg.output.paste,
                egui::Checkbox::new(
                    &mut self.cfg.output.press_enter,
                    "Нажимать Enter после вставки (отправить сообщение)",
                ),
            );
            if self.cfg.output.press_enter {
                ui.label(
                    RichText::new("Пробел после текста при этом не добавляется.")
                        .color(TEXT_DIM)
                        .size(12.0),
                );
            }
        });
    }

    fn interface_group(&mut self, ui: &mut egui::Ui) {
        group(ui, "Интерфейс", |ui| {
            ui.checkbox(&mut self.cfg.ui.show_overlay, "Показывать индикатор записи");
            ui.checkbox(&mut self.cfg.ui.sounds, "Звуки начала и конца записи");
            ui.add_enabled_ui(self.cfg.ui.sounds, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Громкость").color(TEXT_DIM));
                    let resp = ui.add(
                        egui::Slider::new(&mut self.cfg.ui.sound_volume, 0..=100).suffix(" %"),
                    );
                    // Play a sample when the slider is let go, so the level can be judged.
                    if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                        sound::play(Sound::Start, self.cfg.ui.sound_volume);
                    }
                });
            });
            ui.checkbox(&mut self.cfg.ui.autostart, "Запускать вместе с Windows");
        });
    }

    fn log_group(&mut self, ui: &mut egui::Ui) {
        group(ui, "Журнал", |ui| {
            egui::Frame::NONE
                .fill(BG_DEEP)
                .stroke(Stroke::new(1.0, BLUE))
                .inner_margin(Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(100.0);
                    if let Some(e) = &self.load_error {
                        ui.label(
                            RichText::new(format!("config.toml не прочитан: {e}"))
                                .monospace()
                                .size(12.0)
                                .color(RED),
                        );
                    }
                    if self.log_lines.is_empty() {
                        ui.label(
                            RichText::new("Журнал пуст. Запустите VoicePaste.")
                                .monospace()
                                .size(12.0)
                                .color(TEXT_DIM),
                        );
                    }
                    for (line, level) in &self.log_lines {
                        let color = match level {
                            LogLevel::Info => TEXT_DIM,
                            LogLevel::Warn => AMBER,
                            LogLevel::Error => RED,
                        };
                        ui.label(RichText::new(line).monospace().size(12.0).color(color));
                    }
                });
        });
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        ui.add_space(14.0);
        // The status has its own line with a fixed height, so a long message can never
        // push the buttons around. The buttons have fixed sizes and are always present
        // ("Отменить" is greyed out instead of appearing), so nothing shifts on hover.
        ui.allocate_ui(Vec2::new(ui.available_width(), 18.0), |ui| {
            if let Some((msg, ok)) = &self.status {
                ui.label(
                    RichText::new(msg)
                        .color(if *ok { GREEN } else { RED })
                        .size(13.0),
                );
            }
        });
        ui.add_space(6.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let save = egui::Button::new(RichText::new("Сохранить").strong().color(Color32::WHITE))
                .fill(GREEN_DARK)
                .stroke(Stroke::new(1.0, GREEN));
            if ui.add_sized([130.0, 30.0], save).clicked() {
                self.save();
            }
            if ui
                .add_sized([100.0, 30.0], egui::Button::new("Закрыть"))
                .clicked()
            {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            let dirty = self.dirty();
            let revert = ui
                .add_enabled_ui(dirty, |ui| {
                    ui.add_sized([170.0, 30.0], egui::Button::new("Отменить изменения"))
                })
                .inner;
            if revert.clicked() {
                self.cfg = self.saved.clone();
                self.status = None;
            }
        });
        ui.add_space(16.0);
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_capture(&ctx);
        self.poll_download();
        self.refresh_log();
        if let Some(m) = &self.meter {
            // Fast attack, slow decay, so short peaks stay visible.
            self.level_shown = m.level().max(self.level_shown * 0.88);
        } else {
            self.level_shown = 0.0;
        }

        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, BG);
        self.title_bar(ui);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::NONE
                    .inner_margin(Margin::symmetric(18, 0))
                    .show(ui, |ui| {
                        self.hotkeys_group(ui);
                        self.microphone_group(ui);
                        self.recognition_group(ui);
                        self.paste_group(ui);
                        self.interface_group(ui);
                        self.log_group(ui);
                        self.footer(ui);
                    });
            });
        ui.painter().rect_stroke(
            full,
            0.0,
            Stroke::new(1.0, Color32::from_rgb(70, 70, 70)),
            StrokeKind::Inside,
        );

        // Keep the log and the level meter alive without user input.
        let next = if self.meter.is_some() {
            Duration::from_millis(33)
        } else if self.download.is_some() {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(500)
        };
        ctx.request_repaint_after(next);
    }
}

/// A frame with the caption sitting on its top line.
fn group(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(16.0);
    let framed = egui::Frame::NONE
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(Margin {
            left: 14,
            right: 14,
            top: 18,
            bottom: 12,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    let rect = framed.response.rect;
    let galley = ui
        .painter()
        .layout_no_wrap(title.to_owned(), FontId::proportional(14.0), GREEN);
    let pos = Pos2::new(rect.left() + 16.0, rect.top() - galley.size().y / 2.0);
    ui.painter().rect_filled(
        Rect::from_min_size(
            pos - Vec2::new(8.0, 0.0),
            galley.size() + Vec2::new(16.0, 0.0),
        ),
        0.0,
        BG,
    );
    ui.painter().galley(pos, galley, GREEN);
}

fn combo<T: Clone + PartialEq>(
    ui: &mut egui::Ui,
    id: &str,
    value: &mut T,
    options: &[(T, &str)],
    width: f32,
) {
    let shown = options
        .iter()
        .find(|(v, _)| v == value)
        .map(|(_, l)| *l)
        .unwrap_or("—");
    egui::ComboBox::from_id_salt(id)
        .width(width)
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for (v, label) in options {
                ui.selectable_value(value, v.clone(), *label);
            }
        });
}

/// Horizontal level meter; green, then amber, then red near clipping.
fn level_bar(ui: &mut egui::Ui, level: f32, active: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(250.0, 18.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 0.0, BG_DEEP);
    if active {
        // sqrt: quiet microphones (peak ~0.05) still move the bar visibly.
        let frac = level.clamp(0.0, 1.0).sqrt();
        let color = if frac > 0.9 {
            RED
        } else if frac > 0.7 {
            AMBER
        } else {
            GREEN
        };
        let filled = Rect::from_min_size(rect.min, Vec2::new(rect.width() * frac, rect.height()));
        p.rect_filled(filled.shrink(2.0), 0.0, color);
    }
    p.rect_stroke(rect, 0.0, Stroke::new(1.0, BORDER_DIM), StrokeKind::Inside);
}

/// `*.bin` files in the models folder, sorted by name.
fn list_models(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("bin"))
        })
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// Turns `2026-10-06T03:25:28.122476Z  INFO voicepaste::app: message` into
/// `[03:25:28] message`. Debug lines and foreign targets are dropped.
fn parse_log_line(line: &str) -> Option<(String, LogLevel)> {
    let (ts, rest) = line.split_once(' ')?;
    let time = ts.get(11..19)?;
    let rest = rest.trim_start();
    let (level, rest) = rest.split_once(' ')?;
    let level = match level {
        "INFO" => LogLevel::Info,
        "WARN" => LogLevel::Warn,
        "ERROR" => LogLevel::Error,
        _ => return None,
    };
    let (target, message) = rest.trim_start().split_once(": ")?;
    if !target.starts_with("voicepaste") {
        return None;
    }
    Some((format!("[{time}] {message}"), level))
}

/// The last `count` readable lines of the newest log file.
fn read_log_tail(dir: &Path, count: usize) -> Vec<(String, LogLevel)> {
    let newest = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("voicepaste.log")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p);
    let Some(path) = newest else {
        return Vec::new();
    };
    let Ok(mut file) = fs::File::open(&path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    const TAIL: u64 = 24 * 1024;
    let mut truncated = false;
    if len > TAIL {
        truncated = file.seek(SeekFrom::Start(len - TAIL)).is_ok();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    if truncated {
        lines.next(); // probably cut in the middle
    }
    let parsed: Vec<_> = lines.filter_map(parse_log_line).collect();
    let skip = parsed.len().saturating_sub(count);
    parsed.into_iter().skip(skip).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_line_is_shortened() {
        let l = "2026-10-06T03:25:28.122476Z  INFO voicepaste::app: recording stopped (1.9 s), transcribing";
        let (text, level) = parse_log_line(l).unwrap();
        assert_eq!(text, "[03:25:28] recording stopped (1.9 s), transcribing");
        assert_eq!(level, LogLevel::Info);
    }

    #[test]
    fn warnings_and_errors_keep_their_level() {
        let w = "2026-10-06T03:17:50.066654Z  WARN voicepaste::output: modifier keys still held";
        assert_eq!(parse_log_line(w).unwrap().1, LogLevel::Warn);
        let e = "2026-10-06T03:11:28.049682Z ERROR voicepaste::app: invalid hotkey";
        assert_eq!(parse_log_line(e).unwrap().1, LogLevel::Error);
    }

    #[test]
    fn debug_and_foreign_lines_are_dropped() {
        assert!(parse_log_line("2026-10-06T03:25:28.1Z DEBUG voicepaste::app: x").is_none());
        assert!(parse_log_line("2026-10-06T03:25:28.1Z  INFO other::crate: x").is_none());
        assert!(parse_log_line("garbage").is_none());
    }

    #[test]
    fn models_are_listed_sorted_and_filtered() {
        let dir = std::env::temp_dir().join("voicepaste_models_list_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for n in ["b.bin", "a.BIN", "notes.txt"] {
            fs::write(dir.join(n), b"x").unwrap();
        }
        assert_eq!(list_models(&dir), vec!["a.BIN", "b.bin"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_log_dir_gives_empty_tail() {
        assert!(read_log_tail(Path::new("C:\\definitely\\not\\here"), 5).is_empty());
    }
}
