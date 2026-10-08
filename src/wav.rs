//! Minimal WAV reader/writer for debugging (`--dump-audio`, `--transcribe-file`).
//! Writes 32-bit float mono; reads PCM16 and float32, any channel count.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

pub fn write_mono_f32(path: &Path, samples: &[f32], rate: u32) -> Result<()> {
    let data_len = (samples.len() * 4) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    fs::write(path, out).with_context(|| format!("cannot write {}", path.display()))
}

/// Returns mono samples (channels averaged) and the sample rate.
pub fn read_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    let b = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        bail!("not a WAV file");
    }
    let (mut fmt, mut channels, mut rate, mut bits) = (0u16, 0usize, 0u32, 0u16);
    let mut data: Option<&[u8]> = None;
    let mut pos = 12;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let len = u32::from_le_bytes(b[pos + 4..pos + 8].try_into()?) as usize;
        let body = &b[pos + 8..(pos + 8 + len).min(b.len())];
        match id {
            b"fmt " if body.len() >= 16 => {
                fmt = u16::from_le_bytes(body[0..2].try_into()?);
                channels = u16::from_le_bytes(body[2..4].try_into()?) as usize;
                rate = u32::from_le_bytes(body[4..8].try_into()?);
                bits = u16::from_le_bytes(body[14..16].try_into()?);
            }
            b"data" => data = Some(body),
            _ => {}
        }
        pos += 8 + len + (len & 1);
    }
    let data = data.context("no data chunk")?;
    if channels == 0 {
        bail!("no fmt chunk");
    }
    let samples: Vec<f32> = match (fmt, bits) {
        (1, 16) => data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c) as f32 / 32768.0)
            .collect(),
        (3, 32) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect(),
        _ => bail!("unsupported WAV format (tag {fmt}, {bits} bits)"),
    };
    let mono = samples
        .chunks_exact(channels)
        .map(|f| f.iter().sum::<f32>() / channels as f32)
        .collect();
    Ok((mono, rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join("voicepaste_wav_test.wav");
        let src = vec![0.0f32, 0.5, -0.5, 1.0];
        write_mono_f32(&dir, &src, 16_000).unwrap();
        let (back, rate) = read_mono(&dir).unwrap();
        let _ = std::fs::remove_file(&dir);
        assert_eq!(rate, 16_000);
        assert_eq!(back, src);
    }
}
