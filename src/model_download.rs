//! Downloads Whisper models from Hugging Face. This is the only place the app
//! touches the network, and only after an explicit click in the settings window.
//!
//! The file is streamed into `<name>.part` and renamed once its size matches the
//! known size exactly, so a half-finished download can never be mistaken for a model.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

pub struct KnownModel {
    pub file: &'static str,
    pub title: &'static str,
    pub note: &'static str,
    /// Exact size in bytes (checked after the download).
    pub size: u64,
}

/// Models offered in the settings window. Sizes come from the Hugging Face file listing.
pub const KNOWN_MODELS: &[KnownModel] = &[
    KnownModel {
        file: "ggml-large-v3-turbo-q5_0.bin",
        title: "Large v3 turbo (q5_0)",
        note: "рекомендуется: быстро и точно",
        size: 574_041_195,
    },
    KnownModel {
        file: "ggml-large-v3-turbo.bin",
        title: "Large v3 turbo (полная)",
        note: "чуть точнее, нужно больше памяти",
        size: 1_624_555_275,
    },
    KnownModel {
        file: "ggml-medium-q5_0.bin",
        title: "Medium (q5_0)",
        note: "легче и быстрее",
        size: 539_212_467,
    },
    KnownModel {
        file: "ggml-small-q5_1.bin",
        title: "Small (q5_1)",
        note: "для слабых компьютеров и работы на процессоре",
        size: 190_085_487,
    },
];

/// The model's description for this build. Without a graphics card only the small model
/// is comfortable, so the others carry a warning.
pub fn note(model: &KnownModel, gpu_build: bool) -> String {
    if gpu_build {
        model.note.to_string()
    } else if model.file.contains("small") {
        "рекомендуется для процессора".to_string()
    } else {
        format!("{}; на процессоре медленно", model.note)
    }
}

pub fn url_for(file: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}")
}

pub fn size_mb(bytes: u64) -> u64 {
    bytes.div_ceil(1_000_000)
}

/// True if `dir` already holds the complete model.
pub fn is_installed(dir: &Path, model: &KnownModel) -> bool {
    fs::metadata(dir.join(model.file)).is_ok_and(|m| m.len() == model.size)
}

fn part_path(dir: &Path, file: &str) -> PathBuf {
    dir.join(format!("{file}.part"))
}

#[derive(Debug, Clone, Default)]
pub struct DownloadState {
    pub downloaded: u64,
    pub total: u64,
    pub finished: bool,
    pub error: Option<String>,
}

impl DownloadState {
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.downloaded as f64 / self.total as f64).clamp(0.0, 1.0) as f32
        }
    }
}

/// A running (or finished) download. Dropping it does not stop the thread; use `cancel`.
pub struct Download {
    pub file: &'static str,
    state: Arc<Mutex<DownloadState>>,
    cancel: Arc<AtomicBool>,
}

