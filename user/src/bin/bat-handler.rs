//! BatHandler: Bella's hearing (docs/listening-design.md). Started by Bella
//! with a channel. It owns the microphone, finds utterances with the voice
//! detector, transcribes each with Moonshine, and sends Bella
//! `heard:<text>` for those that speak to her ("hey Bella, ..."). Her name
//! alone ("Hey Bella." and a pause) makes the next utterance within
//! `AFTER_NAME_US` the request. Speech not for her is never logged as
//! words: only its length and level (docs/listening-design.md §7).
//! `none:<why>` and an exit when there is no hearing model or no
//! microphone. It ends when Bella's channel closes.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use zoo_infer::hear::{self, Hearing};
use zoo_infer::listen::Detector;
use zoo_rt::{Error, Handle, close, try_recv};

zoo_rt::entry!(main);

/// Ticks (10 ms) between reads when the microphone has nothing new.
const IDLE_TICKS: u64 = 2;
/// After her name alone, how long the request may take to start (5 s).
const AFTER_NAME_US: u64 = 5_000_000;
/// The room's level is logged when it moves this far (dB) from the last
/// logged value, at most every `ROOM_LOG_US` (30 s).
const ROOM_LOG_DB: i32 = 10;
const ROOM_LOG_US: u64 = 30_000_000;

fn say(chan: Handle, prefix: &str, text: &str) {
    let mut m = Vec::with_capacity(prefix.len() + text.len());
    m.extend_from_slice(prefix.as_bytes());
    m.extend_from_slice(text.as_bytes());
    m.truncate(zoo_abi::MAX_MESSAGE);
    let _ = zoo_rt::send(chan, &m);
}

/// Moonshine's bytes: from the boot image when it has them, else read
/// whole from the model disk into page-aligned memory.
fn model_bytes() -> Option<&'static [u8]> {
    if let Some(f) = zoo_rt::fs_find(zoo_abi::HEAR_MODEL_FILE).and_then(|i| zoo_rt::fs_map(i).ok())
    {
        return Some(f);
    }
    let len = usize::try_from(zoo_rt::model_size_of(zoo_abi::MODEL_HEAR).ok()?).ok()?;
    let size = len.div_ceil(zoo_abi::BLOCK_BYTES) * zoo_abi::BLOCK_BYTES;
    let layout = core::alloc::Layout::from_size_align(size, 4096).ok()?;
    // SAFETY: `layout` has a non-zero size (`len` > 0, checked by the
    // kernel when it read the model disk's header).
    let p = unsafe { alloc::alloc::alloc(layout) };
    if p.is_null() {
        return None;
    }
    // SAFETY: `p` is a fresh allocation of `size` bytes, owned here for
    // the rest of the process (never freed).
    let buf: &'static mut [u8] = unsafe { core::slice::from_raw_parts_mut(p, size) };
    let t = zoo_rt::now_us();
    let run = zoo_abi::BLOCK_RUN_MAX * zoo_abi::BLOCK_BYTES;
    for (k, chunk) in buf.chunks_mut(run).enumerate() {
        let block = (k * zoo_abi::BLOCK_RUN_MAX) as u64;
        zoo_rt::model_read_of(zoo_abi::MODEL_HEAR, block, chunk).ok()?;
    }
    zoo_rt::log!(
        "BatHandler: read the hearing model from the model disk: {} MB in {} ms",
        len >> 20,
        (zoo_rt::now_us() - t) / 1000
    );
    Some(&buf[..len])
}

