//! ElephantHandler: runs the language model inside ZooOS
//! (docs/m5b-design.md §5). Maps the model from the initial filesystem,
//! tokenizes a prompt, generates greedily, logs the answer and timings, and
//! compares the generated token ids with the ones the host engine produced
//! for the same prompt (`test/elephant.txt`, written by `cargo xtask`).
//! Exit 0 only if they match exactly.
//!
//! Started with a channel, it is instead the assistant's model service
//! (docs/m5c2-design.md §2): `choose <request>` → progress lines
//! `busy <phase> <done>/<total>`, then `tool <word> <margin> <argument>` or
//! `err <reason>`. `cancel` stops the current request between tokens. It
//! holds no directory handle and never touches files.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::fmt::Write as _;
use zoo_infer::assist::{AssistError, Bella, Step};
use zoo_infer::talk::{Talk, TalkStep};
use zoo_infer::{Gguf, Model, Tokenizer, chat};
use zoo_rt::{Error, Handle, Line, fs_find, fs_map, log, recv, send, ticks, try_recv};

zoo_rt::entry!(main);

const MODEL: &str = "models/qwen2.5-0.5b-instruct-q4_0.gguf";
const EXPECTED: &str = "test/elephant.txt";
const SYSTEM: &str = "You are a helpful assistant.";

/// Longest a request (including first-time model set-up) may run, in ticks.
const DEADLINE_TICKS: u64 = 60_000;

fn main(start_handle: u64) -> u8 {
    if start_handle != zoo_rt::NO_HANDLE {
        return service(start_handle);
    }
    let Some(file) = fs_find(MODEL).and_then(|i| fs_map(i).ok()) else {
        let _ = log("ElephantHandler: model not in the initial filesystem");
        return 2;
    };
    // `test/elephant.txt`: line 1 the user prompt, line 2 expected ids.
    let Some(spec) = fs_find(EXPECTED).and_then(|i| fs_map(i).ok()) else {
        return 3;
    };
    let Ok(spec) = core::str::from_utf8(spec) else {
        return 3;
    };
    let mut lines = spec.lines();
    let (Some(prompt), Some(expected)) = (lines.next(), lines.next()) else {
        return 3;
    };
    let expected: Vec<u32> = expected
        .split(',')
        .filter_map(|x| x.trim().parse().ok())
        .collect();

    let t0 = ticks();
    let g = match Gguf::parse(file) {
        Ok(g) => g,
        Err(e) => {
            zoo_rt::log!("ElephantHandler: GGUF error {:?}", e);
            return 4;
        }
    };
    let (tok, model) = match (Tokenizer::from_gguf(&g), Model::from_gguf(&g)) {
        (Ok(t), Ok(m)) => (t, m),
        _ => return 5,
    };
    let t1 = ticks();
    zoo_rt::log!(
        "ElephantHandler: loaded {} MB model, {} tokens, {} layers in {} ticks",
        file.len() >> 20,
        tok.vocab_size(),
        model.cfg.n_layer,
        t1 - t0
    );
    let ids = tok.encode(&chat::prompt(SYSTEM, prompt));
    let mut st = model.new_state(ids.len() + expected.len() + 2);
    // Only the last prompt token's scores are read.
    for (k, &id) in ids.iter().enumerate() {
        let r = if k + 1 == ids.len() {
            model.forward(&mut st, id)
        } else {
            model.feed(&mut st, id)
        };
        if r.is_err() {
            return 6;
        }
    }
    let t2 = ticks();
    let out = match chat::generate(&model, &tok, &mut st, &[], expected.len() + 1, |_| true) {
        Ok(o) => o,
        Err(_) => return 6,
    };
    let t3 = ticks();
    zoo_rt::log!("ElephantHandler: prompt `{}`", prompt);
    zoo_rt::log!("ElephantHandler: answer `{}`", tok.decode(&out));
    zoo_rt::log!(
        "ElephantHandler: prefill {} tokens in {} ticks, generated {} tokens in {} ticks (100 ticks = 1 s)",
        ids.len(),
        t2 - t1,
        out.len(),
        t3 - t2
    );
    if out != expected {
        zoo_rt::log!("ElephantHandler: tokens differ from the host engine's");
        return 1;
    }
    let _ = log("ElephantHandler: tokens match the host engine exactly");
    0
}

