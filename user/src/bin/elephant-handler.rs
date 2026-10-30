//! ElephantHandler: runs the language model inside ZooOS
//! (docs/m5b-design.md §5). Maps the model from the initial filesystem,
//! tokenizes a prompt, generates greedily, logs the answer and timings, and
//! compares the generated token ids with the ones the host engine produced
//! for the same prompt (`test/elephant.txt`, written by `cargo xtask`).
//! Exit 0 only if they match exactly.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use zoo_infer::{Gguf, Model, Tokenizer, chat};
use zoo_rt::{fs_find, fs_map, log, ticks};

zoo_rt::entry!(main);

const MODEL: &str = "models/qwen2.5-0.5b-instruct-q4_0.gguf";
const EXPECTED: &str = "test/elephant.txt";
const SYSTEM: &str = "You are a helpful assistant.";

fn main(_start_handle: u64) -> u8 {
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
    for &id in &ids {
        if model.forward(&mut st, id).is_err() {
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
