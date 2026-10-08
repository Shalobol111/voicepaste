// With the default `console` feature the exe is a console app (logs on screen,
// handy while developing). Build with `--no-default-features` for the tray-only exe.
#![cfg_attr(not(feature = "console"), windows_subsystem = "windows")]

mod app;
mod audio;
mod autostart;
mod config;
mod hotkey;
mod model_download;
mod output;
mod overlay;
mod paths;
mod platform;
mod postprocess;
mod settings_ui;
mod sound;
mod stt;
mod tray;
mod wav;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// The CPU build of whisper.cpp is compiled for AVX2 + FMA + F16C. On an older processor it
/// would die with a cryptic "illegal instruction", so say what is wrong instead.
#[cfg(all(target_arch = "x86_64", not(feature = "cuda")))]
fn check_cpu_supported() {
    let ok = std::arch::is_x86_feature_detected!("avx2")
        && std::arch::is_x86_feature_detected!("fma")
        && std::arch::is_x86_feature_detected!("f16c");
    if !ok {
        platform::error_box(
            "Этот компьютер не подходит: версии VoicePaste для процессора нужен процессор \
             с поддержкой AVX2 (выпускаются с 2013–2015 года).",
        );
        std::process::exit(1);
    }
}

#[cfg(not(all(target_arch = "x86_64", not(feature = "cuda"))))]
fn check_cpu_supported() {}

fn main() {
    check_cpu_supported();
    let guard = init_logging();
    // A GUI build has no console, so a panic would otherwise vanish without a trace.
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("panic: {info}");
    }));
    if let Err(e) = run() {
        tracing::error!("fatal: {e:#}");
        eprintln!("error: {e:#}");
        // A GUI build has no console, so tell the user explicitly.
        #[cfg(not(feature = "console"))]
        platform::error_box(&format!("{e:#}"));
        drop(guard);
        std::process::exit(1);
    }
}

/// Logs to `%APPDATA%\VoicePaste\logsoicepaste.log.<date>` (and to the console
/// in the `console` build). The returned guard flushes the file on drop.
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = EnvFilter::try_from_default_env()
        // whisper.cpp is very chatty at info level.
        .unwrap_or_else(|_| EnvFilter::new("info,whisper_rs=warn"));

    let (file_layer, guard) = match paths::logs_dir() {
        Ok(dir) => {
            let appender = tracing_appender::rolling::daily(dir, "voicepaste.log");
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false);
            (Some(layer), Some(guard))
        }
        Err(_) => (None, None),
    };
    let console_layer = cfg!(feature = "console")
        .then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(console_layer)
        .init();
    // Route whisper.cpp / ggml log lines through tracing.
    whisper_rs::install_logging_hooks();
    guard
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Global options, removed before the mode is picked.
    let model_arg = take_option(&mut args, "--model").map(PathBuf::from);
    let dump_dir = take_option(&mut args, "--dump-audio").map(PathBuf::from);
    let device_arg = take_option(&mut args, "--device");
    let models_dir = paths::models_dir()?;
    let default_model = || models_dir.join(config::Config::default().stt.model);

    match args.first().map(String::as_str) {
        None => {
            if !platform::acquire_main_instance() {
                platform::error_box("VoicePaste уже запущен — ищите иконку в трее.");
                return Ok(());
            }
            app::run(app::RunOptions {
                config_path: paths::config_path()?,
                models_dir: models_dir.clone(),
                logs_dir: paths::logs_dir()?,
                model_override: model_arg,
                device_override: device_arg,
                dump_dir,
            })
        }
        Some("--settings") => settings_ui::run(
            paths::config_path()?,
            models_dir.clone(),
            paths::logs_dir()?,
        ),
        Some("--demo-overlay") => overlay::demo(),
        Some("--list-devices") => {
            for name in audio::list_input_devices()? {
                println!("{name}");
            }
            Ok(())
        }
        Some("--test-mic") => {
            let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5);
            let language = args.get(2).map(String::as_str).unwrap_or("auto");
            let model = model_arg.unwrap_or_else(default_model);
            test_mic(secs, &model, language, &device_arg.unwrap_or_default())
        }
        Some("--transcribe-file") => {
            let wav = args.get(1).context("--transcribe-file needs a wav path")?;
            let language = args.get(2).map(String::as_str).unwrap_or("auto");
            let beam: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(5);
            let prompt = args.get(4).map(String::as_str);
            let model = model_arg.unwrap_or_else(default_model);
            transcribe_file(Path::new(wav), &model, language, beam, prompt)
        }
        _ => {
            eprintln!(
                "usage: voicepaste [--model <path>] [--device <name>] [--dump-audio <dir>]
       voicepaste --list-devices
       voicepaste [--model <path>] [--device <name>] --test-mic [seconds] [language]
       voicepaste [--model <path>] --transcribe-file <wav> [language] [beam] [prompt|\"-\"]"
            );
            Ok(())
        }
    }
}