/// Sends a progress or result line; progress may be dropped when the
/// client's queue is full, results wait.
fn reply(chan: Handle, args: core::fmt::Arguments, progress: bool) -> Result<(), Error> {
    let mut l = Line::new();
    let _ = l.write_fmt(args);
    if progress {
        match send(chan, l.as_str().as_bytes()) {
            Err(Error::Full) => Ok(()),
            other => other,
        }
    } else {
        zoo_rt::app::send_wait(chan, l.as_str().as_bytes())
    }
}

/// What the progress callback saw on the channel.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    Cancelled,
    TimedOut,
    Closed,
}

/// The model's bytes: mapped from the initial filesystem when the boot
/// image has it (the USB stick, the test images), else read whole from
/// the read-only model disk into page-aligned memory (docs/stage2-design.md
/// §3). Read as a background program, the read does not hold up the
/// desktop.
fn model_bytes() -> Option<&'static [u8]> {
    if let Some(f) = fs_find(MODEL).and_then(|i| fs_map(i).ok()) {
        return Some(f);
    }
    let len = usize::try_from(zoo_rt::model_size().ok()?).ok()?;
    let size = len.div_ceil(zoo_abi::BLOCK_BYTES) * zoo_abi::BLOCK_BYTES;
    let layout = core::alloc::Layout::from_size_align(size, 4096).ok()?;
    // SAFETY: `layout` has a non-zero size (`len` > 0, checked by the
    // kernel when it found the disk).
    let p = unsafe { alloc::alloc::alloc(layout) };
    if p.is_null() {
        let _ = log("ElephantHandler: no memory for the model");
        return None;
    }
    // SAFETY: `p` is a fresh allocation of `size` bytes, owned here for
    // the rest of the process (never freed).
    let buf: &'static mut [u8] = unsafe { core::slice::from_raw_parts_mut(p, size) };
    let t = zoo_rt::now_us();
    let run = zoo_abi::BLOCK_RUN_MAX * zoo_abi::BLOCK_BYTES;
    for (k, chunk) in buf.chunks_mut(run).enumerate() {
        let block = (k * zoo_abi::BLOCK_RUN_MAX) as u64;
        if zoo_rt::model_read(block, chunk).is_err() {
            let _ = log("ElephantHandler: reading the model disk failed");
            return None;
        }
    }
    zoo_rt::log!(
        "ElephantHandler: read the model from the model disk: {} MB in {} ms",
        len >> 20,
        (zoo_rt::now_us() - t) / 1000
    );
    Some(&buf[..len])
}

