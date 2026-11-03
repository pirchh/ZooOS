//! `cargo xtask infer`: runs `zoo-infer` on the host against the real model
//! (docs/m5b-design.md §4). Used to check the engine against the NumPy
//! reference, to produce the expected tokens for the in-guest check, and
//! as a host benchmark.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;
use zoo_infer::{Gguf, Model, Tokenizer, chat};

pub const MODEL_FILE: &str = "qwen2.5-0.5b-instruct-q4_0.gguf";
pub const SYSTEM: &str = "You are a helpful assistant.";
/// The prompt ElephantHandler runs in the guest, and how many tokens.
pub const GUEST_PROMPT: &str = "What is the capital of France?";
pub const GUEST_TOKENS: usize = 8;

pub fn model_path(root: &Path) -> PathBuf {
    root.join("tools").join("models").join(MODEL_FILE)
}

/// Reads the model and checks it against the pinned SHA-256.
pub fn load_model(root: &Path) -> Result<Vec<u8>, String> {
    use sha2::{Digest, Sha256};
    let path = model_path(root);
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
    let prompt_ids = tok.encode(&chat::prompt(system, user));
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
/// name the same tokens and agree within `LOGIT_TOLERANCE`.
pub fn check(root: &Path, max_new: usize) -> Result<bool, String> {
    const LOGIT_TOLERANCE: f32 = 0.02;
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
        let ok = same_prompt && same_out && ref_top.len() == 5 && max_diff <= LOGIT_TOLERANCE;
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

/// Requests for `assist-check` with the tool and argument the assistant
/// must choose (docs/m5c2-design.md §5).
/// Several arguments are accepted where they mean the same to the broker.
pub const ASSIST_CASES: [(&str, &str, &[&str]); 8] = [
    (
        "Find my pancake recipe",
        "search",
        &["pancake", "pancake recipe"],
    ),
    ("Search for files that mention flour", "search", &["flour"]),
    ("What is in my Documents folder?", "list", &["Documents"]),
    ("Show me what's inside Music", "list", &["Music"]),
    (
        "How big is Documents/notes.txt?",
        "inspect",
        &["Documents/notes.txt"],
    ),
    (
        "Make a folder called Projects/2026",
        "create",
        &["Projects/2026"],
    ),
    (
        "Create a new folder named Invoices",
        "create",
        &["Invoices"],
    ),
    (
        "Please create the folder Photos/Holidays",
        "create",
        &["Photos/Holidays"],
    ),
];

/// Runs the guided tool choice on `ASSIST_CASES` with the host engine.
pub fn assist_check(root: &Path, extra: Option<&str>) -> Result<bool, String> {
    use zoo_infer::assist::Bella;
    let data = load_model(root)?;
    let g = Gguf::parse(&data).map_err(|e| format!("GGUF: {e:?}"))?;
    let tok = Tokenizer::from_gguf(&g).map_err(|e| format!("tokenizer: {e:?}"))?;
    let model = Model::from_gguf(&g).map_err(|e| format!("model: {e:?}"))?;
    let t0 = Instant::now();
    let mut a = Bella::new(&model, &tok, |_| true).map_err(|e| format!("{e:?}"))?;
    println!(
        "system prompt: {} tokens in {:.1}s",
        a.prefix_tokens(),
        t0.elapsed().as_secs_f64()
    );
    if let Some(req) = extra {
        let c = a.choose(req, |_| true).map_err(|e| format!("{e:?}"))?;
        println!(
            "{req:?} -> {} {:?} (margin {:.2})",
            c.tool.word(),
            c.arg,
            c.margin
        );
        return Ok(true);
    }
    let mut ok = true;
    for (req, tool, arg) in ASSIST_CASES {
        let t = Instant::now();
        let c = a.choose(req, |_| true).map_err(|e| format!("{e:?}"))?;
        let good = c.tool.word() == tool && arg.contains(&c.arg.as_str());
        ok &= good;
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
                format!("; expected {tool} {arg:?}")
            }
        );
    }
    Ok(ok)
}