/// Removes `--name value` from `args` and returns the value.
fn take_option(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    if i + 1 >= args.len() {
        return None;
    }
    let value = args.remove(i + 1);
    args.remove(i);
    Some(value)
}

/// Debug: run a saved WAV through the model, to compare settings offline.
fn transcribe_file(
    wav_path: &Path,
    model: &Path,
    language: &str,
    beam: i32,
    prompt: Option<&str>,
) -> Result<()> {
    let (mono, rate) = wav::read_mono(wav_path)?;
    let pcm = audio::resample_to_16k(&mono, rate)?;
    let mut cfg = stt::SttConfig::with_model(model);
    cfg.language = language.to_string();
    cfg.beam_size = beam;
    match prompt {
        Some("-") => cfg.initial_prompt.clear(),
        Some(p) => cfg.initial_prompt = p.to_string(),
        None => {}
    }
    let mut engine = stt::SttEngine::load(&cfg)?;
    let trimmed = audio::trim_silence(&pcm, 300);
    println!(
        "audio {:.2} s, after trim {:.2} s; language={language} beam={beam} prompt={:?}",
        pcm.len() as f32 / audio::WHISPER_RATE as f32,
        trimmed.len() as f32 / audio::WHISPER_RATE as f32,
        cfg.initial_prompt
    );
    for (label, data) in [("full", &pcm[..]), ("trimmed", trimmed)] {
        let t = Instant::now();
        let text = engine.transcribe(data)?;
        println!("[{label}] {:.2?}: {text:?}", t.elapsed());
    }
    Ok(())
}

/// Stage 1: record N seconds, transcribe, print text and timings.
fn test_mic(secs: u64, model: &Path, language: &str, device: &str) -> Result<()> {
    let mut cfg = stt::SttConfig::with_model(model);
    cfg.language = language.to_string();

    println!(
        "loading model {} (gpu requested: {})...",
        model.display(),
        cfg.use_gpu
    );
    let t = Instant::now();
    let mut engine = stt::SttEngine::load(&cfg).context("model load failed")?;
    println!("model loaded in {:.2?}", t.elapsed());

    // Warm-up: one second of silence, so the timing below is representative.
    let t = Instant::now();
    engine.transcribe(&vec![0.0f32; audio::WHISPER_RATE as usize])?;
    println!("warm-up took {:.2?}", t.elapsed());

    println!("\nSpeak now, recording {secs} s...");
    let t = Instant::now();
    let rec = audio::Recorder::start(device, secs as u32 + 5)?;
    println!("stream opened in {:.0?}", t.elapsed());
    println!("recording from \"{}\"", rec.device_name);
    std::thread::sleep(Duration::from_secs(secs));
    let pcm = rec.stop()?.into_16k()?;
    println!(
        "recorded {:.2} s of audio",
        pcm.len() as f32 / audio::WHISPER_RATE as f32
    );

    let trimmed = audio::trim_silence(&pcm, 300);
    if trimmed.len() < audio::WHISPER_RATE as usize * 3 / 10 {
        println!(
            "no speech detected (after silence trimming: {} samples)",
            trimmed.len()
        );
        return Ok(());
    }

    let t = Instant::now();
    let text = engine.transcribe(trimmed)?;
    let took = t.elapsed();
    println!(
        "\ntranscribed {:.2} s of speech in {:.2?}\ntext: {text}",
        trimmed.len() as f32 / audio::WHISPER_RATE as f32,
        took
    );
    Ok(())
}
