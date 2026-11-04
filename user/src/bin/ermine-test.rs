//! ErmineTest: the hearing test (docs/hearing-design.md). Transcribes the
//! recordings in the boot image with Moonshine (`models/hear.gguf`) and
//! compares each with the sentence said, from `test/hear.txt` (lines of
//! `<file>|<sentence>`). Exits 0 when every one was heard right, 1 if any
//! was not, 2 when the model or the list is missing or unreadable.

#![no_std]
#![no_main]

extern crate alloc;

use zoo_infer::hear::{self, Hearing};

zoo_rt::entry!(main);

const MODEL: &str = "models/hear.gguf";
const LIST: &str = "test/hear.txt";

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
    let (Some(model), Some(list)) = (file(MODEL), file(LIST)) else {
        let _ = zoo_rt::log("ErmineTest: the boot image has no hearing model or list");
        return 2;
    };
    let t = zoo_rt::now_us();
    let g = match zoo_infer::Gguf::parse(model) {
        Ok(g) => g,
        Err(e) => {
            zoo_rt::log!("ErmineTest: {} is not a GGUF file: {:?}", MODEL, e);
            return 2;
        }
    };
    let h = match Hearing::from_gguf(&g) {
        Ok(h) => h,
        Err(e) => {
            zoo_rt::log!("ErmineTest: {}: {}", MODEL, e.name());
            return 2;
        }
    };
    zoo_rt::log!(
        "ErmineTest: Moonshine ready in {} ms ({} MB, {} threads)",
        (zoo_rt::now_us() - t) / 1000,
        model.len() >> 20,
        zoo_infer::par::width()
    );
    let Ok(list) = core::str::from_utf8(list) else {
        let _ = zoo_rt::log("ErmineTest: the list is not text");
        return 2;
    };
    let (mut right, mut total) = (0, 0);
    let (mut heard_us, mut audio_ms) = (0u64, 0u64);
    for line in list.lines().filter(|l| !l.trim().is_empty()) {
        let Some((name, said)) = line.split_once('|') else {
            continue;
        };
        total += 1;
        let mut path = alloc::string::String::from("test/hear/");
        path.push_str(name);
        let Some(pcm) = file(&path).and_then(hear::wav_samples) else {
            zoo_rt::log!("ErmineTest: {} is missing or not 16 kHz mono", name);
            continue;
        };
        let audio = hear::samples_from_i16(pcm);
        let t = zoo_rt::now_us();
        let text = match h.transcribe(&audio) {
            Ok(text) => text,
            Err(e) => {
                zoo_rt::log!("ErmineTest: {}: {}", name, e.name());
                continue;
            }
        };
        let us = zoo_rt::now_us() - t;
        let ms_audio = (audio.len() * 1000 / hear::RATE) as u64;
        heard_us += us;
        audio_ms += ms_audio;
        let ok = hear::same_words(&text, said);
        if ok {
            right += 1;
        }
        zoo_rt::log!(
            "ErmineTest: {} {} in {} ms ({} ms of audio): \"{}\"",
            if ok { "heard" } else { "MISHEARD" },
            name,
            us / 1000,
            ms_audio,
            text
        );
    }
    zoo_rt::log!(
        "ErmineTest: {} of {} heard right, {} ms for {} ms of audio",
        right,
        total,
        heard_us / 1000,
        audio_ms
    );
    if right == total && total > 0 { 0 } else { 1 }
}
