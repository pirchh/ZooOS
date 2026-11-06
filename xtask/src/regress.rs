//! `cargo xtask bella-regress` (docs/regression-design.md): fixed sentences,
//! as speech to text wrote them in the owner's demos or as typed, through
//! Bella's own path on the host: BatHandler's gate (`zoo_abi::bella`), the
//! plan and route (`zoo_infer::route`), and the model's choice for what
//! reaches it. Each sentence's result is compared with the recorded
//! baseline: a change fails until it is looked at and accepted.

use crate::infer::{load_model, model_file, registry};
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;
use zoo_infer::gguf::Gguf;
use zoo_infer::model::Model;
use zoo_infer::route::{Known, Plan, Route};
use zoo_infer::tokenizer::Tokenizer;

/// The sentences, one per line (see the file's header).
pub const REQUESTS: &str = "tests/bella/requests.txt";

/// One sentence: how it arrived and what it should do.
struct Case {
    /// `said` (through BatHandler's gate) or `typed` (in her window).
    said: bool,
    text: String,
    /// What it should do, when Bella does not do it yet: shown as a known
    /// gap, not a failure (the baseline is what she does).
    want: Option<String>,
}

fn read_cases(root: &Path) -> Result<Vec<Case>, String> {
    let path = root.join(REQUESTS);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut cases = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (kind, rest) = line
            .split_once(':')
            .ok_or_else(|| format!("{REQUESTS}:{}: no `said:` or `typed:`", n + 1))?;
        let said = match kind.trim() {
            "said" => true,
            "typed" => false,
            k => return Err(format!("{REQUESTS}:{}: unknown kind `{k}`", n + 1)),
        };
        let (text, want) = match rest.split_once(" => want ") {
            Some((t, w)) => (t.trim(), Some(w.trim().to_string())),
            None => (rest.trim(), None),
        };
        cases.push(Case {
            said,
            text: text.to_string(),
            want,
        });
    }
    Ok(cases)
}

/// The baseline for the model in use: results differ between models.
fn baseline_path(root: &Path) -> std::path::PathBuf {
    let stem = model_file().trim_end_matches(".gguf");
    root.join("tests/bella")
        .join(format!("baseline-{stem}.txt"))
}

/// Runs every sentence; with `accept`, records the results as the new
/// baseline. Fails on any result that differs from the baseline.
pub fn bella_regress(root: &Path, accept: bool) -> Result<bool, String> {
    use zoo_infer::assist::Bella;
    let cases = read_cases(root)?;
    let data = load_model(root)?;
    let g = Gguf::parse(&data).map_err(|e| format!("GGUF: {e:?}"))?;
    let tok = Tokenizer::from_gguf(&g).map_err(|e| format!("tokenizer: {e:?}"))?;
    let model = Model::from_gguf(&g).map_err(|e| format!("model: {e:?}"))?;
    let reg = registry(root)?;
    let mut bella =
        Bella::new(&model, &tok, registry(root)?, |_| true).map_err(|e| format!("{e:?}"))?;
    println!(
        "model {}, engine {}, {} sentences",
        model_file(),
        zoo_infer::cpu::backend(),
        cases.len()
    );
    let t0 = Instant::now();
    let mut results = Vec::new();
    for c in &cases {
        let r = result(c, &reg, &mut bella)?;
        results.push((c, r));
    }
    let baseline_file = baseline_path(root);
    let old = std::fs::read_to_string(&baseline_file).unwrap_or_default();
    let old: std::collections::HashMap<&str, &str> = old
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once(" => "))
        .collect();
    let (mut same, mut changed, mut new, mut gaps) = (0, 0, 0, 0);
    for (c, r) in &results {
        let key = key(c);
        let mark = match old.get(key.as_str()) {
            Some(b) if *b == r => {
                same += 1;
                "same"
            }
            Some(b) => {
                changed += 1;
                println!("CHANGED {key}\n   was {b}\n   now {r}");
                continue;
            }
            None => {
                new += 1;
                "NEW "
            }
        };
        let gap = c.want.as_ref().filter(|w| !r.contains(w.as_str()));
        gaps += usize::from(gap.is_some());
        println!(
            "{mark} {key} => {r}{}",
            gap.map_or(String::new(), |w| format!("\n      gap: want {w}"))
        );
    }
    println!(
        "{same} as recorded, {changed} changed, {new} new; {gaps} known gaps ({:.0}s)",
        t0.elapsed().as_secs_f64()
    );
    if accept {
        let mut out = format!(
            "# bella-regress baseline for {} (docs/regression-design.md).\n\
             # Written by `cargo xtask bella-regress --accept`; a change to a\n\
             # line is a change in what Bella does with that sentence.\n",
            model_file()
        );
        for (c, r) in &results {
            let _ = writeln!(out, "{} => {r}", key(c));
        }
        std::fs::write(&baseline_file, out)
            .map_err(|e| format!("cannot write {}: {e}", baseline_file.display()))?;
        println!("recorded {}", baseline_file.display());
        return Ok(true);
    }
    if old.is_empty() {
        println!(
            "no baseline at {}; record one with --accept",
            baseline_file.display()
        );
        return Ok(false);
    }
    Ok(changed == 0 && new == 0)
}

