//! Microphone capture (cpal) and conversion to 16 kHz mono f32 for Whisper.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

pub const WHISPER_RATE: u32 = 16_000;

/// Names of all input devices, for the settings UI and `--list-devices`.
pub fn list_input_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let devices = host
        .input_devices()
        .context("cannot enumerate input devices")?;
    Ok(devices
        .filter_map(|d| d.description().ok().map(|desc| desc.name().to_string()))
        .collect())
}

fn find_device(name: &str) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if !name.is_empty() {
        let wanted = name.to_lowercase();
        let named: Vec<(String, cpal::Device)> = host
            .input_devices()?
            .filter_map(|d| {
                let n = d.description().ok()?.name().to_string();
                Some((n, d))
            })
            .collect();
        // Exact name wins, otherwise the first device whose name contains the text.
        let pick = named.iter().position(|(n, _)| n == name).or_else(|| {
            named
                .iter()
                .position(|(n, _)| n.to_lowercase().contains(&wanted))
        });
        match pick {
            Some(i) => {
                return Ok(named
                    .into_iter()
                    .nth(i)
                    .map(|(_, d)| d)
                    .expect("index is valid"))
            }
            None => tracing::warn!("input device '{name}' not found, using the default one"),
        }
    }
    host.default_input_device()
        .ok_or_else(|| anyhow!("no default input device"))
}

/// A running capture. The stream lives only between `start` and `stop`, so the
/// Windows microphone indicator is off while idle.
pub struct Recorder {
    stream: Stream,
    buf: Arc<Mutex<Vec<f32>>>,
    rate: u32,
    pub device_name: String,
}

impl Recorder {
    /// `device_name` empty means the default device.
    pub fn start(device_name: &str, max_seconds: u32) -> Result<Self> {
        let device = find_device(device_name)?;
        let device_name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "unknown".into());
        let supported = device
            .default_input_config()
            .context("cannot get default input config")?;
        let rate = supported.sample_rate();
        let channels = supported.channels() as usize;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();

        let buf = Arc::new(Mutex::new(Vec::with_capacity(
            rate as usize * max_seconds as usize,
        )));

        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, config, channels, &buf)?,
            SampleFormat::I16 => build::<i16>(&device, config, channels, &buf)?,
            SampleFormat::I32 => build::<i32>(&device, config, channels, &buf)?,
            SampleFormat::U16 => build::<u16>(&device, config, channels, &buf)?,
            other => bail!("unsupported sample format {other}"),
        };
        stream.play().context("cannot start input stream")?;
        Ok(Self {
            stream,
            buf,
            rate,
            device_name,
        })
    }

    /// Stops capture. Resampling is left to the caller (`Captured::into_16k`)
    /// so it can run off the main thread.
    pub fn stop(self) -> Result<Captured> {
        drop(self.stream);
        let mono = std::mem::take(
            &mut *self
                .buf
                .lock()
                .map_err(|_| anyhow!("audio buffer poisoned"))?,
        );
        Ok(Captured {
            mono,
            rate: self.rate,
        })
    }
}

/// Live input level for the settings window. Opens the microphone like a recording
/// does, but keeps only the peak of the latest audio block.
pub struct LevelMeter {
    _stream: Stream,
    level: Arc<AtomicU32>,
    pub device_name: String,
}

impl LevelMeter {
    pub fn start(device_name: &str) -> Result<Self> {
        let device = find_device(device_name)?;
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "unknown".into());
        let supported = device
            .default_input_config()
            .context("cannot get default input config")?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let level = Arc::new(AtomicU32::new(0));

        let stream = match format {
            SampleFormat::F32 => meter_stream::<f32>(&device, config, &level)?,
            SampleFormat::I16 => meter_stream::<i16>(&device, config, &level)?,
            SampleFormat::I32 => meter_stream::<i32>(&device, config, &level)?,
            SampleFormat::U16 => meter_stream::<u16>(&device, config, &level)?,
            other => bail!("unsupported sample format {other}"),
        };
        stream.play().context("cannot start input stream")?;
        Ok(Self {
            _stream: stream,
            level,
            device_name: name,
        })
    }

    /// Peak (0.0..=1.0) of the most recent audio block.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
}

