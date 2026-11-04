//! `cargo xtask infer`: runs `zoo-infer` on the host against the real model
//! (docs/m5b-design.md §4). Used to check the engine against the NumPy
//! reference, to produce the expected tokens for the in-guest check, and
//! as a host benchmark.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;
use zoo_infer::{Gguf, Model, Tokenizer, chat};

/// The models Bella can use: (`--bella` name, file in tools/models/), each
/// pinned by its `.sha256` file (tools/models/README.md). The first is the
/// default; the test suite uses it.
pub const MODELS: [(&str, &str); 4] = [
    ("0.5b", "qwen2.5-0.5b-instruct-q4_0.gguf"),
    ("1.5b", "qwen2.5-1.5b-instruct-q4_0.gguf"),
    // Under evaluation (docs/qwen3-design.md).
    ("qwen3-1.7b", "qwen3-1.7b-q8_0.gguf"),
    // Made by `cargo xtask requant --bella qwen3-1.7b`.
    ("qwen3-1.7b-q4", "qwen3-1.7b-q4_0.gguf"),
];

static SELECTED: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// `--bella <name>`: which of `MODELS` this command uses.
pub fn select_model(name: &str) -> Result<(), String> {
    let file = MODELS
        .iter()
        .find(|m| m.0 == name)
        .map(|m| m.1)
        .ok_or_else(|| {
            let names: Vec<&str> = MODELS.iter().map(|m| m.0).collect();
            format!("unknown model `{name}` (one of: {})", names.join(", "))
        })?;
    SELECTED
        .set(file)
        .map_err(|_| String::from("the model was already chosen"))
}

/// Bella's model unless `--bella` says otherwise: Qwen3-1.7B in 4 bits,
/// chosen by the owner on 2026-11-03 (docs/qwen3-design.md).
pub const DEFAULT_MODEL: &str = "qwen3-1.7b-q4";

/// The chosen model's file name.
pub fn model_file() -> &'static str {
    SELECTED.get().copied().unwrap_or_else(|| {
        MODELS
            .iter()
            .find(|m| m.0 == DEFAULT_MODEL)
            .map_or(MODELS[0].1, |m| m.1)
    })
}

/// Largest model `verify` puts in the normal boot image for the cases
/// that check it there (`inference`, `bella-model`); with a larger one
/// the Bella cases read it from the model disk, as `run` does. Any size
/// fits a boot image since docs/usb-model-design.md (the USB stick holds
/// Qwen3 and the embedding model); this keeps the verified setup.
pub const BOOT_IMAGE_MODEL_MAX: u64 = 600 << 20;

/// Whether `verify` puts the chosen model in the boot image.
pub fn model_in_boot_image(root: &Path) -> bool {
    std::fs::metadata(model_path(root)).is_ok_and(|m| m.len() <= BOOT_IMAGE_MODEL_MAX)
}
pub const SYSTEM: &str = "You are a helpful assistant.";
/// The prompt ElephantHandler runs in the guest, and how many tokens.
pub const GUEST_PROMPT: &str = "What is the capital of France?";
pub const GUEST_TOKENS: usize = 8;

pub fn model_path(root: &Path) -> PathBuf {
    root.join("tools").join("models").join(model_file())
}

/// The embedding model (docs/vectors-design.md), approved by the owner
/// on 2026-11-03.
pub const EMBEDDER: &str = "qwen3-embedding-0.6b-q8_0.gguf";

pub fn embedder_path(root: &Path) -> PathBuf {
    root.join("tools").join("models").join(EMBEDDER)
}

/// Reads the model and checks it against the pinned SHA-256.
pub fn load_model(root: &Path) -> Result<Vec<u8>, String> {
    load_pinned(&model_path(root))
}

/// Reads the embedding model and checks it against its pinned SHA-256.
pub fn load_embedder(root: &Path) -> Result<Vec<u8>, String> {
    load_pinned(&embedder_path(root))
}

/// The model card's example (huggingface.co/Qwen/Qwen3-Embedding-0.6B):
/// two queries with the card's web-search instruction, two documents,
/// and the scores the card prints for them.
const CARD_TASK: &str =
    "Given a web search query, retrieve relevant passages that answer the query";
