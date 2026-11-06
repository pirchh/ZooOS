//! Hearing (docs/hearing-design.md): `cargo xtask hear-gguf` converts the
//! downloaded Moonshine Tiny (safetensors, tools/models/voice/) to one GGUF
//! file the engine reads, and `cargo xtask hear-check` compares the engine
//! with the NumPy reference on recordings made with the host's voice.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A JSON value: just enough for safetensors headers, `config.json` and
/// `tokenizer.json`.
#[derive(Debug, Clone)]
pub enum Json {
    Null,
    // Read from `true`/`false`; no caller needs the value yet.
    #[allow(dead_code)]
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn num(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

pub fn parse_json(s: &str) -> Result<Json, String> {
    let b = s.as_bytes();
    let mut at = 0;
    let v = value(b, &mut at)?;
    skip_ws(b, &mut at);
    if at != b.len() {
        return Err(format!("JSON: trailing data at byte {at}"));
    }
    Ok(v)
}

fn skip_ws(b: &[u8], at: &mut usize) {
    while *at < b.len() && b[*at].is_ascii_whitespace() {
        *at += 1;
    }
}

fn value(b: &[u8], at: &mut usize) -> Result<Json, String> {
    skip_ws(b, at);
    let err = |at: usize| format!("JSON: unexpected input at byte {at}");
    match b.get(*at) {
        Some(b'{') => {
            *at += 1;
            let mut kv = Vec::new();
            skip_ws(b, at);
            if b.get(*at) == Some(&b'}') {
                *at += 1;
                return Ok(Json::Obj(kv));
            }
            loop {
                skip_ws(b, at);
                let Json::Str(k) = value(b, at)? else {
                    return Err(err(*at));
                };
                skip_ws(b, at);
                if b.get(*at) != Some(&b':') {
                    return Err(err(*at));
                }
                *at += 1;
                kv.push((k, value(b, at)?));
                skip_ws(b, at);
                match b.get(*at) {
                    Some(b',') => *at += 1,
                    Some(b'}') => {
                        *at += 1;
                        return Ok(Json::Obj(kv));
                    }
                    _ => return Err(err(*at)),
                }
            }
        }
        Some(b'[') => {
            *at += 1;
            let mut a = Vec::new();
            skip_ws(b, at);
            if b.get(*at) == Some(&b']') {
                *at += 1;
                return Ok(Json::Arr(a));
            }
            loop {
                a.push(value(b, at)?);
                skip_ws(b, at);
                match b.get(*at) {
                    Some(b',') => *at += 1,
                    Some(b']') => {
                        *at += 1;
                        return Ok(Json::Arr(a));
                    }
                    _ => return Err(err(*at)),
                }
            }
        }
        Some(b'"') => {
            *at += 1;
            let mut s = String::new();
            loop {
                match b.get(*at) {
                    None => return Err("JSON: unterminated string".into()),
                    Some(b'"') => {
                        *at += 1;
                        return Ok(Json::Str(s));
                    }
                    Some(b'\\') => {
                        let c = *b.get(*at + 1).ok_or("JSON: bad escape")?;
                        *at += 2;
                        match c {
                            b'"' => s.push('"'),
                            b'\\' => s.push('\\'),
                            b'/' => s.push('/'),
                            b'b' => s.push('\u{8}'),
                            b'f' => s.push('\u{c}'),
                            b'n' => s.push('\n'),
                            b'r' => s.push('\r'),
                            b't' => s.push('\t'),
                            b'u' => {
                                let hex = |at: usize| -> Result<u32, String> {
                                    let h = b.get(at..at + 4).ok_or("JSON: bad \\u")?;
                                    u32::from_str_radix(std::str::from_utf8(h).unwrap_or("x"), 16)
                                        .map_err(|_| "JSON: bad \\u".to_string())
                                };
                                let mut c = hex(*at)?;
                                *at += 4;
                                if (0xD800..0xDC00).contains(&c)
                                    && b.get(*at..*at + 2) == Some(b"\\u")
                                {
                                    let lo = hex(*at + 2)?;
                                    *at += 6;
                                    c = 0x10000 + ((c - 0xD800) << 10) + (lo - 0xDC00);
                                }
                                s.push(char::from_u32(c).unwrap_or('\u{fffd}'));
                            }
                            _ => return Err("JSON: bad escape".into()),
                        }
                    }
                    Some(_) => {
                        // A whole UTF-8 sequence at once.
                        let start = *at;
                        *at += 1;
                        while *at < b.len() && (b[*at] & 0xC0) == 0x80 {
                            *at += 1;
                        }
                        s.push_str(
                            std::str::from_utf8(&b[start..*at]).map_err(|_| "JSON: bad UTF-8")?,
                        );
                    }
                }
            }
        }
        Some(b't') if b[*at..].starts_with(b"true") => {
            *at += 4;
            Ok(Json::Bool(true))
        }
        Some(b'f') if b[*at..].starts_with(b"false") => {
            *at += 5;
            Ok(Json::Bool(false))
        }
        Some(b'n') if b[*at..].starts_with(b"null") => {
            *at += 4;
            Ok(Json::Null)
        }
        Some(c) if *c == b'-' || c.is_ascii_digit() => {
            let start = *at;
            while *at < b.len() && matches!(b[*at], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
            {
                *at += 1;
            }
            let t = std::str::from_utf8(&b[start..*at]).unwrap_or("");
            t.parse().map(Json::Num).map_err(|_| err(start))
        }
        _ => Err(err(*at)),
    }
}

/// The voice model folder and the converted file.
pub fn moonshine_dir(root: &Path) -> PathBuf {
    root.join("tools/models/voice/moonshine-tiny")
}

pub fn gguf_path(root: &Path) -> PathBuf {
    root.join("tools/models/voice/moonshine-tiny-f32.gguf")
}

fn read(p: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(p).map_err(|e| {
        format!(
            "cannot read {}: {e} (download it: tools/models/README.md, \"Voice\")",
            p.display()
        )
    })
}

/// GGUF metadata value types.
const GGUF_U32: u32 = 4;
const GGUF_F32: u32 = 6;
const GGUF_STRING: u32 = 8;
const GGUF_ARRAY: u32 = 9;
const ALIGN: usize = 32;

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u64).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// `cargo xtask hear-gguf`: Moonshine's safetensors, config and token list
/// as one GGUF file of F32 tensors. Two-dimensional weights keep their
/// layout (a row per output); a convolution `[out, in, k]` becomes the
/// matrix `[out, in * k]`, the layout of the engine's im2col products.
pub fn convert(root: &Path) -> Result<PathBuf, String> {
    let dir = moonshine_dir(root);
    let st = read(&dir.join("model.safetensors"))?;
    let n = u64::from_le_bytes(
        st.get(..8)
            .ok_or("short safetensors file")?
            .try_into()
            .unwrap(),
    ) as usize;
    let header = parse_json(
        std::str::from_utf8(st.get(8..8 + n).ok_or("short safetensors header")?)
            .map_err(|_| "safetensors header is not UTF-8")?,
    )?;
    let data = &st[8 + n..];
    let cfg = parse_json(&String::from_utf8_lossy(&read(&dir.join("config.json"))?))?;
    let tok = parse_json(&String::from_utf8_lossy(&read(
        &dir.join("tokenizer.json"),
    )?))?;

    // Token strings by id.
    let mut vocab: BTreeMap<usize, String> = BTreeMap::new();
    if let Some(Json::Obj(kv)) = tok.get("model").and_then(|m| m.get("vocab")) {
        for (s, id) in kv {
            vocab.insert(id.num().ok_or("bad vocab id")? as usize, s.clone());
        }
    }
    if let Some(Json::Arr(added)) = tok.get("added_tokens") {
        for a in added {
            let id = a.get("id").and_then(Json::num).ok_or("bad added token")? as usize;
            let s = a
                .get("content")
                .and_then(Json::str)
                .ok_or("bad added token")?;
            vocab.insert(id, s.to_string());
        }
    }
    let vocab_size = cfg
        .get("vocab_size")
        .and_then(Json::num)
        .ok_or("no vocab_size")? as usize;
    let tokens: Vec<String> = (0..vocab_size)
        .map(|i| vocab.get(&i).cloned().unwrap_or_default())
        .collect();

    // Metadata.
    let u32_keys = [
        "hidden_size",
        "intermediate_size",
        "encoder_num_hidden_layers",
        "decoder_num_hidden_layers",
        "encoder_num_attention_heads",
        "decoder_num_attention_heads",
        "vocab_size",
        "decoder_start_token_id",
        "eos_token_id",
        "max_position_embeddings",
    ];
    let f32_keys = ["partial_rotary_factor", "rope_theta"];
    let mut kv = Vec::new();
    let mut n_kv = 0u64;
    put_str(&mut kv, "general.architecture");
    kv.extend_from_slice(&GGUF_STRING.to_le_bytes());
    put_str(&mut kv, "moonshine");
    n_kv += 1;
    for k in u32_keys {
        let v = cfg
            .get(k)
            .and_then(Json::num)
            .ok_or(format!("config.json has no {k}"))?;
        put_str(&mut kv, &format!("moonshine.{k}"));
        kv.extend_from_slice(&GGUF_U32.to_le_bytes());
        kv.extend_from_slice(&(v as u32).to_le_bytes());
        n_kv += 1;
    }
    for k in f32_keys {
        let v = cfg
            .get(k)
            .and_then(Json::num)
            .ok_or(format!("config.json has no {k}"))?;
        put_str(&mut kv, &format!("moonshine.{k}"));
        kv.extend_from_slice(&GGUF_F32.to_le_bytes());
        kv.extend_from_slice(&(v as f32).to_le_bytes());
        n_kv += 1;
    }
    put_str(&mut kv, "tokenizer.ggml.tokens");
    kv.extend_from_slice(&GGUF_ARRAY.to_le_bytes());
    kv.extend_from_slice(&GGUF_STRING.to_le_bytes());
    kv.extend_from_slice(&(tokens.len() as u64).to_le_bytes());
    for t in &tokens {
        put_str(&mut kv, t);
    }
    n_kv += 1;

    // Tensors, by name, without the "model." prefix.
    let Json::Obj(entries) = &header else {
        return Err("safetensors header is not an object".into());
    };
    let mut infos = Vec::new();
    let mut blobs: Vec<&[u8]> = Vec::new();
    let mut offset = 0usize;
    for (name, v) in entries {
        if name == "__metadata__" {
            continue;
        }
        if v.get("dtype").and_then(Json::str) != Some("F32") {
            return Err(format!("{name}: only F32 tensors are handled"));
        }
        let shape: Vec<u64> = match v.get("shape") {
            Some(Json::Arr(a)) => a.iter().map(|d| d.num().unwrap_or(0.0) as u64).collect(),
            _ => return Err(format!("{name}: no shape")),
        };
        let off = match v.get("data_offsets") {
            Some(Json::Arr(a)) if a.len() == 2 => (
                a[0].num().unwrap_or(0.0) as usize,
                a[1].num().unwrap_or(0.0) as usize,
            ),
            _ => return Err(format!("{name}: no data_offsets")),
        };
        let blob = data
            .get(off.0..off.1)
            .ok_or(format!("{name}: data out of range"))?;
        // GGUF dimensions are innermost first: [cols, rows].
        let dims: Vec<u64> = match shape.len() {
            1 => vec![shape[0]],
            2 => vec![shape[1], shape[0]],
            3 => vec![shape[1] * shape[2], shape[0]],
            _ => return Err(format!("{name}: {} dimensions", shape.len())),
        };
        if dims.iter().product::<u64>() as usize * 4 != blob.len() {
            return Err(format!("{name}: size does not match its shape"));
        }
        let short = name.strip_prefix("model.").unwrap_or(name);
        let mut info = Vec::new();
        put_str(&mut info, short);
        info.extend_from_slice(&(dims.len() as u32).to_le_bytes());
        for d in &dims {
            info.extend_from_slice(&d.to_le_bytes());
        }
        info.extend_from_slice(&0u32.to_le_bytes()); // F32
        info.extend_from_slice(&(offset as u64).to_le_bytes());
        infos.push(info);
        blobs.push(blob);
        offset = (offset + blob.len()).div_ceil(ALIGN) * ALIGN;
    }

    let mut out = Vec::with_capacity(st.len() + kv.len() + 4096);
    out.extend_from_slice(b"GGUF");
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&(infos.len() as u64).to_le_bytes());
    out.extend_from_slice(&n_kv.to_le_bytes());
    out.extend_from_slice(&kv);
    for i in &infos {
        out.extend_from_slice(i);
    }
    for b in &blobs {
        out.resize(out.len().div_ceil(ALIGN) * ALIGN, 0);
        out.extend_from_slice(b);
    }
    let path = gguf_path(root);
    std::fs::write(&path, &out).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    println!(
        "hear-gguf: wrote {} ({} bytes, {} tensors, {} tokens)",
        path.display(),
        out.len(),
        infos.len(),
        tokens.len()
    );
    Ok(path)
}

/// What `hear-check` says, in each of the host's voices.
pub const SENTENCES: [(&str, &str); 4] = [
    ("open-notes", "Hey Bella, open notes and maximize it."),
    ("split", "Put notes on the left and files on the right."),
    ("dentist", "Take a note that the dentist is on Friday."),
    ("monitor", "Put notes maximized on monitor 3."),
];
/// Phrases recorded in the microphone voice only: `voice-pause` (her
/// name, then the request after a pause) and `voice-confirm` (a delete,
/// then "yes" to her question).
pub const PAUSE_PARTS: [(&str, &str); 7] = [
    ("hey-bella", "Hey Bella."),
    ("request", "Open notes and maximize it."),
    ("remove-milk", "Hey Bella, remove milk."),
    ("yes", "Yes."),
    // `voice-follow` and `voice-follow-chat` (docs/follow-design.md).
    ("open-notes-only", "Hey Bella, open notes."),
    ("maximize-notes", "Maximize notes."),
    ("chat", "I think it is raining again."),
];
/// Quiet before "yes" in `voice-confirm` and before the follow-up in
/// `voice-follow` (3 s). Under TCG her question comes only once the
/// request is written down, seconds later: BatHandler keeps a sentence
/// said just before (docs/late-design.md), so the same recording works
/// with or without a hardware accelerator.
const ANSWER_GAP_SECONDS: usize = 3;
/// The host voices (Windows' System.Speech), each saying every sentence.
pub const VOICES: [&str; 2] = ["Microsoft David Desktop", "Microsoft Zira Desktop"];

/// What the microphone cases play: this voice's `open-notes` recording
/// (docs/microphone-design.md).
pub const MIC_SENTENCE: &str = "Hey Bella, open notes and maximize it.";
const MIC_VOICE: usize = 1;

/// The raw microphone input (16-bit mono at 16 kHz) for a `mic-` or
/// `voice-` case: one second of silence, the recording of `sentence`
/// (a `SENTENCES` name), two seconds of silence; or, with `silent`, the
/// same length of silence (the negative case). With `room`, the recording
/// in a noisy room instead (`in_room`: digital silence, then noise under
/// everything), as a real microphone gives it.
pub fn mic_input(root: &Path, sentence: &str, silent: bool, room: bool) -> Result<PathBuf, String> {
    make_recordings(root)?;
    let samples_of = |name: &str| -> Result<Vec<u8>, String> {
        let wav_file = wav_path(root, MIC_VOICE, name);
        let wav = read(&wav_file)?;
        zoo_infer::hear::wav_samples(&wav)
            .map(<[u8]>::to_vec)
            .ok_or(format!("{}: not 16 kHz mono 16-bit", wav_file.display()))
    };
    // `pause`: her name, a second of quiet, then the request.
    // `confirm`: a delete, quiet while she asks, then "yes".
    // `follow`: a request, quiet while she does it, then a follow-up
    // without her name; `follow-chat`: talk instead.
    let pcm = if matches!(sentence, "pause" | "confirm" | "follow" | "follow-chat") {
        let (first, second, gap) = match sentence {
            "pause" => (PAUSE_PARTS[0].0, PAUSE_PARTS[1].0, 1),
            "confirm" => (PAUSE_PARTS[2].0, PAUSE_PARTS[3].0, ANSWER_GAP_SECONDS),
            "follow" => (PAUSE_PARTS[4].0, PAUSE_PARTS[5].0, ANSWER_GAP_SECONDS),
            _ => (PAUSE_PARTS[4].0, PAUSE_PARTS[6].0, ANSWER_GAP_SECONDS),
        };
        let mut p = samples_of(first)?;
        p.resize(p.len() + gap * 2 * zoo_infer::hear::RATE, 0);
        p.extend(samples_of(second)?);
        p
    } else {
        samples_of(sentence)?
    };
    let pcm = pcm.as_slice();
    let second = 2 * zoo_infer::hear::RATE;
    let mut raw = vec![0u8; second];
    if silent {
        raw.resize(raw.len() + pcm.len(), 0);
    } else {
        raw.extend_from_slice(pcm);
    }
    raw.resize(raw.len() + 2 * second, 0);
    if room {
        let pcm16: Vec<i16> = pcm
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        raw = in_room(&pcm16, 9)
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
    }
    let out = wav_dir(root).join(if room {
        format!("mic-{sentence}-room.raw")
    } else if silent {
        format!("mic-{sentence}-silence.raw")
    } else {
        format!("mic-{sentence}.raw")
    });
    std::fs::write(&out, &raw).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    Ok(out)
}

/// Where the recordings go (made, not committed: build/ is ignored).
pub fn wav_dir(root: &Path) -> PathBuf {
    root.join("build/voice")
}

pub fn wav_path(root: &Path, voice: usize, name: &str) -> PathBuf {
    wav_dir(root).join(format!("{name}-{voice}.wav"))
}

/// Records every sentence in every voice as 16 kHz mono 16-bit WAV with
/// the host's speech synthesizer, unless already there.
pub fn make_recordings(root: &Path) -> Result<(), String> {
    let dir = wav_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let mut script = String::from(
        "Add-Type -AssemblyName System.Speech; \
         $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
         $f = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, \
         [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, \
         [System.Speech.AudioFormat.AudioChannel]::Mono); ",
    );
    let mut needed = false;
    for (v, voice) in VOICES.iter().enumerate() {
        let extra: &[(&str, &str)] = if v == MIC_VOICE { &PAUSE_PARTS } else { &[] };
        for &(name, text) in SENTENCES.iter().chain(extra) {
            let p = wav_path(root, v, name);
            if p.is_file() {
                continue;
            }
            needed = true;
            script.push_str(&format!(
                "$s.SelectVoice('{voice}'); $s.SetOutputToWaveFile('{}', $f); \
                 $s.Speak('{}'); $s.SetOutputToNull(); ",
                p.display(),
                text.replace('\'', "''")
            ));
        }
    }
    if !needed {
        return Ok(());
    }
    script.push_str("$s.Dispose()");
    let mut cmd = std::process::Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    let out = crate::process::output_bounded(
        cmd,
        std::time::Duration::from_secs(120),
        "speech synthesizer",
    )?;
    if !out.status.success() {
        return Err(format!(
            "the host speech synthesizer failed (Windows System.Speech is needed): {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn python(root: &Path) -> Result<PathBuf, String> {
    let p = root.join("tools/reference/.venv/Scripts/python.exe");
    let p = if p.is_file() {
        p
    } else {
        root.join("tools/reference/.venv/bin/python")
    };
    if p.is_file() {
        Ok(p)
    } else {
        Err("reference venv missing; see tools/reference/README.md".into())
    }
}

/// The room noise `hear-check` mixes in (dB of full scale, RMS).
const ROOM_DB: f32 = -45.0;

/// `speech` as a microphone in a room gives it: 300 ms of digital silence
/// (the device starting), 1.5 s of noise, the speech with the noise under
/// it, and 2 s of noise. The noise is a fixed pseudo-random hiss.
pub fn in_room(speech: &[i16], seed: u32) -> Vec<i16> {
    let mut x = seed.wrapping_add(1);
    let amp = 10f32.powf(ROOM_DB / 20.0) * 32767.0 * 3f32.sqrt();
    let mut noise = move || {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((x >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * amp
    };
    let mut out = vec![0i16; 16 * 300];
    out.extend((0..16 * 1500).map(|_| noise() as i16));
    out.extend(
        speech
            .iter()
            .map(|&s| (f32::from(s) + noise()).clamp(-32768.0, 32767.0) as i16),
    );
    out.extend((0..16 * 2000).map(|_| noise() as i16));
    out
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

/// `cargo xtask hear-check`: for every recording, the engine's transcript
/// must be the sentence said (by words), its tokens the reference's, and
/// its encoder output and first logits within tolerance of the reference's.
pub fn check(root: &Path) -> Result<bool, String> {
    // Set before measuring: F32 throughout, so only summation order differs.
    const ENC_TOLERANCE: f32 = 1e-3;
    const LOGIT_TOLERANCE: f32 = 1e-2;
    let path = gguf_path(root);
    if !path.is_file() {
        convert(root)?;
    }
    let data = read(&path)?;
    let g = zoo_infer::Gguf::parse(&data).map_err(|e| format!("{}: {e:?}", path.display()))?;
    let h = zoo_infer::hear::Hearing::from_gguf(&g)
        .map_err(|e| format!("{}: {}", path.display(), e.name()))?;
    make_recordings(root)?;
    let py = python(root)?;
    let dump = wav_dir(root).join("ref.f32");
    let mut all_ok = true;
    for (v, _) in VOICES.iter().enumerate() {
        for (name, said) in SENTENCES {
            let wav_file = wav_path(root, v, name);
            let wav = read(&wav_file)?;
            let pcm = zoo_infer::hear::wav_samples(&wav)
                .ok_or(format!("{}: not 16 kHz mono 16-bit", wav_file.display()))?;
            let audio = zoo_infer::hear::samples_from_i16(pcm);
            let t0 = std::time::Instant::now();
            let (enc, frames) = h.encode(&audio).map_err(|e| e.name().to_string())?;
            let (ids, first) = h.decode(
                &enc,
                frames,
                zoo_infer::hear::Hearing::token_limit(audio.len()),
            );
            let secs = t0.elapsed().as_secs_f64();
            let text = h.text(&ids);

            let mut cmd = std::process::Command::new(&py);
            cmd.args(["-I", "-X", "utf8"])
                .arg(root.join("tools/reference/moonshine_ref.py"))
                .arg(moonshine_dir(root))
                .arg(&wav_file)
                .arg("--dump")
                .arg(&dump);
            let out = crate::process::output_bounded(
                cmd,
                std::time::Duration::from_secs(300),
                "reference",
            )?;
            if !out.status.success() {
                return Err(format!(
                    "reference failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
            let stdout = String::from_utf8_lossy(&out.stdout);
            let ref_ids: Vec<u32> = stdout
                .lines()
                .find_map(|l| l.strip_prefix("ids"))
                .unwrap_or("")
                .split_whitespace()
                .filter_map(|t| t.parse().ok())
                .collect();
            let raw = read(&dump)?;
            let r: Vec<f32> = raw
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect();
            let (r_enc, r_first) = r.split_at(r.len().saturating_sub(first.len()));
            let enc_diff = if r_enc.len() == enc.len() {
                max_diff(&enc, r_enc)
            } else {
                f32::INFINITY
            };
            let logit_diff = max_diff(&first, r_first);
            // Voice detection on the recording between silences
            // (docs/listening-design.md): exactly one utterance, heard
            // the same, addressed to Bella only when it says "hey Bella".
            let pcm16: Vec<i16> = pcm
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b))
                .collect();
            let mut padded = vec![0i16; 16000];
            padded.extend_from_slice(&pcm16);
            padded.extend(std::iter::repeat_n(0i16, 32000));
            let mut det = zoo_infer::listen::Detector::new();
            let mut utterances = det.push(&padded);
            utterances.extend(det.finish());
            let detected_ok = utterances.len() == 1 && {
                let u: Vec<f32> = utterances[0].iter().map(|&s| s as f32 / 32768.0).collect();
                let heard = h.transcribe(&u).map_err(|e| e.name().to_string())?;
                let ok = zoo_infer::hear::same_words(&heard, said);
                if !ok {
                    println!("     the detected utterance was heard as {heard:?}");
                }
                ok
            };
            // The same in a room (what `run --mic` met): 300 ms of digital
            // silence, then noise at ROOM_DB under everything.
            let noisy = in_room(&pcm16, v as u32 * 4 + name.len() as u32);
            let mut det = zoo_infer::listen::Detector::new();
            let mut in_noise = det.push(&noisy);
            in_noise.extend(det.finish());
            // One utterance about as long as the speech: the noise must not
            // keep it going (the old detector ran on for 7 s).
            let room_ok = in_noise.len() == 1 && in_noise[0].len() <= pcm16.len() + 16 * 500 && {
                let u: Vec<f32> = in_noise[0].iter().map(|&s| s as f32 / 32768.0).collect();
                let heard = h.transcribe(&u).map_err(|e| e.name().to_string())?;
                let ok = zoo_infer::hear::same_words(&heard, said);
                if !ok {
                    println!("     in the noisy room it was heard as {heard:?}");
                }
                ok
            };
            println!(
                "     in a room at {ROOM_DB} dB: {} utterance(s) {:?} ms",
                in_noise.len(),
                in_noise.iter().map(|u| u.len() / 16).collect::<Vec<_>>()
            );
            // Pauses (docs/endpoint-design.md): the speech so far at each
            // 300 ms pause must not sound finished unless it is the whole
            // sentence, or BatHandler would take half of it.
            let mut det = zoo_infer::listen::Detector::new();
            let mut pauses = Vec::new();
            for ev in det.push_heard(&padded) {
                if let zoo_infer::listen::Heard::Pause(u) = ev {
                    let u: Vec<f32> = u.iter().map(|&s| s as f32 / 32768.0).collect();
                    let heard = h.transcribe(&u).map_err(|e| e.name().to_string())?;
                    let finished = zoo_abi::bella::sounds_finished(&heard);
                    let whole = zoo_infer::hear::same_words(&heard, said);
                    pauses.push((heard, finished, whole));
                }
            }
            let cut_short = pauses.iter().any(|(_, f, w)| *f && !*w);
            println!(
                "     pauses: {}",
                if pauses.is_empty() {
                    "none".to_string()
                } else {
                    pauses
                        .iter()
                        .map(|(t, f, w)| {
                            format!(
                                "{t:?} {}",
                                match (f, w) {
                                    (true, true) => "(taken: whole)",
                                    (true, false) => "(taken: CUT SHORT)",
                                    (false, _) => "(waits)",
                                }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
            let detected_ok = detected_ok && room_ok && !cut_short;
            let for_bella = zoo_abi::bella::addressed(&text).is_some();
            let gate_ok = for_bella == said.to_ascii_lowercase().starts_with("hey bella");
            println!(
                "     voice detection: {} utterance(s){}; {}",
                utterances.len(),
                utterances
                    .first()
                    .map(|u| format!(" of {} ms", u.len() / 16))
                    .unwrap_or_default(),
                if for_bella {
                    "addressed to Bella"
                } else {
                    "not for Bella"
                }
            );
            let heard_ok = zoo_infer::hear::same_words(&text, said) && detected_ok && gate_ok;
            let same = ids == ref_ids;
            let ok = heard_ok && same && enc_diff <= ENC_TOLERANCE && logit_diff <= LOGIT_TOLERANCE;
            all_ok &= ok;
            println!(
                "{} voice {v} {name}: {text:?} in {secs:.2}s ({:.1} s of audio); {}; encoder diff {enc_diff:.2e}, logit diff {logit_diff:.2e}",
                if ok { "OK  " } else { "FAIL" },
                audio.len() as f64 / 16000.0,
                if same {
                    "same tokens as the reference"
                } else {
                    "tokens differ from the reference"
                },
            );
            if !heard_ok {
                println!("     wanted {said:?}");
            }
        }
    }
    Ok(all_ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_values() {
        let j = parse_json(r#"{"a": [1, -2.5e1, "xé\n"], "b": {"c": true, "d": null}}"#).unwrap();
        let Some(Json::Arr(a)) = j.get("a") else {
            panic!()
        };
        assert_eq!(a[1].num(), Some(-25.0));
        assert_eq!(a[2].str(), Some("x\u{e9}\n"));
        assert!(matches!(
            j.get("b").and_then(|b| b.get("c")),
            Some(Json::Bool(true))
        ));
        assert!(parse_json("{\"a\": 1,}").is_err());
        assert!(parse_json("[1] x").is_err());
    }
}
