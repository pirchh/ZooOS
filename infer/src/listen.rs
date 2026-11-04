//! Voice detection (docs/listening-design.md): finds where speech starts
//! and ends in 16 kHz mono audio by its loudness against the background,
//! so speech to text only sees utterances (Moonshine writes "You" for
//! silence). No model: frames of 20 ms, a background level that follows
//! the quietest frames, speech when a frame is well above it.

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
/// How fast the background level rises, dB per frame (2.5 dB/s), in every
/// frame: a steady noise becomes background within seconds, while a
/// pause in speech pulls it back down at once.
const RISE_DB: f32 = 0.05;

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
    background_db: Option<f32>,
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
            background_db: None,
            loud_run: 0,
            quiet_run: 0,
            loud_in: 0,
            frames_in: 0,
        }
    }

    /// The background level now (dB of full scale), once a frame was seen.
    pub fn background_db(&self) -> Option<f32> {
        self.background_db
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
        let bg = *self.background_db.get_or_insert(db);
        let loud = db > bg + ABOVE_DB && db > FLOOR_DB;
        // The background follows quieter frames down at once, louder ones
        // up slowly.
        self.background_db = Some(if db < bg {
            db
        } else {
            bg + RISE_DB.min(db - bg)
        });
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