fn key(c: &Case) -> String {
    format!("{} {}", if c.said { "said" } else { "typed" }, c.text)
}

/// What Bella does with one sentence, as one line.
fn result(
    c: &Case,
    reg: &zoo_infer::registry::Registry,
    bella: &mut zoo_infer::assist::Bella,
) -> Result<String, String> {
    let mut out = String::new();
    let mut text = c.text.as_str();
    if c.said {
        // BatHandler (user/src/bin/bat-handler.rs): its gate, then Bella
        // takes off the mark speech to text ends a sentence with.
        if zoo_abi::bella::name_only(text) {
            return Ok("her name only; the next sentence is the request".into());
        }
        if zoo_abi::bella::addressed(text).is_none() {
            return Ok("not for her".into());
        }
        text = text.trim().trim_end_matches(['.', '!', ',', ';']);
        out.push_str("for her; ");
    }
    let steps = match zoo_infer::route::plan(text, |t: &str| t.to_string()) {
        Plan::Negated => {
            out.push_str("nothing done (asked not to)");
            return Ok(out);
        }
        Plan::One(t) => vec![t],
        Plan::Routine(name, steps) => {
            let _ = write!(out, "routine {name:?} asked: {}", steps.join(" | "));
            return Ok(out);
        }
        Plan::SaveAs(name) => {
            let _ = write!(out, "the last request saved as {name:?}, asked");
            return Ok(out);
        }
        Plan::Steps(s) => {
            let _ = write!(out, "{} steps; ", s.len());
            s
        }
    };
    let mut last = String::new();
    for (k, step) in steps.iter().enumerate() {
        // As Bella's `next_step`: "it" names the apps of the step before.
        let step = zoo_infer::assist::resolve_it(step, &last);
        if k > 0 {
            out.push_str("; ");
        }
        if steps.len() > 1 {
            let _ = write!(out, "[{step}] ");
        }
        match zoo_infer::route::route(&step, reg, Known::default()) {
            Route::Model => {
                let ch = bella
                    .choose(&step, |_| true)
                    .map_err(|e| format!("{e:?}"))?;
                if ch.word.as_str() == "none" {
                    out.push_str("model: no command (answered in words)");
                } else {
                    let arg = ch.arg.replace(zoo_infer::registry::SLOT_SEP, " / ");
                    let _ = write!(out, "model: {} {arg:?}", ch.word.as_str());
                    // As Bella (docs/unsure-design.md): margins in hundredths.
                    let margin = (ch.margin * 100.0) as i32;
                    if zoo_infer::route::vague(ch.word.as_str(), &ch.arg) {
                        out.push_str(", names nothing (answered in words)");
                    } else if zoo_infer::route::ask_first(ch.word.as_str(), margin, reg) {
                        out.push_str(", asked first (unsure)");
                    }
                }
            }
            Route::Typed => {
                // An app's action fills its slots from the words, as
                // Bella's `app_action` does; the others run as typed.
                let first = step.split_whitespace().next().unwrap_or("");
                match reg.find_word(first) {
                    Some(a) => {
                        let slots = a.fill(&step).replace(zoo_infer::registry::SLOT_SEP, " / ");
                        let _ = write!(out, "typed: {} {} {slots:?}", a.program, a.name);
                    }
                    None => out.push_str("typed command"),
                }
            }
            Route::Phrased(cmd) => {
                let (word, slots) = cmd.split_once(' ').unwrap_or((&cmd, ""));
                let slots = slots.replace(zoo_infer::registry::SLOT_SEP, " / ");
                let _ = write!(out, "registry words: {word} {slots:?}");
            }
            Route::Sides(cmd) => {
                let _ = write!(out, "left and right: {cmd}");
            }
            Route::Layout(tree) => {
                let _ = write!(out, "layout {}", tree.text());
            }
            other => {
                let _ = write!(out, "{other:?}");
            }
        }
        last = step;
    }
    Ok(out)
}

