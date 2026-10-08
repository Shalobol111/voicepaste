//! Short embedded beeps. Playback is asynchronous, so it never blocks the event loop.
//!
//! `PlaySoundW` cannot change the volume, so each (sound, volume) pair gets its own
//! copy of the WAV with scaled samples. Copies are kept for the life of the process:
//! with `SND_ASYNC` the buffer must stay valid until playback ends.

use std::sync::Mutex;

use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    Start,
    Stop,
    Error,
}

static START: &[u8] = include_bytes!("../assets/sounds/start.wav");
static STOP: &[u8] = include_bytes!("../assets/sounds/stop.wav");
static ERROR: &[u8] = include_bytes!("../assets/sounds/error.wav");

type CacheEntry = ((Sound, u8), &'static [u8]);

/// Scaled copies already made, at most `3 * 101` of them.
static CACHE: Mutex<Vec<CacheEntry>> = Mutex::new(Vec::new());

/// Slider percent to amplitude gain. Squared, because loudness is perceived
/// roughly logarithmically: 50 % on the slider should sound about half as loud.
fn gain(volume_percent: u8) -> f32 {
    let v = volume_percent.min(100) as f32 / 100.0;
    v * v
}

/// Returns a copy of a 16-bit PCM WAV with every sample multiplied by `gain`.
/// Anything that is not recognisable 16-bit PCM is returned unchanged.
fn scaled(wav: &[u8], gain: f32) -> Vec<u8> {
    let mut out = wav.to_vec();
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return out;
    }
    let mut pos = 12;
    let mut is_pcm16 = false;
    while pos + 8 <= wav.len() {
        let id = &wav[pos..pos + 4];
        let len =
            u32::from_le_bytes([wav[pos + 4], wav[pos + 5], wav[pos + 6], wav[pos + 7]]) as usize;
        let body = pos + 8;
        let end = (body + len).min(wav.len());
        if id == b"fmt " && end - body >= 16 {
            let tag = u16::from_le_bytes([wav[body], wav[body + 1]]);
            let bits = u16::from_le_bytes([wav[body + 14], wav[body + 15]]);
            is_pcm16 = tag == 1 && bits == 16;
        } else if id == b"data" && is_pcm16 {
            for s in out[body..end].as_chunks_mut::<2>().0 {
                let v = i16::from_le_bytes(*s) as f32 * gain;
                *s = (v.round() as i16).to_le_bytes();
            }
            break;
        }
        pos = body + len + (len & 1);
    }
    out
}

fn buffer(sound: Sound, volume: u8) -> &'static [u8] {
    let base: &'static [u8] = match sound {
        Sound::Start => START,
        Sound::Stop => STOP,
        Sound::Error => ERROR,
    };
    if volume >= 100 {
        return base;
    }
    let Ok(mut cache) = CACHE.lock() else {
        return base;
    };
    if let Some((_, data)) = cache.iter().find(|(key, _)| *key == (sound, volume)) {
        return data;
    }
    let data: &'static [u8] = Box::leak(scaled(base, gain(volume)).into_boxed_slice());
    cache.push(((sound, volume), data));
    data
}

/// Plays `sound` at `volume` percent (0 = silent), cutting off a previous one.
pub fn play(sound: Sound, volume: u8) {
    if volume == 0 {
        return;
    }
    let data = buffer(sound, volume);
    // SAFETY: with SND_MEMORY the first argument points at an in-memory WAV image.
    // `data` is 'static (embedded, or leaked into the cache), so it stays valid for
    // the whole asynchronous playback.
    let ok = unsafe {
        PlaySoundW(
            PCWSTR(data.as_ptr().cast()),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        )
    };
    if !ok.as_bool() {
        tracing::debug!("PlaySoundW failed for {sound:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(wav: &[u8]) -> Vec<i16> {
        wav[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c))
            .collect()
    }

    #[test]
    fn gain_is_silent_at_zero_and_unity_at_full() {
        assert_eq!(gain(0), 0.0);
        assert_eq!(gain(100), 1.0);
        assert!((gain(50) - 0.25).abs() < 1e-6);
        assert_eq!(gain(250), 1.0); // clamped
    }

    #[test]
    fn scaling_halves_the_samples_and_keeps_the_rest() {
        let half = scaled(START, 0.5);
        assert_eq!(half.len(), START.len());
        assert_eq!(&half[..44], &START[..44]); // header untouched
        for (a, b) in samples(START).iter().zip(samples(&half)) {
            assert!((*a as f32 * 0.5 - b as f32).abs() <= 1.0);
        }
    }

    #[test]
    fn scaling_to_zero_gives_silence() {
        assert!(samples(&scaled(ERROR, 0.0)).iter().all(|&s| s == 0));
    }

    #[test]
    fn non_wav_data_is_returned_unchanged() {
        assert_eq!(scaled(b"hello", 0.5), b"hello");
    }

    #[test]
    fn buffers_are_cached_per_sound_and_volume() {
        let a = buffer(Sound::Stop, 40);
        let b = buffer(Sound::Stop, 40);
        assert_eq!(a.as_ptr(), b.as_ptr());
        assert_ne!(a.as_ptr(), buffer(Sound::Stop, 41).as_ptr());
        assert_eq!(buffer(Sound::Stop, 100).as_ptr(), STOP.as_ptr());
    }
}
