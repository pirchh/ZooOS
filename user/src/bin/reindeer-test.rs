//! ReindeerTest: the microphone test (docs/microphone-design.md). Records
//! from the microphone until it has `SECONDS` of audio, transcribes it
//! with Moonshine (`models/hear.gguf`, the hearing image) and compares the
//! text with `test/mic.txt`. When there is sound, one second from its
//! start must also equal the same second of `test/mic.raw`, what the host
//! plays: the samples arrive unchanged. Exits 0 when heard right, 1 when
//! not, 2 without a microphone or model, 3 if the microphone delivered
//! too little in time, 4 if the samples differ from the source.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use zoo_infer::hear::{self, Hearing};
use zoo_rt::Error;

zoo_rt::entry!(main);

/// What is recorded: the host's recording plays for about 5 s.
const SECONDS: usize = 6;
/// Ticks (10 ms) to wait for the samples: twice the recording, as TCG may
/// run slower than the device's clock.
const PATIENCE: u32 = (SECONDS as u32) * 200;

fn file(name: &str) -> Option<&'static [u8]> {
    zoo_rt::fs_find(name).and_then(|i| zoo_rt::fs_map(i).ok())
}

fn main(_start: u64) -> u8 {
    let cpus = zoo_rt::cpu_count();
    if cpus > 1
        && let Some(pool) = zoo_rt::pool::Workers::start(cpus - 1)
    {
        zoo_infer::par::set_pool(pool);
    }
    let (Some(model), Some(want)) = (file("models/hear.gguf"), file("test/mic.txt")) else {
        let _ = zoo_rt::log("ReindeerTest: the boot image has no hearing model or test/mic.txt");
        return 2;
    };
    let want = core::str::from_utf8(want).unwrap_or("").trim();
    let Ok(g) = zoo_infer::Gguf::parse(model) else {
        let _ = zoo_rt::log("ReindeerTest: the hearing model is not a GGUF file");
        return 2;
    };
    let h = match Hearing::from_gguf(&g) {
        Ok(h) => h,
        Err(e) => {
            zoo_rt::log!("ReindeerTest: {}", e.name());
            return 2;
        }
    };

    let total = SECONDS * hear::RATE * 2;
    let mut pcm: Vec<u8> = Vec::with_capacity(total);
    let mut buf = [0u8; 8192];
    let t = zoo_rt::now_us();
    let mut ticks = 0;
    while pcm.len() < total {
        match zoo_rt::audio_read(&mut buf) {
            Ok(0) => {
                ticks += 1;
                if ticks > PATIENCE {
                    zoo_rt::log!(
                        "ReindeerTest: only {} ms recorded in {} ms",
                        pcm.len() / 32,
                        (zoo_rt::now_us() - t) / 1000
                    );
                    return 3;
                }
                zoo_rt::sleep(1);
            }
            Ok(n) => pcm.extend_from_slice(&buf[..n.min(total - pcm.len())]),
            Err(Error::NotFound) => {
                let _ = zoo_rt::log("ReindeerTest: no microphone");
                return 2;
            }
            Err(e) => {
                zoo_rt::log!("ReindeerTest: audio_read failed: {}", e.name());
                return 2;
            }
        }
    }
    let audio = hear::samples_from_i16(&pcm);
    let peak = audio.iter().fold(0f32, |m, a| m.max(a.abs()));
    zoo_rt::log!(
        "ReindeerTest: recorded {} ms in {} ms (peak {}%)",
        audio.len() * 1000 / hear::RATE,
        (zoo_rt::now_us() - t) / 1000,
        (peak * 100.0) as u32
    );
    if let Some(code) = compare_with_source(&pcm) {
        return code;
    }
    let t = zoo_rt::now_us();
    let text = match h.transcribe(&audio) {
        Ok(text) => text,
        Err(e) => {
            zoo_rt::log!("ReindeerTest: {}", e.name());
            return 1;
        }
    };
    let ok = hear::same_words(&text, want);
    zoo_rt::log!(
        "ReindeerTest: {} in {} ms: \"{}\"",
        if ok { "heard" } else { "MISHEARD" },
        (zoo_rt::now_us() - t) / 1000,
        text
    );
    if ok { 0 } else { 1 }
}

/// Index of the first sample louder than 1% of full scale.
fn first_sound(pcm: &[u8]) -> Option<usize> {
    pcm.as_chunks::<2>()
        .0
        .iter()
        .position(|b| i16::from_le_bytes(*b).unsigned_abs() > 327)
}

/// Compares one second from the first sound with the source; `Some(4)`
/// when they differ, `None` to go on (equal, or no sound or no source).
fn compare_with_source(pcm: &[u8]) -> Option<u8> {
    let source = file("test/mic.raw")?;
    let (Some(a), Some(b)) = (first_sound(pcm), first_sound(source)) else {
        let _ = zoo_rt::log("ReindeerTest: no sound recorded");
        return None;
    };
    let n = 2 * hear::RATE;
    let (Some(got), Some(want)) = (pcm.get(2 * a..2 * a + n), source.get(2 * b..2 * b + n)) else {
        let _ = zoo_rt::log("ReindeerTest: less than a second after the first sound");
        return Some(4);
    };
    let differ = got
        .as_chunks::<2>()
        .0
        .iter()
        .zip(want.as_chunks::<2>().0)
        .filter(|(x, y)| x != y)
        .count();
    if differ == 0 {
        zoo_rt::log!(
            "ReindeerTest: 1 s from the first sound (at {} ms) equals the source",
            a / 16
        );
        None
    } else {
        zoo_rt::log!(
            "ReindeerTest: {} of 16000 samples differ from the source",
            differ
        );
        Some(4)
    }
}