/// Labelled speech after her answer (docs/addressee-design.md).
pub const ADDRESSEE: &str = "tests/bella/addressee.txt";

/// `cargo xtask bella-addressee`: the judge on each sentence, beside
/// today's gate (`needs_no_model`). Fails when talk not said to her is
/// taken at `zoo_infer::addressee::THRESHOLD`.
pub fn bella_addressee(root: &Path) -> Result<bool, String> {
    use zoo_infer::addressee::{Judge, THRESHOLD};
    let path = root.join(ADDRESSEE);
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut cases = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('|').map(str::trim).collect();
        let [label, answer, heard] = parts[..] else {
            return Err(format!("{ADDRESSEE}:{}: want `label | answer | heard`", n + 1));
        };
        let for_her = match label {
            "for" => true,
            "not" => false,
            l => return Err(format!("{ADDRESSEE}:{}: unknown label `{l}`", n + 1)),
        };
        cases.push((for_her, answer.to_string(), heard.to_string()));
    }
    let data = load_model(root)?;
    let g = Gguf::parse(&data).map_err(|e| format!("GGUF: {e:?}"))?;
    let tok = Tokenizer::from_gguf(&g).map_err(|e| format!("tokenizer: {e:?}"))?;
    let model = Model::from_gguf(&g).map_err(|e| format!("model: {e:?}"))?;
    let reg = registry(root)?;
    let t = Instant::now();
    let mut judge = Judge::new(&model, &tok).map_err(|e| format!("{e:?}"))?;
    println!(
        "model {}, engine {}, {} sentences, threshold {THRESHOLD}; system prompt in {:.2}s",
        model_file(),
        zoo_infer::cpu::backend(),
        cases.len(),
        t.elapsed().as_secs_f64()
    );
    let (mut gate_for, mut judge_for, mut judge_not, mut fors, mut nots) = (0, 0, 0, 0, 0);
    let mut times = Vec::new();
    let mut margins = Vec::new();
    for (for_her, answer, heard) in &cases {
        let gate = zoo_infer::route::needs_no_model(heard.trim_end_matches(['.', '!', '?']), &reg);
        let t = Instant::now();
        let v = judge.judge(answer, heard).map_err(|e| format!("{e:?}"))?;
        times.push(t.elapsed().as_secs_f64());
        let took = v.for_her(THRESHOLD);
        margins.push((*for_her, v.margin));
        if *for_her {
            fors += 1;
            gate_for += usize::from(gate);
            judge_for += usize::from(took || gate);
        } else {
            nots += 1;
            judge_not += usize::from(took || gate);
        }
        let mark = match (*for_her, took || gate) {
            (true, true) | (false, false) => "ok  ",
            (true, false) => "MISS",
            (false, true) => "TOOK",
        };
        println!(
            "{mark} {} {:>6.2} {:>3} tok  {heard}",
            if *for_her { "for" } else { "not" },
            v.margin,
            v.tokens
        );
    }
    times.sort_by(f64::total_cmp);
    let lowest_for = margins
        .iter()
        .filter(|m| m.0)
        .map(|m| m.1)
        .fold(f32::INFINITY, f32::min);
    let highest_not = margins
        .iter()
        .filter(|m| !m.0)
        .map(|m| m.1)
        .fold(f32::NEG_INFINITY, f32::max);
    println!(
        "said to her: {gate_for} of {fors} taken by today's gate, {judge_for} of {fors} with the judge"
    );
    println!("not said to her: {judge_not} of {nots} taken (must be 0)");
    println!(
        "margins: lowest for her {lowest_for:.2}, highest not for her {highest_not:.2}; \
         judging took {:.0}-{:.0} ms (median {:.0} ms)",
        times[0] * 1000.0,
        times[times.len() - 1] * 1000.0,
        times[times.len() / 2] * 1000.0
    );
    Ok(judge_not == 0)
}