fn service(chan: Handle) -> u8 {
    // Inference yields to the desktop (docs/stage2-design.md, B12).
    let _ = zoo_rt::set_background(true);
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    let Some(file) = model_bytes() else {
        let _ = log("ElephantHandler: service: model not installed");
        // Answer every request with the reason until the client leaves.
        loop {
            match recv(chan, &mut buf) {
                // `warm` and `note` expect no answer.
                Ok(n) if &buf[..n] == b"warm" || buf[..n].starts_with(b"note ") => {}
                Ok(_) => {
                    if reply(chan, format_args!("err model not installed"), false).is_err() {
                        return 0;
                    }
                }
                Err(_) => return 0,
            }
        }
    };
    let parse = zoo_rt::now_us();
    let Ok(g) = Gguf::parse(file) else { return 4 };
    let (Ok(tok), Ok(model)) = (Tokenizer::from_gguf(&g), Model::from_gguf(&g)) else {
        return 5;
    };
    zoo_rt::log!(
        "ElephantHandler: service ready (model mapped, parsed in {} ms, engine {})",
        (zoo_rt::now_us() - parse) / 1000,
        zoo_infer::cpu::backend()
    );
    let mut assistant: Option<Bella> = None;
    // Replies in words, with their own system prompt (docs/talk-design.md).
    let mut talk: Option<Talk> = None;
    // Bella asked for a warm-up: the reply prompt is warmed whenever no
    // request is waiting, and a request interrupts it (it resumes after).
    let mut warm_talk = false;
    let mut talk_tries = 0;
    // A request that arrived during the warm-up, answered after it.
    let mut pending: Option<alloc::string::String> = None;
    loop {
        // Resume the reply prompt's warm-up once no request is waiting
        // (a few tries: it only fails on a model error).
        if pending.is_none() && warm_talk && talk.is_none() && assistant.is_some() && talk_tries < 3
        {
            talk_tries += 1;
            pending = Some(alloc::string::String::from("warm"));
        }
        let msg = match pending.take() {
            Some(m) => m,
            None => {
                let n = match recv(chan, &mut buf) {
                    Ok(n) => n,
                    Err(_) => return 0,
                };
                let Ok(m) = core::str::from_utf8(&buf[..n]) else {
                    continue;
                };
                alloc::string::String::from(m)
            }
        };
        let msg = msg.as_str();
        if msg == "warm" {
            warm_talk = true;
            // Read the system prompt now, in the background, so the first
            // request is answered as fast as later ones (docs/stage2-design.md).
            // A request arriving meanwhile waits in `pending`; a `cancel`
            // drops it.
            if assistant.is_none() {
                let t = zoo_rt::now_us();
                let mut closed = false;
                let made = Bella::new(&model, &tok, |step| {
                    // A request is waiting: show it the set-up's progress.
                    if let (Some(_), Step::Prefix { done, total }) = (&pending, step) {
                        let _ = reply(chan, format_args!("busy setup {}/{}", done, total), true);
                    }
                    let mut m = [0u8; zoo_abi::MAX_MESSAGE];
                    match try_recv(chan, &mut m) {
                        Ok(k) => {
                            let s = core::str::from_utf8(&m[..k]).unwrap_or("");
                            if s.starts_with("choose ") || s.starts_with("talk ") {
                                pending = Some(alloc::string::String::from(s));
                                // Tell it at once that the set-up is under way.
                                if let Step::Prefix { done, total } = step {
                                    let _ = reply(
                                        chan,
                                        format_args!("busy setup {}/{}", done, total),
                                        true,
                                    );
                                }
                            } else if s == "cancel" && pending.take().is_some() {
                                let _ = log("ElephantHandler: request cancelled");
                                let _ = reply(chan, format_args!("err cancelled"), false);
                            }
                        }
                        Err(Error::Again) => {}
                        Err(_) => closed = true,
                    }
                    !closed
                });
                if closed {
                    return 0;
                }
                if let Ok(a) = made {
                    zoo_rt::log!(
                        "ElephantHandler: warmed up: system prompt {} tokens in {} ms",
                        a.prefix_tokens(),
                        (zoo_rt::now_us() - t) / 1000
                    );
                    assistant = Some(a);
                }
            }
            // Then the prompt for replies in words, unless a request is
            // already waiting: it is answered first.
            if talk.is_none() && pending.is_none() {
                let t = zoo_rt::now_us();
                let mut closed = false;
                let made = Talk::new(&model, &tok, |_| {
                    let mut m = [0u8; zoo_abi::MAX_MESSAGE];
                    match try_recv(chan, &mut m) {
                        Ok(k) => {
                            let s = core::str::from_utf8(&m[..k]).unwrap_or("");
                            if s.starts_with("choose ") || s.starts_with("talk ") {
                                pending = Some(alloc::string::String::from(s));
                            } else if s == "cancel" && pending.take().is_some() {
                                let _ = log("ElephantHandler: request cancelled");
                                let _ = reply(chan, format_args!("err cancelled"), false);
                            }
                        }
                        Err(Error::Again) => {}
                        Err(_) => closed = true,
                    }
                    !closed && pending.is_none()
                });
                if closed {
                    return 0;
                }
                if let Ok(tk) = made {
                    zoo_rt::log!(
                        "ElephantHandler: warmed up: reply prompt {} tokens in {} ms",
                        tk.prefix_tokens(),
                        (zoo_rt::now_us() - t) / 1000
                    );
                    talk = Some(tk);
                }
            }
            continue;
        }
        if let Some(request) = msg.strip_prefix("talk ") {
            // `talk <request>\t<facts>`: facts the user told Bella before.
            let (request, facts) = request.split_once('\t').unwrap_or((request, ""));
            // Bella answers in words: text only, never run
            // (docs/talk-design.md).
            let start = ticks();
            let t0 = zoo_rt::now_us();
            let mut stop = None;
            let mut keep = |step: TalkStep| -> bool {
                let mut m = [0u8; 16];
                match try_recv(chan, &mut m) {
                    Ok(k) if &m[..k] == b"cancel" => stop = Some(Stop::Cancelled),
                    Ok(_) | Err(Error::Again) => {}
                    Err(_) => stop = Some(Stop::Closed),
                }
                if stop.is_none() && ticks() - start > DEADLINE_TICKS {
                    stop = Some(Stop::TimedOut);
                }
                if stop.is_some() {
                    return false;
                }
                let _ = match step {
                    TalkStep::Prefix { done, total } => {
                        reply(chan, format_args!("busy setup {}/{}", done, total), true)
                    }
                    TalkStep::Request { done, total } => {
                        reply(chan, format_args!("busy reading {}/{}", done, total), true)
                    }
                    TalkStep::Reply { done } => {
                        reply(chan, format_args!("busy writing {}", done), true)
                    }
                };
                true
            };
            if talk.is_none()
                && let Ok(tk) = Talk::new(&model, &tok, &mut keep)
            {
                talk = Some(tk);
            }
            let result = match talk.as_mut() {
                Some(tk) => tk.reply_with(request, facts, &mut keep),
                None => Err(AssistError::Cancelled),
            };
            let sent = match (result, stop) {
                (_, Some(Stop::Closed)) => return 0,
                (_, Some(Stop::Cancelled)) => {
                    let _ = log("ElephantHandler: request cancelled");
                    reply(chan, format_args!("err cancelled"), false)
                }
                (_, Some(Stop::TimedOut)) => reply(chan, format_args!("err timed out"), false),
                (Ok(text), None) => {
                    zoo_rt::log!(
                        "ElephantHandler: said `{}` in {} ms",
                        text,
                        (zoo_rt::now_us() - t0) / 1000
                    );
                    reply(chan, format_args!("say {}", text), false)
                }
                (Err(_), None) => reply(chan, format_args!("err model error"), false),
            };
            if sent.is_err() {
                return 0;
            }
            continue;
        }
        if let Some(note) = msg.strip_prefix("note ") {
            // An exchange that was not a reply in words, for the
            // conversation (docs/talk-design.md §6). Fed in the background;
            // a request arriving meanwhile is kept and answered next.
            if let (Some(tk), Some((user, said))) = (talk.as_mut(), note.split_once('\t')) {
                let t = zoo_rt::now_us();
                let mut closed = false;
                let fed = tk.note(user, said, |_| {
                    let mut m = [0u8; zoo_abi::MAX_MESSAGE];
                    match try_recv(chan, &mut m) {
                        Ok(k) => {
                            let s = core::str::from_utf8(&m[..k]).unwrap_or("");
                            if s.starts_with("choose ") || s.starts_with("talk ") {
                                pending = Some(alloc::string::String::from(s));
                            }
                        }
                        Err(Error::Again) => {}
                        Err(_) => closed = true,
                    }
                    !closed
                });
                if closed {
                    return 0;
                }
                if fed.is_ok() {
                    zoo_rt::log!(
                        "ElephantHandler: noted an exchange in {} ms ({} tokens of conversation)",
                        (zoo_rt::now_us() - t) / 1000,
                        tk.conversation_tokens()
                    );
                }
            }
            continue;
        }
        let Some(request) = msg.strip_prefix("choose ") else {
            // `cancel` with nothing running, or unknown: nothing to do.
            continue;
        };
        // `choose <request>\t<context>`: a follow-up, with the last exchange.
        let (request, context) = request.split_once('\t').unwrap_or((request, ""));
        let start = ticks();
        let t0 = zoo_rt::now_us();
        let mut setup_us = 0;
        let mut first_arg: Option<u64> = None;
        let mut stop = None;
        // Checked between tokens: a `cancel` message, the client leaving,
        // or the deadline end the request.
        let keep_going = |chan: Handle, step: Step, stop: &mut Option<Stop>| -> bool {
            let mut m = [0u8; 16];
            match try_recv(chan, &mut m) {
                Ok(k) if &m[..k] == b"cancel" => *stop = Some(Stop::Cancelled),
                Ok(_) | Err(Error::Again) => {}
                Err(_) => *stop = Some(Stop::Closed),
            }
            if stop.is_none() && ticks() - start > DEADLINE_TICKS {
                *stop = Some(Stop::TimedOut);
            }
            if stop.is_some() {
                return false;
            }
            let _ = match step {
                Step::Prefix { done, total } => {
                    reply(chan, format_args!("busy setup {}/{}", done, total), true)
                }
                Step::Request { done, total } => {
                    reply(chan, format_args!("busy reading {}/{}", done, total), true)
                }
                Step::Arg { done } => reply(chan, format_args!("busy writing {}", done), true),
            };
            true
        };
        let mut setup_err = None;
        if assistant.is_none() {
            let t = ticks();
            let tu = zoo_rt::now_us();
            let made = Bella::new(&model, &tok, |s| keep_going(chan, s, &mut stop));
            setup_us = zoo_rt::now_us() - tu;
            match made {
                Ok(a) => {
                    zoo_rt::log!(
                        "ElephantHandler: system prompt {} tokens in {} ticks",
                        a.prefix_tokens(),
                        ticks() - t
                    );
                    assistant = Some(a);
                }
                Err(e) => setup_err = Some(e),
            }
        }
        let result = match (assistant.as_mut(), setup_err) {
            (Some(a), _) => a.choose_in(request, context, |s| {
                if matches!(s, Step::Arg { .. }) && first_arg.is_none() {
                    first_arg = Some(zoo_rt::now_us());
                }
                keep_going(chan, s, &mut stop)
            }),
            (None, e) => Err(e.unwrap_or(AssistError::Cancelled)),
        };
        let elapsed = ticks() - start;
        let sent = match (result, stop) {
            (_, Some(Stop::Closed)) => return 0,
            (_, Some(Stop::Cancelled)) => {
                zoo_rt::log!(
                    "ElephantHandler: cancel seen at tick {}, {} ticks into the request",
                    ticks(),
                    elapsed
                );
                let _ = log("ElephantHandler: request cancelled");
                reply(chan, format_args!("err cancelled"), false)
            }
            (_, Some(Stop::TimedOut)) => {
                zoo_rt::log!("ElephantHandler: timed out after {} ticks", elapsed);
                reply(chan, format_args!("err timed out"), false)
            }
            (Ok(c), None) => {
                zoo_rt::log!(
                    "ElephantHandler: `{}`: margin {}, {} request + {} argument tokens, {} ticks",
                    request,
                    (c.margin * 100.0) as i32,
                    c.request_tokens,
                    c.arg_tokens,
                    elapsed
                );
                zoo_rt::log!("ElephantHandler: chose {} `{}`", c.tool.word(), c.arg);
                // One timing line per request (docs/stage1-design.md): set-up
                // (the system prompt, first request only), reading the
                // request, writing the argument, and the heap.
                let end = zoo_rt::now_us();
                let read_end = first_arg.unwrap_or(end);
                let (used, peak, _) = zoo_rt::heap_stats();
                zoo_rt::log!(
                    "ElephantHandler: timing: setup {} ms, reading {} ms ({} tokens), writing {} ms ({} tokens), total {} ms; heap {} KiB, peak {} KiB",
                    setup_us / 1000,
                    (read_end - t0 - setup_us) / 1000,
                    c.request_tokens,
                    (end - read_end) / 1000,
                    c.arg_tokens,
                    (end - t0) / 1000,
                    used / 1024,
                    peak / 1024
                );
                reply(
                    chan,
                    format_args!(
                        "tool {} {} {}",
                        c.tool.word(),
                        (c.margin * 100.0) as i32,
                        c.arg
                    ),
                    false,
                )
            }
            (Err(e), None) => {
                let why = match e {
                    AssistError::TooLong => "request too long",
                    AssistError::Empty => "empty request",
                    AssistError::Cancelled => "cancelled",
                    AssistError::Model(_) => "model error",
                };
                reply(chan, format_args!("err {}", why), false)
            }
        };
        if sent.is_err() {
            return 0;
        }
    }
}