const CARD_QUERIES: [&str; 2] = ["What is the capital of China?", "Explain gravity"];
const CARD_DOCS: [&str; 2] = [
    "The capital of China is Beijing.",
    "Gravity is a force that attracts two bodies towards each other. It gives weight to physical objects and is responsible for the movement of planets around the sun.",
];
const CARD_SCORES: [[f32; 2]; 2] = [[0.7646, 0.1414], [0.1355, 0.6000]];
/// How far our 8-bit run may be from the card's (bf16) scores.
const CARD_TOLERANCE: f32 = 0.03;

/// `cargo xtask embed-check`: the card's example at full width (1024), then
/// Bella's own case at `embed::DIM`: a question about the vet is nearest
/// the vet line of `Documents/notes.txt` though they share one word.
pub fn embed_check(root: &Path) -> Result<bool, String> {
    use zoo_infer::embed::{Embedder, similarity};
    let data = load_embedder(root)?;
    let g = zoo_infer::gguf::Gguf::parse(&data).map_err(|e| format!("{e:?}"))?;
    let model = zoo_infer::model::Model::from_gguf(&g).map_err(|e| format!("{e:?}"))?;
    let tok = zoo_infer::tokenizer::Tokenizer::from_gguf(&g).map_err(|e| format!("{e:?}"))?;
    let mut e = Embedder::new(&model, &tok);
    let full = model.cfg.n_embd;
    let t0 = std::time::Instant::now();
    let q: Vec<Vec<f32>> = CARD_QUERIES
        .iter()
        .map(|q| e.embed(&format!("Instruct: {CARD_TASK}\nQuery:{q}"), full))
        .collect::<Result<_, _>>()
        .map_err(|x| format!("{x:?}"))?;
    let d: Vec<Vec<f32>> = CARD_DOCS
        .iter()
        .map(|t| e.embed(t, full))
        .collect::<Result<_, _>>()
        .map_err(|x| format!("{x:?}"))?;
    let mut ok = true;
    for (i, qi) in q.iter().enumerate() {
        for (j, dj) in d.iter().enumerate() {
            let s = similarity(qi, dj);
            let want = CARD_SCORES[i][j];
            let good = (s - want).abs() <= CARD_TOLERANCE;
            ok &= good;
            println!(
                "{} card query {i} / document {j}: {s:.4} (card {want:.4})",
                if good { "OK  " } else { "FAIL" }
            );
        }
    }
    println!(
        "4 embeddings in {:.2}s ({} dims)",
        t0.elapsed().as_secs_f64(),
        full
    );
    // Bella's case, at the dimensions the index keeps.
    let lines = [
        "Documents/notes.txt: Shopping: flour, eggs, milk.",
        "Documents/notes.txt: Call the vet about Zara the zebra.",
        "Documents/recipes/pancakes.txt: Whisk, rest 10 minutes, cook on a hot pan.",
    ];
    let question = "when do I need to take the zebra to the animal doctor?";
    let qv = e.query(question).map_err(|x| format!("{x:?}"))?;
    let mut scores = Vec::new();
    for l in lines {
        let v = e.document(l).map_err(|x| format!("{x:?}"))?;
        scores.push(similarity(&qv, &v));
    }
    let best = (0..scores.len())
        .max_by(|&a, &b| scores[a].total_cmp(&scores[b]))
        .unwrap_or(0);
    for (l, s) in lines.iter().zip(&scores) {
        println!("     {s:.4}  {l}");
    }
    let vet = best == 1;
    println!(
        "{} \"{question}\" is nearest the vet line",
        if vet { "OK  " } else { "FAIL" }
    );
    Ok(ok && vet)
}

