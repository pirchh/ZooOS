//! MynahHandler: Bella's voice (docs/speaking-design.md). Started by Bella
//! with a channel when a voice model is installed. For each `say:<text>`
//! it makes the words into phonemes (misaki's word lists, in the model
//! file), the phonemes into 24 kHz speech with Kokoro, and plays it at
//! 48 kHz in stereo through `AUDIO_WRITE`. `stop` cuts it off (barge-in,
//! docs/conversation-design.md) and drops what was queued; `quiet` goes
//! back when nothing is left to say.
//! `none:<why>` and an exit when there is no voice model or no sound
//! device. It ends when Bella's channel closes.
//!
//! Her words are hers: they are already in her window and her log, so
//! they are logged here too.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use zoo_infer::g2p::Lexicon;
use zoo_infer::speak::{self, Speech};
use zoo_rt::{Error, Handle, close, recv, try_recv};

zoo_rt::entry!(main);

fn send(chan: Handle, prefix: &str, text: &str) {
    let mut m = Vec::with_capacity(prefix.len() + text.len());
    m.extend_from_slice(prefix.as_bytes());
    m.extend_from_slice(text.as_bytes());
    m.truncate(zoo_abi::MAX_MESSAGE);
    let _ = zoo_rt::send(chan, &m);
}

/// The voice model's bytes: from the boot image when it has them, else
/// read whole from the model disk into page-aligned memory.
fn model_bytes() -> Option<&'static [u8]> {
    if let Some(f) = zoo_rt::fs_find(zoo_abi::SPEAK_MODEL_FILE).and_then(|i| zoo_rt::fs_map(i).ok())
    {
        return Some(f);
    }
    let len = usize::try_from(zoo_rt::model_size_of(zoo_abi::MODEL_SPEAK).ok()?).ok()?;
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
        zoo_rt::model_read_of(zoo_abi::MODEL_SPEAK, block, chunk).ok()?;
    }
    zoo_rt::log!(
        "MynahHandler: read the voice from the model disk: {} MB in {} ms",
        len >> 20,
        (zoo_rt::now_us() - t) / 1000
    );
    Some(&buf[..len])
}

/// 24 kHz mono to the device's 48 kHz 16-bit stereo: each sample, then
/// the point halfway to the next, on both channels.
fn to_device(audio: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(audio.len() * 8);
    let pcm = |v: f32| ((v * 32767.0).clamp(-32768.0, 32767.0) as i16).to_le_bytes();
    for (i, &a) in audio.iter().enumerate() {
        let b = audio.get(i + 1).copied().unwrap_or(a);
        for v in [a, (a + b) * 0.5] {
            let s = pcm(v);
            out.extend_from_slice(&s);
            out.extend_from_slice(&s);
        }
    }
    out
}

/// What Bella sent while a sentence was made or played.
enum Heard {
    /// Nothing that stops it.
    Nothing,
    /// `stop`: she was interrupted (docs/conversation-design.md).
    Stop,
    /// Her channel closed.
    Gone,
}

/// Takes Bella's messages without waiting: sentences join the queue;
/// `stop` empties it.
fn check(chan: Handle, queue: &mut VecDeque<String>) -> Heard {
    let mut msg = [0u8; zoo_abi::MAX_MESSAGE];
    loop {
        match try_recv(chan, &mut msg) {
            Ok(n) => match core::str::from_utf8(&msg[..n]) {
                Ok("stop") => {
                    queue.clear();
                    return Heard::Stop;
                }
                Ok(m) => {
                    if let Some(t) = m.strip_prefix("say:") {
                        queue.push_back(String::from(t));
                    }
                }
                Err(_) => {}
            },
            Err(Error::Again) => return Heard::Nothing,
            Err(_) => return Heard::Gone,
        }
    }
}

/// Plays `bytes` to the end, or until Bella says `stop` (`Ok(Some(at))`,
/// the bytes played by then); `Err` when the device or Bella is gone.
fn play(chan: Handle, bytes: &[u8], queue: &mut VecDeque<String>) -> Result<Option<usize>, Error> {
    let mut at = 0;
    while at < bytes.len() {
        match check(chan, queue) {
            Heard::Nothing => {}
            Heard::Stop => return Ok(Some(at)),
            Heard::Gone => return Err(Error::NotFound),
        }
        let n = zoo_rt::audio_write(&bytes[at..])?;
        if n == 0 {
            // Every buffer is queued (200 ms ahead): wait a tick.
            zoo_rt::sleep(1);
        }
        at += n;
    }
    Ok(None)
}

