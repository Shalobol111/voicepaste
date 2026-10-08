//! System tray icon: state colour, tooltip and the context menu.

use anyhow::{Context, Result};
use tray_icon::menu::{Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Idle,
    Recording,
    Busy,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Settings,
    OpenConfig,
    OpenModels,
    OpenLogs,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    settings: MenuItem,
    open_config: MenuItem,
    open_models: MenuItem,
    open_logs: MenuItem,
    quit: MenuItem,
}

impl Tray {
    pub fn new(tooltip: &str) -> Result<Self> {
        let settings = MenuItem::new("Настройки…", true, None);
        let open_config = MenuItem::new("Открыть config.toml", true, None);
        let open_models = MenuItem::new("Открыть папку моделей", true, None);
        let open_logs = MenuItem::new("Открыть папку логов", true, None);
        let quit = MenuItem::new("Выход", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &settings,
            &PredefinedMenuItem::separator(),
            &open_config,
            &open_models,
            &open_logs,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .context("cannot build tray menu")?;

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(tooltip)
            .with_icon(make_icon(IconKind::Busy)?)
            .build()
            .context("cannot create tray icon")?;

        Ok(Self {
            icon,
            settings,
            open_config,
            open_models,
            open_logs,
            quit,
        })
    }

    pub fn set_status(&self, kind: IconKind, tooltip: &str) {
        match make_icon(kind) {
            Ok(icon) => {
                let _ = self.icon.set_icon(Some(icon));
            }
            Err(e) => tracing::warn!("cannot draw tray icon: {e:#}"),
        }
        let _ = self.icon.set_tooltip(Some(tooltip));
    }

    pub fn action(&self, id: &MenuId) -> Option<MenuAction> {
        if id == self.settings.id() {
            Some(MenuAction::Settings)
        } else if id == self.open_config.id() {
            Some(MenuAction::OpenConfig)
        } else if id == self.open_models.id() {
            Some(MenuAction::OpenModels)
        } else if id == self.open_logs.id() {
            Some(MenuAction::OpenLogs)
        } else if id == self.quit.id() {
            Some(MenuAction::Quit)
        } else {
            None
        }
    }
}

/// Draws a 32x32 anti-aliased disc in the state colour (error gets a "!").
fn make_icon(kind: IconKind) -> Result<Icon> {
    const SIZE: u32 = 32;
    let (r, g, b) = match kind {
        IconKind::Idle => (120, 130, 140),
        IconKind::Recording => (230, 50, 50),
        IconKind::Busy => (240, 180, 40),
        IconKind::Error => (200, 90, 20),
    };
    let center = (SIZE as f32 - 1.0) / 2.0;
    let radius = 14.0f32;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dist = ((x as f32 - center).powi(2) + (y as f32 - center).powi(2)).sqrt();
            let alpha = (radius + 0.5 - dist).clamp(0.0, 1.0);
            let dx = (x as f32 - center).abs();
            let bang = kind == IconKind::Error
                && dx < 2.0
                && ((6..=18).contains(&y) || (22..=25).contains(&y));
            let (pr, pg, pb) = if bang { (255, 255, 255) } else { (r, g, b) };
            rgba.extend_from_slice(&[pr, pg, pb, (alpha * 255.0) as u8]);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).context("bad tray icon data")
}