fn load_pinned(path: &Path) -> Result<Vec<u8>, String> {
    use sha2::{Digest, Sha256};
    let path = path.to_path_buf();
    // The 4-bit Qwen3 file is made here, not downloaded.
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !path.exists() && name.contains("q4_0") && name.starts_with("qwen3") {
        return Err(format!(
            "{} is missing; make it with `cargo xtask requant --bella qwen3-1.7b` \
             after downloading the Q8_0 file (tools/models/README.md)",
            path.display()
        ));
    }
    let data = std::fs::read(&path).map_err(|e| {
        format!(
            "cannot read {} ({e}); see tools/models/README.md",
            path.display()
        )
    })?;
    let pinned = std::fs::read_to_string(path.with_extension("gguf.sha256"))
        .map_err(|e| format!("cannot read the model's .sha256 file: {e}"))?;
    let want = pinned
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let got: String = Sha256::digest(&data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got != want {
        return Err(format!(
            "{} has SHA-256 {got}, expected {want}",
            path.display()
        ));
    }
    Ok(data)
}

pub struct Run {
    pub prompt_ids: Vec<u32>,
    pub output_ids: Vec<u32>,
    pub text: String,
    /// Top five (token, logit) after the prompt.
    pub first_top: Vec<(u32, f32)>,
    pub prefill_secs: f64,
    pub decode_secs: f64,
}

pub fn run(data: &[u8], user: &str, max_new: usize) -> Result<Run, String> {
    run_with_system(data, SYSTEM, user, max_new)
}

pub fn run_with_system(
    data: &[u8],
    system: &str,
    user: &str,
    max_new: usize,
) -> Result<Run, String> {
    let g = Gguf::parse(data).map_err(|e| format!("GGUF: {e:?}"))?;
    let tok = Tokenizer::from_gguf(&g).map_err(|e| format!("tokenizer: {e:?}"))?;
    let model = Model::from_gguf(&g).map_err(|e| format!("model: {e:?}"))?;
    let prompt_ids = tok.encode(&chat::prompt_for(&model, system, user));
    let mut st = model.new_state(prompt_ids.len() + max_new + 1);
    let t0 = Instant::now();
    for (k, &id) in prompt_ids.iter().enumerate() {
        if k + 1 == prompt_ids.len() {
            model.forward(&mut st, id)
        } else {
            model.feed(&mut st, id)
        }
        .map_err(|e| format!("forward: {e:?}"))?;
    }
    let prefill_secs = t0.elapsed().as_secs_f64();
    let mut idx: Vec<usize> = (0..st.logits.len()).collect();
    idx.sort_by(|&a, &b| st.logits[b].total_cmp(&st.logits[a]));
    let first_top = idx[..5].iter().map(|&i| (i as u32, st.logits[i])).collect();
    let t1 = Instant::now();
    let output_ids = chat::generate(&model, &tok, &mut st, &[], max_new, |_| true)
        .map_err(|e| format!("generate: {e:?}"))?;
    let decode_secs = t1.elapsed().as_secs_f64();
    let text = tok.decode(&output_ids);
    Ok(Run {
        prompt_ids,
        output_ids,
        text,
        first_top,
        prefill_secs,
        decode_secs,
    })
}

pub fn ids(v: &[u32]) -> String {
    let mut s = String::new();
    for (i, id) in v.iter().enumerate() {
        let _ = write!(s, "{}{id}", if i == 0 { "" } else { "," });
    }
    s
}

/// Prompts for `infer-check`: chosen to exercise digits, newlines,
/// punctuation runs, non-ASCII text, contractions and an assistant-style
/// file request.
pub const CHECK_PROMPTS: [&str; 5] = [
    "What is the capital of France?",
    "List three animals found in a zoo, one per line.",
    "Compute 12 + 30 and explain in one sentence.",
    "Translate 'café' to English; don't explain.",
    "Create a folder named Projects/2026 in my home directory.",
];

fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.strip_prefix(key))
        .map(str::trim)
}

fn parse_ids(s: &str) -> Vec<u32> {
    s.split(',')
        .filter(|x| !x.is_empty())
        .filter_map(|x| x.parse().ok())
        .collect()
}

