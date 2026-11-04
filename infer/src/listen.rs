//! Voice detection (docs/listening-design.md): finds where speech starts
//! and ends in 16 kHz mono audio by its loudness against the background,
//! so speech to text only sees utterances (Moonshine writes "You" for
//! silence). No model: frames of 20 ms; the background is the quietest
//! frame of the last 3 seconds (speech dips between syllables, steady noise
//! does not), and speech is a frame well above it.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// Samples in one 20 ms frame at 16 kHz.
pub const FRAME: usize = 320;
/// A frame is speech this many dB above the background...
const ABOVE_DB: f32 = 12.0;
/// ...and above this absolute level (dB of full scale).
const FLOOR_DB: f32 = -50.0;
/// Speech starts after this many speech frames in a row (60 ms).
const START_FRAMES: usize = 3;
/// It ends after this many quiet frames (700 ms): pauses between words
/// stay inside one utterance.
const END_FRAMES: usize = 35;
/// Audio kept from before the start (200 ms), so the first sound is whole.
const PRE_FRAMES: usize = 10;
/// The longest utterance (12 s); a longer one is cut there.
const MAX_FRAMES: usize = 600;
/// Utterances with fewer loud frames than this (200 ms) are dropped as
/// clicks.
const MIN_LOUD: usize = 10;
/// Frames over which the background is the quietest (3 s): longer than
/// any unbroken sound in speech, short enough that a fan turned up is
/// background within seconds.
const WINDOW: usize = 150;
/// Below this a frame is digital silence (a device starting, a muted
/// input): it says nothing about the room and is left out (-90 dBFS).
const SILENT_DB: f32 = -90.0;

/// Loudness of a frame in dB of full scale.
pub fn level_db(frame: &[i16]) -> f32 {
    let sum: f64 = frame.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    let ms = sum / frame.len().max(1) as f64 / (32768.0 * 32768.0);
    10.0 * libm::log10f(ms.max(1e-12) as f32)
}

pub struct Detector {
    /// Samples not yet a whole frame.
    partial: Vec<i16>,
    /// Recent quiet frames, for the start of an utterance.
    before: VecDeque<Vec<i16>>,
    /// The utterance so far, when in one.
    speech: Option<Vec<i16>>,
    /// Levels of the last `WINDOW` frames that were not digital silence.
    levels: VecDeque<f32>,
    loud_run: usize,
    quiet_run: usize,
    /// Loud frames in the utterance so far.
    loud_in: usize,
    frames_in: usize,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    pub fn new() -> Detector {
        Detector {
            partial: Vec::new(),
            before: VecDeque::new(),
            speech: None,
            levels: VecDeque::new(),
            loud_run: 0,
            quiet_run: 0,
            loud_in: 0,
            frames_in: 0,
        }
    }

    /// The background level now (dB of full scale), once a frame was seen.
    pub fn background_db(&self) -> Option<f32> {
        self.levels.iter().copied().reduce(f32::min)
    }

    /// Feeds samples; returns every utterance they complete, in order.
    pub fn push(&mut self, samples: &[i16]) -> Vec<Vec<i16>> {
        let mut done = Vec::new();
        self.partial.extend_from_slice(samples);
        let whole = self.partial.len() / FRAME * FRAME;
        let frames: Vec<Vec<i16>> = self.partial[..whole].chunks(FRAME).map(Vec::from).collect();
        self.partial.drain(..whole);
        for f in frames {
            if let Some(u) = self.frame(f) {
                done.push(u);
            }
        }
        done
    }

    /// Ends an utterance in progress (the audio stopped).
    pub fn finish(&mut self) -> Option<Vec<i16>> {
        let u = self.speech.take()?;
        self.quiet_run = 0;
        (self.loud_in >= MIN_LOUD).then_some(u)
    }