impl Download {
    pub fn start(model: &'static KnownModel, dir: PathBuf) -> Self {
        let state = Arc::new(Mutex::new(DownloadState {
            total: model.size,
            ..Default::default()
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let (st, ca) = (Arc::clone(&state), Arc::clone(&cancel));
        std::thread::Builder::new()
            .name("model-download".into())
            .spawn(move || {
                let progress = |done: u64| {
                    if let Ok(mut s) = st.lock() {
                        s.downloaded = done;
                    }
                };
                let result = download(model, &dir, &ca, &progress);
                if let Ok(mut s) = st.lock() {
                    match result {
                        Ok(()) => s.finished = true,
                        Err(e) => s.error = Some(format!("{e:#}")),
                    }
                }
            })
            .ok();
        Self {
            file: model.file,
            state,
            cancel,
        }
    }

    pub fn snapshot(&self) -> DownloadState {
        self.state
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| DownloadState {
                error: Some("internal error".into()),
                ..Default::default()
            })
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn download(
    model: &KnownModel,
    dir: &Path,
    cancel: &AtomicBool,
    progress: &dyn Fn(u64),
) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let part = part_path(dir, model.file);
    let target = dir.join(model.file);

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .build()
        .into();
    let response = agent
        .get(url_for(model.file))
        .call()
        .map_err(|e| anyhow!("не удалось подключиться: {e}"))?;
    let reader = response.into_body().into_reader();

    let result = (|| -> Result<()> {
        let mut out =
            fs::File::create(&part).with_context(|| format!("cannot create {}", part.display()))?;
        let written = copy_with_progress(reader, &mut out, cancel, progress)?;
        out.flush().ok();
        out.sync_all().ok();
        drop(out);
        verify_size(written, model.size)?;
        fs::rename(&part, &target)
            .with_context(|| format!("cannot move the model to {}", target.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// Copies `reader` to `writer`, reporting the byte count. Stops with an error when
/// `cancel` is set.
pub fn copy_with_progress(
    mut reader: impl Read,
    writer: &mut impl Write,
    cancel: &AtomicBool,
    progress: &dyn Fn(u64),
) -> Result<u64> {
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("скачивание отменено");
        }
        let n = reader.read(&mut buf).context("ошибка при скачивании")?;
        if n == 0 {
            return Ok(total);
        }
        writer
            .write_all(&buf[..n])
            .context("не удалось записать файл (нет места на диске?)")?;
        total += n as u64;
        progress(total);
    }
}

/// A truncated or oversized file is never accepted as a model.
pub fn verify_size(actual: u64, expected: u64) -> Result<()> {
    if actual != expected {
        bail!("файл скачан не полностью или повреждён ({actual} из {expected} байт)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::Cursor;

    #[test]
    fn copy_reports_progress_and_returns_total() {
        let data = vec![7u8; 600_000];
        let mut out = Vec::new();
        let last = Cell::new(0u64);
        let cancel = AtomicBool::new(false);
        let n =
            copy_with_progress(Cursor::new(&data), &mut out, &cancel, &|d| last.set(d)).unwrap();
        assert_eq!(n, 600_000);
        assert_eq!(last.get(), 600_000);
        assert_eq!(out, data);
    }

    #[test]
    fn copy_stops_when_cancelled() {
        let data = vec![1u8; 1_000_000];
        let mut out = Vec::new();
        let cancel = AtomicBool::new(true);
        let err = copy_with_progress(Cursor::new(&data), &mut out, &cancel, &|_| {}).unwrap_err();
        assert!(err.to_string().contains("отменено"));
        assert!(out.is_empty());
    }

    #[test]
    fn size_check_rejects_partial_and_oversized_files() {
        assert!(verify_size(100, 100).is_ok());
        assert!(verify_size(99, 100).is_err());
        assert!(verify_size(101, 100).is_err());
    }

    #[test]
    fn fraction_is_clamped() {
        let s = DownloadState {
            downloaded: 50,
            total: 200,
            ..Default::default()
        };
        assert!((s.fraction() - 0.25).abs() < 1e-6);
        let s = DownloadState {
            downloaded: 300,
            total: 200,
            ..Default::default()
        };
        assert_eq!(s.fraction(), 1.0);
        assert_eq!(DownloadState::default().fraction(), 0.0);
    }

    #[test]
    fn installed_means_exact_size() {
        let dir = std::env::temp_dir().join("voicepaste_dl_installed_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let model = &KnownModel {
            file: "m.bin",
            title: "t",
            note: "n",
            size: 4,
        };
        assert!(!is_installed(&dir, model));
        fs::write(dir.join("m.bin"), b"123").unwrap();
        assert!(!is_installed(&dir, model));
        fs::write(dir.join("m.bin"), b"1234").unwrap();
        assert!(is_installed(&dir, model));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn notes_warn_about_big_models_in_the_cpu_build() {
        let small = &KNOWN_MODELS[3];
        let turbo = &KNOWN_MODELS[0];
        assert_eq!(note(turbo, true), turbo.note);
        assert!(note(turbo, false).contains("на процессоре медленно"));
        assert!(note(small, false).contains("рекомендуется"));
    }

    #[test]
    fn urls_and_sizes_look_right() {
        assert_eq!(
            url_for("ggml-small-q5_1.bin"),
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin"
        );
        assert_eq!(size_mb(574_041_195), 575);
        assert!(KNOWN_MODELS
            .iter()
            .all(|m| m.file.ends_with(".bin") && m.size > 0));
    }
}
