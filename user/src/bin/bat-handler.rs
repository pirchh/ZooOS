//! BatHandler: Bella's hearing (docs/listening-design.md). Started by Bella
//! with a channel. It owns the microphone, finds utterances with the voice
//! detector, transcribes each with Moonshine, and sends Bella
//! `heard:<text>` for those that speak to her ("hey Bella, ..."). Her name
//! alone ("Hey Bella." and a pause) makes the next utterance within
//! `AFTER_NAME_US` the request. Speech not for her is never logged as
//! words: only its length and level (docs/listening-design.md §7).
//! While Bella speaks (`mute` to `unmute`) only speech to her or "stop"
//! is passed on, as `barge:<text>` (docs/conversation-design.md).
//! `none:<why>` and an exit when there is no hearing model or no
//! microphone. It ends when Bella's channel closes.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use zoo_infer::hear::{self, Hearing};
use zoo_infer::listen::{Detector, Heard};
use zoo_rt::{Error, Handle, close, try_recv};

zoo_rt::entry!(main);

/// Ticks (10 ms) between reads when the microphone has nothing new.
const IDLE_TICKS: u64 = 2;
/// After her name alone, how long the request may take to start (5 s).
const AFTER_NAME_US: u64 = 5_000_000;
/// After Bella asks a question, how long the answer may take (10 s).
const ANSWER_US: u64 = 10_000_000;
/// After she finishes a spoken request, how long a follow-up may take to
/// start, without her name (docs/follow-design.md).
const FOLLOW_US: u64 = 8_000_000;
/// A sentence heard just before her window opened still counts, for this
/// long after it was written down (docs/late-design.md): on a slow
/// processor her question comes while an answer said quickly is still
/// being written down.
const LATE_US: u64 = 3_000_000;
/// At a pause (docs/endpoint-design.md), the speech so far is written
/// down only when that is expected to take at most this long (400 ms),
/// judged by the last utterance: the 400 ms more an end waits. Audio keeps
/// arriving meanwhile, so a try that is not taken costs next to nothing.
/// Under QEMU's emulated processor (seconds to hear) it is never tried.
const TRY_AT_PAUSE_US: u64 = 400_000;
/// `name_until`'s reason for a follow-up: sent to Bella to decide.
const AS_FOLLOW_UP: &str = "as a follow-up";
/// After she stops speaking, speech that began this soon (500 ms) is
/// still taken as her own voice: the device plays 200 ms after the last
/// write, and a room echoes (docs/conversation-design.md).
const TAIL_US: u64 = 500_000;
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

