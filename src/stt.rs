//! Speech recognition on top of whisper-rs. The rest of the app never touches
//! whisper types directly.

use std::path::Path;

use crate::audio::{trim_silence, Captured, WHISPER_RATE};
use crate::wav;
use anyhow::{anyhow, Context, Result};
use crossbeam_channel::Sender;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

#[derive(Debug, Clone)]
pub struct SttConfig {
    pub model_path: std::path::PathBuf,
    pub use_gpu: bool,
    /// "auto", "ru" or "en".
    pub language: String,
    pub beam_size: i32,
    pub initial_prompt: String,
    /// Debug only (`--dump-audio`): save each recording here and print stats.
    pub dump_dir: Option<std::path::PathBuf>,
}

pub const DEFAULT_PROMPT: &str =
    "Привет. Это обычный текст с правильной пунктуацией, запятыми и точками. Hello, this is English text.";

impl SttConfig {
    pub fn with_model(model_path: &Path) -> Self {
        Self {
            model_path: model_path.to_path_buf(),
            use_gpu: true,
            language: "auto".into(),
            beam_size: 5,
            initial_prompt: DEFAULT_PROMPT.into(),
            dump_dir: None,
        }
    }
}

impl SttConfig {
    pub fn from_config(cfg: &crate::config::Config, models_dir: &Path) -> Self {
        Self {
            model_path: models_dir.join(&cfg.stt.model),
            use_gpu: cfg.stt.use_gpu,
            language: cfg.stt.language.clone(),
            beam_size: cfg.stt.beam_size,
            initial_prompt: cfg.stt.initial_prompt.clone(),
            dump_dir: None,
        }
    }
}

pub struct SttEngine {
    // The state borrows nothing from the context (whisper-rs keeps an Arc inside),
    // but the context must stay alive for as long as the state is used.
    _ctx: WhisperContext,
    state: WhisperState,
    cfg: SttConfig,
}

impl SttEngine {
    pub fn load(cfg: &SttConfig) -> Result<Self> {
        if !cfg.model_path.is_file() {
            anyhow::bail!(
                "model file not found: {} (put it in the models folder or set `model` in config.toml)",
                cfg.model_path.display()
            );
        }
        let path = cfg
            .model_path
            .to_str()
            .ok_or_else(|| anyhow!("model path is not valid UTF-8"))?;
        let mut params = WhisperContextParameters::default();
        params.use_gpu(cfg.use_gpu);
        let ctx = WhisperContext::new_with_params(path, params)
            .map_err(|e| anyhow!("cannot load model {path}: {e}"))?;
        let state = ctx.create_state().context("cannot create whisper state")?;
        Ok(Self {
            _ctx: ctx,
            state,
            cfg: cfg.clone(),
        })
    }

    /// Applies language/beam/prompt changes without reloading the model.
    pub fn update_params(&mut self, cfg: &SttConfig) {
        self.cfg = cfg.clone();
    }

    /// `pcm` must be 16 kHz mono f32.
    pub fn transcribe(&mut self, pcm: &[f32]) -> Result<String> {
        let mut params = FullParams::new(SamplingStrategy::BeamSearch {
            beam_size: self.cfg.beam_size.max(1),
            patience: -1.0,
        });
        match self.cfg.language.as_str() {
            // Do not call set_detect_language(true): whisper.cpp then only detects
            // the language and returns without transcribing.
            "auto" => params.set_language(None),
            lang => params.set_language(Some(lang)),
        }
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().min(8))
            .unwrap_or(4);
        params.set_n_threads(threads as i32);
        params.set_translate(false);
        params.set_no_context(true);
        params.set_suppress_blank(true);
        params.set_temperature(0.0);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        if !self.cfg.initial_prompt.is_empty() {
            params.set_initial_prompt(&self.cfg.initial_prompt);
        }

        self.state
            .full(params, pcm)
            .map_err(|e| anyhow!("whisper inference failed: {e}"))?;

        let text = self
            .state
            .as_iter()
            .map(|seg| seg.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(text.trim().to_string())
    }
}

/// Messages from the STT worker thread to the app.
#[derive(Debug)]
pub enum WorkerMsg {
    Ready,
    /// The model is being (re)loaded after a settings change.
    Reloading,
    LoadFailed(String),
    /// Raw recognized text (not yet post-processed) or an error.
    Text(Result<String, String>),
}