fn main(chan: u64) -> u8 {
    if chan == zoo_rt::NO_HANDLE {
        return 2;
    }
    // The microphone first: without one, the model is not worth reading.
    // This read starts recording; the detector gets what follows.
    let mut buf = [0u8; 8192];
    match zoo_rt::audio_read(&mut buf) {
        Ok(_) => {}
        Err(Error::NotFound) => {
            say(chan, "none:", "no microphone");
            return 0;
        }
        Err(Error::Busy) => {
            say(chan, "none:", "the microphone is in use");
            return 0;
        }
        Err(e) => {
            say(chan, "none:", e.name());
            return 0;
        }
    }
    let Some(model) = model_bytes() else {
        say(chan, "none:", "no hearing model");
        return 0;
    };
    let g = match zoo_infer::Gguf::parse(model) {
        Ok(g) => g,
        Err(_) => {
            say(chan, "none:", "the hearing model is not a GGUF file");
            return 0;
        }
    };
    let h = match Hearing::from_gguf(&g) {
        Ok(h) => h,
        Err(e) => {
            say(chan, "none:", e.name());
            return 0;
        }
    };
    let cpus = zoo_rt::cpu_count();
    if cpus > 1
        && let Some(pool) = zoo_rt::pool::Workers::start(cpus - 1)
    {
        zoo_infer::par::set_pool(pool);
    }
    let mut det = Detector::new();
    let mut msg = [0u8; 64];
    let mut listening = false;
    // The room's level as the detector sees it, logged when it settles and
    // when it shifts, for tuning against a real microphone.
    let mut room_logged: Option<(i32, u64)> = None;
    let mut samples_in = 0usize;
    // Until when an utterance is the request after her name alone.
    let mut name_until: Option<u64> = None;
    loop {
        // Bella gone: stop listening (the microphone is released at exit).
        match try_recv(chan, &mut msg) {
            Ok(_) | Err(Error::Again) => {}
            Err(_) => {
                let _ = close(chan);
                return 0;
            }
        }
        let n = match zoo_rt::audio_read(&mut buf) {
            Ok(n) => n,
            Err(Error::NotFound) => {
                say(chan, "none:", "no microphone");
                return 0;
            }
            Err(Error::Busy) => {
                say(chan, "none:", "the microphone is in use");
                return 0;
            }
            Err(e) => {
                say(chan, "none:", e.name());
                return 0;
            }
        };
        if !listening {
            listening = true;
            zoo_rt::log!(
                "BatHandler: listening ({} MB model, {} threads)",
                model.len() >> 20,
                zoo_infer::par::width()
            );
            say(chan, "listening", "");
        }
        if n == 0 {
            zoo_rt::sleep(IDLE_TICKS);
            continue;
        }
        let samples: Vec<i16> = buf[..n]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        samples_in += samples.len();
        let now = zoo_rt::now_us();
        if let Some(bg) = det.background_db()
            && samples_in >= 3 * hear::RATE
            && room_logged.is_none_or(|(r, at)| {
                (bg as i32 - r).abs() >= ROOM_LOG_DB && now - at >= ROOM_LOG_US
            })
        {
            room_logged = Some((bg as i32, now));
            zoo_rt::log!("BatHandler: room level {} dB", bg as i32);
        }
        for u in det.push(&samples) {
            let audio: Vec<f32> = u.iter().map(|&s| f32::from(s) / 32768.0).collect();
            let t = zoo_rt::now_us();
            let Ok(text) = h.transcribe(&audio) else {
                continue;
            };
            let ms = (zoo_rt::now_us() - t) / 1000;
            let speech_ms = audio.len() * 1000 / hear::RATE;
            let level = zoo_infer::listen::level_db(&u) as i32;
            let after_name = name_until.take().is_some_and(|until| t <= until);
            if zoo_abi::bella::addressed(&text).is_some() {
                zoo_rt::log!(
                    "BatHandler: heard \"{}\" ({} ms of speech at {} dB, {} ms to hear)",
                    text,
                    speech_ms,
                    level,
                    ms
                );
                say(chan, "heard:", &text);
            } else if zoo_abi::bella::name_only(&text) {
                zoo_rt::log!(
                    "BatHandler: heard her name (\"{}\"); the request may follow",
                    text
                );
                name_until = Some(zoo_rt::now_us() + AFTER_NAME_US);
                say(chan, "name", "");
            } else if after_name {
                zoo_rt::log!(
                    "BatHandler: heard \"{}\" after her name ({} ms of speech at {} dB, {} ms to hear)",
                    text,
                    speech_ms,
                    level,
                    ms
                );
                say(chan, "heard:", &text);
            } else {
                // Not for Bella: what was said stays unlogged.
                zoo_rt::log!(
                    "BatHandler: speech not for Bella ({} ms at {} dB)",
                    speech_ms,
                    level
                );
            }
        }
    }
}