/// Her window just opened: a sentence written down in the last
/// `LATE_US` is taken as said in it (docs/late-design.md), and the window
/// is used.
fn late(
    chan: Handle,
    held: &mut Option<(String, u64, usize, i32)>,
    window: &mut Option<(u64, &str)>,
) {
    let Some((text, at, speech_ms, level)) = held.take() else {
        return;
    };
    if zoo_rt::now_us().saturating_sub(at) > LATE_US {
        return;
    }
    let Some((_, why)) = window.take() else {
        return;
    };
    if why == AS_FOLLOW_UP {
        zoo_rt::log!(
            "BatHandler: heard speech after her answer ({} ms at {} dB, just before)",
            speech_ms,
            level
        );
        say(chan, "follow:", &text);
    } else {
        zoo_rt::log!(
            "BatHandler: heard \"{}\" {} (said just before; {} ms at {} dB)",
            text,
            why,
            speech_ms,
            level
        );
        say(chan, "heard:", &text);
    }
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
    // `run --mic --show-heard`: every transcript is logged, for tuning;
    // by default speech not for Bella stays unlogged.
    let show_heard = zoo_rt::fs_find("etc/show-heard").is_some();
    if show_heard {
        let _ = zoo_rt::log("BatHandler: showing every transcript (--show-heard)");
    }
    let mut listening = false;
    // The room's level as the detector sees it, logged when it settles and
    // when it shifts, for tuning against a real microphone.
    let mut room_logged: Option<(i32, u64)> = None;
    let mut samples_in = 0usize;
    // Until when an utterance is the request after her name alone.
    // Until when an utterance is the request after her name alone, or
    // the answer to her question, and which.
    let mut name_until: Option<(u64, &str)> = None;
    // The last sentence not for her, kept (never logged) for `LATE_US` in
    // case her window opens just after it: its text, when it was written
    // down, its length and level.
    let mut held: Option<(String, u64, usize, i32)> = None;
    // How long writing down takes, in µs per second of speech, from the
    // last utterance: whether trying at a pause pays.
    let mut hear_rate: Option<u64> = None;
    // While she speaks (`mute` to `unmute`), and when she stopped: what
    // the microphone hears then is mostly her own voice. Only speech to
    // her, or telling her to stop, is passed on (docs/conversation-design.md).
    let mut muted_since: Option<u64> = None;
    let mut unmuted_at: Option<u64> = None;
    loop {
        // Bella gone: stop listening (the microphone is released at exit).
        match try_recv(chan, &mut msg) {
            // Bella asked a question: the next utterance is the answer,
            // with or without her name.
            Ok(n) if &msg[..n] == b"await" => {
                name_until = Some((zoo_rt::now_us() + ANSWER_US, "as her answer"));
                late(chan, &mut held, &mut name_until);
            }
            // She finished a spoken request: the next utterance may follow
            // up on it, for Bella to accept or not.
            Ok(n) if &msg[..n] == b"follow" => {
                if name_until.is_none() {
                    name_until = Some((zoo_rt::now_us() + FOLLOW_US, AS_FOLLOW_UP));
                    late(chan, &mut held, &mut name_until);
                }
            }
            Ok(n) if &msg[..n] == b"mute" => {
                muted_since.get_or_insert(zoo_rt::now_us());
            }
            Ok(n) if &msg[..n] == b"unmute" => {
                let now = zoo_rt::now_us();
                if let Some(since) = muted_since.take() {
                    // A window that opened while she spoke (her question)
                    // counts from when she finished.
                    if let Some((until, _)) = name_until.as_mut() {
                        *until += now - since;
                    }
                }
                unmuted_at = Some(now);
            }
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
        for ev in det.push_heard(&samples) {
            let (u, at_pause) = match ev {
                Heard::End(u) => (u, false),
                Heard::Pause(u) => {
                    let speech_ms = (u.len() * 1000 / hear::RATE) as u64;
                    if !hear_rate.is_some_and(|r| r * speech_ms / 1000 <= TRY_AT_PAUSE_US) {
                        continue;
                    }
                    (u, true)
                }
            };
            let audio: Vec<f32> = u.iter().map(|&s| f32::from(s) / 32768.0).collect();
            let t = zoo_rt::now_us();
            // Said while she spoke, or so soon after that it may be her.
            let began = t.saturating_sub((u.len() as u64) * 1_000_000 / hear::RATE as u64);
            let over_her =
                muted_since.is_some() || unmuted_at.is_some_and(|at| began < at + TAIL_US);
            // After her name (or her question), what follows is for her:
            // Bella shows she works on it while it is written down
            // (docs/wake-design.md). Other speech says nothing.
            let expected = !over_her && name_until.is_some_and(|(until, _)| t <= until);
            if expected {
                say(chan, "hearing", "");
            }
            let Ok(text) = h.transcribe(&audio) else {
                if expected && !at_pause {
                    say(chan, "lost", "");
                }
                continue;
            };
            let us = zoo_rt::now_us() - t;
            let ms = us / 1000;
            let speech_ms = audio.len() * 1000 / hear::RATE;
            hear_rate = Some(us * 1000 / (speech_ms as u64).max(1));
            if at_pause {
                // Taken now only if it is for her and sounds whole; else
                // the end decides, as before (no words logged either way).
                let for_her = expected || zoo_abi::bella::addressed(&text).is_some();
                if !(for_her && zoo_abi::bella::sounds_finished(&text)) {
                    zoo_rt::log!("BatHandler: not over at a pause ({} ms to hear)", ms);
                    continue;
                }
                det.cut();
                zoo_rt::log!(
                    "BatHandler: over at a {} ms pause, not {} ({} ms to hear)",
                    zoo_infer::listen::PAUSE_FRAMES * 20,
                    zoo_infer::listen::END_FRAMES * 20,
                    ms
                );
            }
            let level = zoo_infer::listen::level_db(&u) as i32;
            if over_her {
                // Her name, a request to her, or "stop": she is cut off
                // (barge-in). Anything else is taken as her own voice:
                // not logged as words, not kept, no window used.
                let to_her = zoo_abi::bella::addressed(&text).is_some()
                    || zoo_abi::bella::name_only(&text)
                    || zoo_abi::bella::stop_talking(&text);
                if to_her {
                    zoo_rt::log!(
                        "BatHandler: heard \"{}\" while she spoke ({} ms of speech at {} dB, {} ms to hear)",
                        text,
                        speech_ms,
                        level,
                        ms
                    );
                    if zoo_abi::bella::name_only(&text) {
                        name_until = Some((zoo_rt::now_us() + AFTER_NAME_US, "after her name"));
                    }
                    held = None;
                    say(chan, "barge:", &text);
                } else {
                    zoo_rt::log!(
                        "BatHandler: speech while she spoke ({} ms at {} dB), taken as her voice",
                        speech_ms,
                        level
                    );
                }
                continue;
            }
            let after_name = name_until
                .take()
                .filter(|(until, _)| t <= *until)
                .map(|(_, why)| why);
            // Anything said to her makes an older sentence stale.
            held = None;
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
                name_until = Some((zoo_rt::now_us() + AFTER_NAME_US, "after her name"));
                say(chan, "name", "");
            } else if after_name == Some(AS_FOLLOW_UP) {
                // Perhaps not for her: no words in the log; Bella decides.
                zoo_rt::log!(
                    "BatHandler: heard speech after her answer ({} ms at {} dB, {} ms to hear)",
                    speech_ms,
                    level,
                    ms
                );
                say(chan, "follow:", &text);
            } else if let Some(why) = after_name {
                zoo_rt::log!(
                    "BatHandler: heard \"{}\" {} ({} ms of speech at {} dB, {} ms to hear)",
                    text,
                    why,
                    speech_ms,
                    level,
                    ms
                );
                say(chan, "heard:", &text);
            } else {
                // Not for Bella: what was said stays unlogged, unless the
                // owner asked to see it.
                if show_heard {
                    zoo_rt::log!(
                        "BatHandler: speech not for Bella ({} ms at {} dB): \"{}\"",
                        speech_ms,
                        level,
                        text
                    );
                } else {
                    zoo_rt::log!(
                        "BatHandler: speech not for Bella ({} ms at {} dB)",
                        speech_ms,
                        level
                    );
                }
                held = Some((text, zoo_rt::now_us(), speech_ms, level));
            }
        }
    }
}