fn meter_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    level: &Arc<AtomicU32>,
) -> Result<Stream>
where
    T: SizedSample + Copy,
    f32: FromSample<T>,
{
    let level = Arc::clone(level);
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            let peak = data
                .iter()
                .map(|&s| f32::from_sample(s).abs())
                .fold(0.0f32, f32::max);
            level.store(peak.min(1.0).to_bits(), Ordering::Relaxed);
        },
        |err| {
            if !matches!(err.kind(), cpal::ErrorKind::Xrun) {
                tracing::error!("audio stream error: {err}");
            }
        },
        None,
    )?;
    Ok(stream)
}

/// Raw mono audio at the device sample rate.
pub struct Captured {
    pub mono: Vec<f32>,
    pub rate: u32,
}

impl Captured {
    pub fn into_16k(self) -> Result<Vec<f32>> {
        resample_to_16k(&self.mono, self.rate)
    }
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    channels: usize,
    buf: &Arc<Mutex<Vec<f32>>>,
) -> Result<Stream>
where
    T: SizedSample + Copy,
    f32: FromSample<T>,
{
    let buf = Arc::clone(buf);
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            // Keep the callback short: convert + downmix straight into the buffer.
            if let Ok(mut out) = buf.lock() {
                downmix_into(data, channels, &mut out);
            }
        },
        |err| {
            // Xruns are routine on shared-mode WASAPI and harmless for dictation.
            if !matches!(err.kind(), cpal::ErrorKind::Xrun) {
                tracing::error!("audio stream error: {err}");
            }
        },
        None,
    )?;
    Ok(stream)
}

fn downmix_into<T>(data: &[T], channels: usize, out: &mut Vec<f32>)
where
    T: Copy,
    f32: FromSample<T>,
{
    if channels <= 1 {
        out.extend(data.iter().map(|&s| f32::from_sample(s)));
    } else {
        out.extend(data.chunks_exact(channels).map(|frame| {
            frame.iter().map(|&s| f32::from_sample(s)).sum::<f32>() / channels as f32
        }));
    }
}

/// Resamples mono audio from `rate` to 16 kHz.
pub fn resample_to_16k(mono: &[f32], rate: u32) -> Result<Vec<f32>> {
    if mono.is_empty() || rate == WHISPER_RATE {
        return Ok(mono.to_vec());
    }
    let mut resampler = Fft::<f32>::new(
        rate as usize,
        WHISPER_RATE as usize,
        1024,
        1,
        FixedSync::Both,
    )
    .map_err(|e| anyhow!("cannot create resampler: {e}"))?;
    let input =
        InterleavedSlice::new(mono, 1, mono.len()).map_err(|e| anyhow!("bad input buffer: {e}"))?;
    let out = resampler
        .process_all(&input, mono.len(), None)
        .map_err(|e| anyhow!("resampling failed: {e}"))?;
    Ok(out.take_data())
}

/// Louder than this (RMS of a 30 ms window) somewhere in the clip = there is speech.
/// Quiet laptop mics peak around 0.02-0.03, so this must stay low.
const MIN_SPEECH_PEAK_RMS: f32 = 0.004;
/// Lowest per-window RMS that still counts as speech.
const ABS_THRESHOLD: f32 = 0.0015;
/// Speech threshold as a fraction of the loudest window, so soft word endings survive.
const REL_THRESHOLD: f32 = 0.05;