pub enum Job {
    Transcribe(Captured),
    /// New settings; the model is reloaded only if the file or GPU flag changed.
    Reconfigure(SttConfig),
}

fn load_and_warm(cfg: &SttConfig) -> Result<SttEngine> {
    let mut engine = SttEngine::load(cfg)?;
    // Warm-up initialises the GPU kernels so the first real dictation is not slow. On the
    // CPU there is nothing to warm up, and a pass over silence would only cost time.
    if cfg!(feature = "cuda") && cfg.use_gpu {
        if let Err(e) = engine.transcribe(&vec![0.0f32; WHISPER_RATE as usize]) {
            tracing::warn!("warm-up failed: {e:#}");
        }
    }
    Ok(engine)
}

/// Starts the STT worker. The model is loaded once and kept in memory; each
/// `Job::Transcribe` produces one `WorkerMsg::Text`. The thread survives a failed
/// model load so that a fixed config can load it later.
pub fn spawn_worker<F>(cfg: SttConfig, emit: F) -> Result<Sender<Job>>
where
    F: Fn(WorkerMsg) + Send + 'static,
{
    let (tx, rx) = crossbeam_channel::unbounded::<Job>();
    std::thread::Builder::new()
        .name("stt".into())
        .spawn(move || {
            let mut cfg = cfg;
            let load = |cfg: &SttConfig| match load_and_warm(cfg) {
                Ok(e) => {
                    emit(WorkerMsg::Ready);
                    Some(e)
                }
                Err(e) => {
                    emit(WorkerMsg::LoadFailed(format!("{e:#}")));
                    None
                }
            };
            let mut engine = load(&cfg);

            for job in rx {
                match job {
                    Job::Transcribe(captured) => {
                        let result = match engine.as_mut() {
                            None => Err("speech model is not loaded".to_string()),
                            Some(e) => {
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    process(e, captured, cfg.dump_dir.as_deref())
                                }))
                                .unwrap_or_else(|_| Err("recognition thread panicked".to_string()))
                            }
                        };
                        emit(WorkerMsg::Text(result));
                    }
                    Job::Reconfigure(new) => {
                        let reload = engine.is_none()
                            || new.model_path != cfg.model_path
                            || new.use_gpu != cfg.use_gpu;
                        cfg = new;
                        if reload {
                            emit(WorkerMsg::Reloading);
                            // Free the old model (VRAM) before loading the next one.
                            drop(engine.take());
                            engine = load(&cfg);
                        } else if let Some(e) = engine.as_mut() {
                            e.update_params(&cfg);
                        }
                    }
                }
            }
        })
        .context("cannot spawn stt thread")?;
    Ok(tx)
}

fn process(
    engine: &mut SttEngine,
    captured: Captured,
    dump_dir: Option<&Path>,
) -> Result<String, String> {
    let (raw_len, rate) = (captured.mono.len(), captured.rate);
    if let Some(dir) = dump_dir {
        let _ = std::fs::create_dir_all(dir);
        if let Err(e) = wav::write_mono_f32(&dir.join("last_raw.wav"), &captured.mono, rate) {
            tracing::warn!("{e:#}");
        }
    }
    let pcm = captured.into_16k().map_err(|e| format!("{e:#}"))?;
    let speech = trim_silence(&pcm, 300);
    if let Some(dir) = dump_dir {
        let _ = wav::write_mono_f32(&dir.join("last.wav"), &pcm, WHISPER_RATE);
        let peak = pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let rms = (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len().max(1) as f32).sqrt();
        tracing::info!(
            "[dump] device {rate} Hz, {raw_len} samples -> 16k: {} samples ({:.2} s), peak {peak:.3}, rms {rms:.4}, after trim {:.2} s",
            pcm.len(),
            pcm.len() as f32 / WHISPER_RATE as f32,
            speech.len() as f32 / WHISPER_RATE as f32,
        );
    }
    // Too little speech: skip Whisper entirely, it hallucinates on silence.
    if speech.len() < WHISPER_RATE as usize * 3 / 10 {
        return Ok(String::new());
    }
    let raw = engine.transcribe(speech).map_err(|e| format!("{e:#}"))?;
    if dump_dir.is_some() {
        tracing::info!("[dump] raw whisper text: {raw:?}");
    }
    Ok(raw)
}