    fn frame(&mut self, f: Vec<i16>) -> Option<Vec<i16>> {
        let db = level_db(&f);
        if db > SILENT_DB {
            if self.levels.len() == WINDOW {
                self.levels.pop_front();
            }
            self.levels.push_back(db);
        }
        let bg = self.background_db().unwrap_or(db);
        let loud = db > SILENT_DB && db > bg + ABOVE_DB && db > FLOOR_DB;
        self.frames_in += 1;
        if let Some(sp) = self.speech.as_mut() {
            sp.extend_from_slice(&f);
            self.quiet_run = if loud { 0 } else { self.quiet_run + 1 };
            self.loud_in += usize::from(loud);
            if self.quiet_run >= END_FRAMES || sp.len() / FRAME >= MAX_FRAMES {
                let mut u = self.speech.take()?;
                // Trim the quiet tail to what was kept before the start.
                let keep_tail = PRE_FRAMES.min(self.quiet_run);
                let cut = (self.quiet_run - keep_tail) * FRAME;
                u.truncate(u.len().saturating_sub(cut));
                self.quiet_run = 0;
                self.loud_run = 0;
                return (self.loud_in >= MIN_LOUD).then_some(u);
            }
            return None;
        }
        self.loud_run = if loud { self.loud_run + 1 } else { 0 };
        self.before.push_back(f);
        if self.before.len() > PRE_FRAMES + START_FRAMES {
            self.before.pop_front();
        }
        if self.loud_run >= START_FRAMES {
            let mut sp = Vec::with_capacity(MAX_FRAMES * FRAME);
            for b in self.before.drain(..) {
                sp.extend_from_slice(&b);
            }
            self.speech = Some(sp);
            self.quiet_run = 0;
            self.loud_in = START_FRAMES;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;

    /// A 300 Hz tone of `ms` at `amp`, or noise-like quiet (`amp` small).
    fn tone(ms: usize, amp: f32) -> Vec<i16> {
        (0..ms * 16)
            .map(|i| {
                (amp * 32767.0
                    * libm::sinf(i as f32 * 2.0 * core::f32::consts::PI * 300.0 / 16000.0))
                    as i16
            })
            .collect()
    }

    fn quiet(ms: usize) -> Vec<i16> {
        // A faint hiss: about -70 dB.
        (0..ms * 16)
            .map(|i| if i % 2 == 0 { 10 } else { -10 })
            .collect()
    }

    /// Room noise: a fixed pseudo-random hiss of about `db` dB of full
    /// scale.
    fn room(ms: usize, db: f32, seed: &mut u32) -> Vec<i16> {
        // Uniform in -a..a has RMS a / sqrt(3).
        let a = libm::powf(10.0, db / 20.0) * 32767.0 * 1.732;
        (0..ms * 16)
            .map(|_| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((*seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * a
            })
            .map(|v| v as i16)
            .collect()
    }

    /// What `run --mic` showed (docs/listening-design.md §6): the
    /// microphone starts with digital silence, then a room's noise at
    /// -45 dB. The noise must become background, not 12-second
    /// "utterances", and the one request after it must be found whole.
    #[test]
    fn digital_silence_then_room_noise() {
        let mut seed = 1;
        let mut d = Detector::new();
        let mut a = vec![0i16; 16 * 300];
        a.extend(room(3000, -45.0, &mut seed));
        let speech_at = a.len();
        let burst = tone(1500, 0.1);
        let noise = room(1500, -45.0, &mut seed);
        a.extend(burst.iter().zip(&noise).map(|(s, n)| s.saturating_add(*n)));
        a.extend(room(3000, -45.0, &mut seed));
        let us = d.push(&a);
        assert_eq!(
            us.len(),
            1,
            "utterances of {:?} ms",
            us.iter().map(|u| u.len() / 16).collect::<Vec<_>>()
        );
        let ms = us[0].len() / 16;
        assert!((1600..=1960).contains(&ms), "{ms} ms");
        let _ = speech_at;
    }

    /// Room noise that changes level (a fan turned up) becomes background
    /// within the 3-second window when nobody speaks.
    #[test]
    fn louder_room_becomes_background() {
        let mut seed = 7;
        let mut d = Detector::new();
        let mut a = room(2000, -60.0, &mut seed);
        a.extend(room(8000, -40.0, &mut seed));
        let us = d.push(&a);
        let total: usize = us.iter().map(|u| u.len() / 16).sum();
        assert!(total <= 4000, "{total} ms of noise taken as speech");
        assert!(d.push(&room(5000, -40.0, &mut seed)).is_empty());
    }

    #[test]
    fn one_burst_is_one_utterance() {
        let mut d = Detector::new();
        let mut a = quiet(1000);
        a.extend(tone(1500, 0.3));
        a.extend(quiet(2000));
        let us = d.push(&a);
        assert_eq!(us.len(), 1);
        let ms = us[0].len() / 16;
        // The burst, the 200 ms before it, and at most 200 ms of the tail.
        assert!((1600..=1960).contains(&ms), "{ms} ms");
    }

    #[test]
    fn silence_and_hiss_are_nothing() {
        let mut d = Detector::new();
        assert!(d.push(&vec![0; 16000 * 5]).is_empty());
        assert!(d.push(&quiet(5000)).is_empty());
        assert!(d.finish().is_none());
    }

    #[test]
    fn pauses_between_words_stay_inside() {
        let mut d = Detector::new();
        let mut a = quiet(500);
        a.extend(tone(600, 0.3));
        a.extend(quiet(400));
        a.extend(tone(600, 0.3));
        a.extend(quiet(2000));
        assert_eq!(d.push(&a).len(), 1);
    }

    #[test]
    fn two_requests_are_two() {
        let mut d = Detector::new();
        let mut a = quiet(500);
        a.extend(tone(800, 0.3));
        a.extend(quiet(1500));
        a.extend(tone(800, 0.3));
        a.extend(quiet(1500));
        assert_eq!(d.push(&a).len(), 2);
    }

    #[test]
    fn a_click_is_dropped() {
        let mut d = Detector::new();
        let mut a = quiet(500);
        a.extend(tone(100, 0.5));
        a.extend(quiet(2000));
        assert!(d.push(&a).is_empty());
    }

    #[test]
    fn steady_noise_becomes_background() {
        let mut d = Detector::new();
        // A fan starts and keeps going: one false start at most, then
        // nothing (it is cut at 12 s and the background catches up).
        let mut a = quiet(500);
        a.extend(tone(30_000, 0.05));
        let n = d.push(&a).len();
        assert!(n <= 1, "{n} utterances from steady noise");
        assert!(d.push(&tone(10_000, 0.05)).is_empty());
    }
}