/// Trims leading/trailing silence using an adaptive RMS threshold (30 ms
/// windows) and keeps `pad_ms` of margin. The threshold follows the clip's own
/// loudness: a fixed one cut off the quiet tail of sentences on soft mics.
/// Returns an empty slice when no speech is found.
pub fn trim_silence(pcm: &[f32], pad_ms: usize) -> &[f32] {
    let win = (WHISPER_RATE as usize * 30) / 1000;
    let pad = (WHISPER_RATE as usize * pad_ms) / 1000;

    let rms: Vec<f32> = pcm
        .chunks(win)
        .map(|w| (w.iter().map(|s| s * s).sum::<f32>() / w.len() as f32).sqrt())
        .collect();
    let peak = rms.iter().copied().fold(0.0f32, f32::max);
    if peak < MIN_SPEECH_PEAK_RMS {
        return &[];
    }
    let mut sorted = rms.clone();
    sorted.sort_by(f32::total_cmp);
    let noise_floor = sorted[sorted.len() / 10];
    let threshold = ABS_THRESHOLD
        .max(noise_floor * 3.0)
        .max(peak * REL_THRESHOLD);

    let first = rms.iter().position(|&v| v >= threshold);
    let last = rms.iter().rposition(|&v| v >= threshold);
    match (first, last) {
        (Some(f), Some(l)) => {
            let start = (f * win).saturating_sub(pad);
            let end = ((l + 1) * win + pad).min(pcm.len());
            &pcm[start..end]
        }
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_stereo_averages_channels() {
        let mut out = Vec::new();
        downmix_into(&[1.0f32, 0.0, 0.5, 0.5], 2, &mut out);
        assert_eq!(out, vec![0.5, 0.5]);
    }

    #[test]
    fn downmix_converts_i16() {
        let mut out = Vec::new();
        downmix_into(&[i16::MAX, i16::MIN], 1, &mut out);
        assert!(out[0] > 0.99 && out[1] <= -0.99);
    }

    #[test]
    fn resample_48k_to_16k_has_expected_length_and_is_finite() {
        let input: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5)
            .collect();
        let out = resample_to_16k(&input, 48_000).unwrap();
        assert!(
            (out.len() as i64 - 16_000).abs() < 64,
            "len = {}",
            out.len()
        );
        assert!(out.iter().all(|s| s.is_finite()));
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.4 && peak < 0.6, "peak = {peak}");
    }

    #[test]
    fn resample_passthrough_at_16k() {
        let input = vec![0.1f32; 100];
        assert_eq!(resample_to_16k(&input, 16_000).unwrap(), input);
    }

    #[test]
    fn trim_silence_keeps_speech_with_padding() {
        let mut pcm = vec![0.0f32; 16_000];
        pcm.extend(std::iter::repeat_n(0.3, 8_000));
        pcm.extend(vec![0.0f32; 16_000]);
        let trimmed = trim_silence(&pcm, 200);
        assert!(trimmed.len() >= 8_000 && trimmed.len() <= 8_000 + 2 * 3_200 + 960);
    }

    #[test]
    fn trim_silence_returns_empty_for_pure_silence() {
        assert!(trim_silence(&vec![0.0f32; 16_000], 200).is_empty());
    }

    #[test]
    fn trim_silence_keeps_quiet_tail_of_a_soft_mic() {
        // 0.5 s loud word, then a 0.5 s soft word at ~8% of its level, then silence.
        let mut pcm = vec![0.0f32; 8_000];
        pcm.extend(std::iter::repeat_n(0.05, 8_000));
        pcm.extend(std::iter::repeat_n(0.004, 8_000));
        pcm.extend(vec![0.0f32; 16_000]);
        let trimmed = trim_silence(&pcm, 100);
        // Must reach (nearly) the end of the soft word at sample 24_000.
        let end_in_pcm = trimmed.as_ptr() as usize - pcm.as_ptr() as usize;
        let end = end_in_pcm / 4 + trimmed.len();
        assert!(end >= 23_000, "tail was cut, ends at {end}");
    }

    #[test]
    fn trim_silence_ignores_faint_noise() {
        assert!(trim_silence(&vec![0.001f32; 16_000], 200).is_empty());
    }
}