fn main(chan: u64) -> u8 {
    if chan == zoo_rt::NO_HANDLE {
        return 2;
    }
    let Some(model) = model_bytes() else {
        send(chan, "none:", "no voice model");
        return 0;
    };
    let g = match zoo_infer::Gguf::parse(model) {
        Ok(g) => g,
        Err(_) => {
            send(chan, "none:", "the voice model is not a GGUF file");
            return 0;
        }
    };
    let voice = match Speech::from_gguf(&g) {
        Ok(s) => s,
        Err(e) => {
            send(chan, "none:", e.name());
            return 0;
        }
    };
    let Ok(words) = g.str("kokoro.lexicon") else {
        send(chan, "none:", "the voice model has no word list");
        return 0;
    };
    let lexicon = Lexicon::new(words);
    let cpus = zoo_rt::cpu_count();
    if cpus > 1
        && let Some(pool) = zoo_rt::pool::Workers::start(cpus - 1)
    {
        zoo_infer::par::set_pool(pool);
    }
    zoo_rt::log!(
        "MynahHandler: ready ({} MB voice, {} words, {} threads)",
        model.len() >> 20,
        lexicon.len(),
        zoo_infer::par::width()
    );
    send(chan, "ready", "");
    let mut msg = [0u8; zoo_abi::MAX_MESSAGE];
    let mut queue: VecDeque<String> = VecDeque::new();
    loop {
        let Some(text) = queue.pop_front() else {
            let n = match recv(chan, &mut msg) {
                Ok(n) => n,
                Err(_) => {
                    let _ = close(chan);
                    return 0;
                }
            };
            if let Some(t) = core::str::from_utf8(&msg[..n])
                .ok()
                .and_then(|m| m.strip_prefix("say:"))
            {
                queue.push_back(String::from(t));
            }
            continue;
        };
        let t0 = zoo_rt::now_us();
        let phonemes = lexicon.phonemes(&text);
        let made = voice.synthesize(&phonemes, 1.0);
        // A stop that came while it was made: not played at all.
        let stopped_early = match check(chan, &mut queue) {
            Heard::Nothing => false,
            Heard::Stop => true,
            Heard::Gone => {
                let _ = close(chan);
                return 0;
            }
        };
        let (audio, _) = match made {
            Ok(a) => a,
            Err(e) => {
                zoo_rt::log!("MynahHandler: cannot say \"{}\": {}", text, e.name());
                if queue.is_empty() {
                    send(chan, "quiet", "");
                }
                continue;
            }
        };
        if stopped_early {
            zoo_rt::log!("MynahHandler: stopped before saying \"{}\"", text);
            send(chan, "quiet", "");
            continue;
        }
        let made_ms = (zoo_rt::now_us() - t0) / 1000;
        let audio_ms = audio.len() * 1000 / speak::RATE;
        zoo_rt::log!(
            "MynahHandler: saying \"{}\" ({} ms of speech, made in {} ms)",
            text,
            audio_ms,
            made_ms
        );
        let bytes = to_device(&audio);
        match play(chan, &bytes, &mut queue) {
            Ok(None) => {
                zoo_rt::log!(
                    "MynahHandler: said it, {} ms after it was asked",
                    (zoo_rt::now_us() - t0) / 1000
                );
            }
            Ok(Some(at)) => {
                // 48 kHz, 4 bytes a frame.
                zoo_rt::log!(
                    "MynahHandler: stopped, {} of {} ms said",
                    at / 4 * 1000 / 48_000,
                    audio_ms
                );
            }
            Err(Error::NotFound) => {
                let _ = close(chan);
                return 0;
            }
            Err(e) => {
                send(chan, "none:", e.name());
                return 0;
            }
        }
        if queue.is_empty() {
            send(chan, "quiet", "");
        }
    }
}