fn parse_top(s: &str) -> Vec<(u32, f32)> {
    s.split(',')
        .filter_map(|p| p.split_once(':'))
        .filter_map(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
        .collect()
}

/// Compares the Rust engine with the NumPy reference on `CHECK_PROMPTS`.
/// Token ids must match exactly; the top-5 logits after the prompt must
/// name the same tokens and agree within `LOGIT_TOLERANCE`, or, with 8-bit
/// activations (docs/int8-design.md), within `INT8_LOGIT_TOLERANCE`, set
/// before it was measured.
pub fn check(root: &Path, max_new: usize) -> Result<bool, String> {
    const LOGIT_TOLERANCE: f32 = 0.02;
    const INT8_LOGIT_TOLERANCE: f32 = 0.5;
    let tolerance = if zoo_infer::quant::int8_for(zoo_infer::quant::TYPE_Q4_0) {
        INT8_LOGIT_TOLERANCE
    } else {
        LOGIT_TOLERANCE
    };
    let data = load_model(root)?;
    let python = root.join("tools/reference/.venv/Scripts/python.exe");
    let python = if python.is_file() {
        python
    } else {
        root.join("tools/reference/.venv/bin/python")
    };
    if !python.is_file() {
        return Err("reference venv missing; see tools/reference/README.md".into());
    }
    let mut all_ok = true;
    for prompt in CHECK_PROMPTS {
        let ours = run(&data, prompt, max_new)?;
        let mut cmd = std::process::Command::new(&python);
        cmd.args(["-I", "-X", "utf8"])
            .arg(root.join("tools/reference/qwen_ref.py"))
            .arg(model_path(root))
            .arg(prompt)
            .arg(max_new.to_string());
        let out =
            crate::process::output_bounded(cmd, std::time::Duration::from_secs(600), "reference")?;
        if !out.status.success() {
            return Err(format!(
                "reference failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let ref_prompt = parse_ids(field(&text, "prompt_ids=").unwrap_or(""));
        let ref_out = parse_ids(field(&text, "output_ids=").unwrap_or(""));
        let ref_top = parse_top(field(&text, "first_top=").unwrap_or(""));
        let same_prompt = ref_prompt == ours.prompt_ids;
        let same_out = ref_out == ours.output_ids;
        let max_diff = ours
            .first_top
            .iter()
            .zip(&ref_top)
            .map(|((a, la), (b, lb))| {
                if a == b {
                    (la - lb).abs()
                } else {
                    f32::INFINITY
                }
            })
            .fold(0.0f32, f32::max);
        let ok = same_prompt && same_out && ref_top.len() == 5 && max_diff <= tolerance;
        all_ok &= ok;
        println!(
            "{:<4} {:?}
     prompt tokens {} ({}), output tokens {} ({}), top-5 logit max diff {:.5}
     output: {:?}",
            if ok { "OK" } else { "FAIL" },
            prompt,
            ours.prompt_ids.len(),
            if same_prompt { "same" } else { "DIFFERENT" },
            ours.output_ids.len(),
            if same_out { "same" } else { "DIFFERENT" },
            max_diff,
            ours.text
        );
    }
    Ok(all_ok)
}

/// Requests for `assist-check` with the tools and arguments accepted
/// (docs/m5c2-design.md §5, docs/stage1-design.md). Several are accepted where
/// they mean the same to the broker. `none` means no command fits: the
/// first 8 are the original set; then more requests with a right command;
/// then requests no command answers (B9).
pub const ASSIST_CASES: &[(&str, &[&str], &[&str])] = &[
    (
        "Find my pancake recipe",
        &["search"],
        &["pancake", "pancake recipe"],
    ),
    (
        "Search for files that mention flour",
        &["search"],
        &["flour"],
    ),
    ("What is in my Documents folder?", &["list"], &["Documents"]),
    ("Show me what's inside Music", &["list"], &["Music"]),
    (
        "How big is Documents/notes.txt?",
        &["inspect"],
        &["Documents/notes.txt"],
    ),
    (
        "Make a folder called Projects/2026",
        &["create"],
        &["Projects/2026"],
    ),
    (
        "Create a new folder named Invoices",
        &["create"],
        &["Invoices"],
    ),
    (
        "Please create the folder Photos/Holidays",
        &["create"],
        &["Photos/Holidays"],
    ),
    // More with a right command.
    ("List the files in Photos", &["list"], &["Photos"]),
    ("How large is the Music folder?", &["inspect"], &["Music"]),
    ("Look for anything about taxes", &["search"], &["taxes"]),
    (
        "Make a new folder for receipts",
        &["create"],
        &["receipts", "Receipts"],
    ),
    (
        "Which files are in Documents/recipes?",
        &["list"],
        &["Documents/recipes"],
    ),
    ("Where is my playlist?", &["search"], &["playlist"]),
    (
        "where is my music",
        &["search", "list"],
        &["music", "Music"],
    ),
    // Broad access (docs/access-design.md).
    (
        "Open my notes file",
        &["open"],
        &["Documents/notes.txt", "notes.txt"],
    ),
    ("Open the Photos folder", &["open"], &["Photos"]),
    (
        "Show me what notes.txt says",
        &["read"],
        &["Documents/notes.txt", "notes.txt"],
    ),
    (
        "Move notes.txt into Archive",
        &["move"],
        &[
            "notes.txt to Archive",
            "Documents/notes.txt to Archive",
            "Documents/notes.txt to Archive/notes.txt",
        ],
    ),
    (
        "Throw away the recipes folder",
        &["trash"],
        &["recipes", "Documents/recipes"],
    ),
    (
        "Delete my old playlist",
        &["trash"],
        &["playlist", "playlist.txt", "Music/playlist.txt"],
    ),
    ("Launch the Notes app", &["launch"], &["Notes", "notes"]),
    // Window actions (docs/window-actions-design.md).
    (
        "Open notes and maximize it on my screen",
        &["maximize"],
        &["Notes"],
    ),
    (
        "Make Files fill the whole screen",
        &["maximize"],
        &["Files"],
    ),
    ("Hide the Network window", &["minimize"], &["Network"]),
    ("Quit Notes", &["close"], &["Notes"]),
    // Tiling (docs/tiling-design.md).
    (
        "Put notes on the left and the assistant on the right",
        &["split"],
        &["Notes and Bella"],
    ),
    (
        "Show Files and Notes side by side",
        &["split"],
        &["Files and Notes"],
    ),
    // Worded freely (the owner's examples, 2026-11-03).
    ("hey bella split these 2 windows", &["split"], &[""]),
    (
        "can you split the screen with the files, notes and network apps",
        &["split"],
        &["Files and Notes and Network"],
    ),
    (
        "I want Notes and Files next to each other",
        &["split"],
        &["Notes and Files"],
    ),
    ("Tile my windows", &["split"], &[""]),
    (
        "put two finder windows side by side",
        &["split"],
        &["Files and Files"],
    ),
    ("How much space am I using?", &["info"], &[""]),
    // No command fits.
    ("What's the weather today?", &["none"], &[""]),
    ("Tell me a joke", &["none"], &[""]),
    ("Send an email to my boss", &["none"], &[""]),
    ("What time is it?", &["none"], &[""]),
    ("Turn up the volume", &["none"], &[""]),
    ("Who won the game last night?", &["none"], &[""]),
    ("Shut down the computer", &["none"], &[""]),
    ("Order me a pizza", &["none"], &[""]),
];

/// Runs the guided tool choice on `ASSIST_CASES` with the host engine.
pub fn assist_check(root: &Path, extra: Option<&str>) -> Result<bool, String> {
    use zoo_infer::assist::Bella;
    let data = load_model(root)?;
    let g = Gguf::parse(&data).map_err(|e| format!("GGUF: {e:?}"))?;
    let tok = Tokenizer::from_gguf(&g).map_err(|e| format!("tokenizer: {e:?}"))?;
    let model = Model::from_gguf(&g).map_err(|e| format!("model: {e:?}"))?;
    println!("engine: {}", zoo_infer::cpu::backend());
    let t0 = Instant::now();
    let mut a = Bella::new(&model, &tok, |_| true).map_err(|e| format!("{e:?}"))?;
    println!(
        "system prompt: {} tokens in {:.1}s",
        a.prefix_tokens(),
        t0.elapsed().as_secs_f64()
    );
    // A scripted follow-up (docs/talk-design.md §6): a command answered,
    // then "is that it?" with the last exchange as context.
    if extra == Some("--dialogue") {
        let mut talk =
            zoo_infer::talk::Talk::new(&model, &tok, |_| true).map_err(|e| format!("{e:?}"))?;
        let asked = "where is my music";
        let said = "I ran `list Music` and it showed: file playlist.txt (46 bytes).";
        talk.note(asked, said, |_| true)
            .map_err(|e| format!("{e:?}"))?;
        let context = format!("The user asked: {asked}. Bella: {said}");
        for follow in ["is that it?", "what is in it?", "what about Photos?"] {
            let c = a
                .choose_in(follow, &context, |_| true)
                .map_err(|e| format!("{e:?}"))?;
            print!("{follow:?} -> {} {:?}", c.tool.word(), c.arg);
            if c.tool.word() == "none" || c.arg.is_empty() {
                let r = talk.reply(follow, |_| true).map_err(|e| format!("{e:?}"))?;
                print!("; Bella says: {r:?}");
            }
            println!();
        }
        return Ok(true);
    }
    if let Some(req) = extra {
        let c = a.choose(req, |_| true).map_err(|e| format!("{e:?}"))?;
        println!(
            "{req:?} -> {} {:?} (margin {:.2})",
            c.tool.word(),
            c.arg,
            c.margin
        );
        // No command fits: Bella answers in words (docs/talk-design.md).
        if c.tool.word() == "none" {
            let t = Instant::now();
            let mut talk =
                zoo_infer::talk::Talk::new(&model, &tok, |_| true).map_err(|e| format!("{e:?}"))?;
            let p = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let r = talk.reply(req, |_| true).map_err(|e| format!("{e:?}"))?;
            println!(
                "Bella says: {r:?} (prompt {} tokens in {p:.1}s, reply {:.1}s)",
                talk.prefix_tokens(),
                t.elapsed().as_secs_f64()
            );
        }
        return Ok(true);
    }
    let mut ok = true;
    // (right, total) with a command; (acted, total) without one.
    let (mut right, mut with) = (0, 0);
    let (mut acted, mut without) = (0, 0);
    for &(req, tools, args) in ASSIST_CASES {
        let t = Instant::now();
        let c = a.choose(req, |_| true).map_err(|e| format!("{e:?}"))?;
        let good = tools.contains(&c.tool.word()) && args.contains(&c.arg.as_str());
        ok &= good;
        if tools.contains(&"none") {
            without += 1;
            acted += usize::from(c.tool.word() != "none");
        } else {
            with += 1;
            right += usize::from(good);
        }
        let (tool, arg) = (tools, args);
        println!(
            "{} {:<45} -> {} {:?} (margin {:.2}, {} + {} tokens, {:.1}s){}",
            if good { "OK  " } else { "FAIL" },
            format!("{req:?}"),
            c.tool.word(),
            c.arg,
            c.margin,
            c.request_tokens,
            c.arg_tokens,
            t.elapsed().as_secs_f64(),
            if good {
                String::new()
            } else {
                format!("; expected {tool:?} {arg:?}")
            }
        );
    }
    println!(
        "accuracy: {right} of {with} requests with a command answered right; {acted} of {without} requests without one acted on anyway"
    );
    // The bar is the measured state of 2026-11-02 (docs/stage1-design.md): a
    // change that answers fewer right, or acts more often, fails.
    let pass = right >= MIN_RIGHT && acted <= MAX_ACTED;
    if !ok && pass {
        println!(
            "within the recorded bar (at least {MIN_RIGHT} right, at most {MAX_ACTED} acted on)"
        );
    }
    Ok(pass)
}

/// The recorded bar for `assist-check` (docs/stage1-design.md).
/// 23 requests with a command since docs/access-design.md (0.5B: 17).
const MIN_RIGHT: usize = 17;
/// Of 8 without one (0.5B: 2).
const MAX_ACTED: usize = 2;
