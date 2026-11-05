//! The path one request takes in Bella before the model
//! (docs/regression-design.md): read from the words alone, so Bella and the
//! host's `bella-regress` take the same path for the same sentence.

use crate::layout::Node;
use crate::registry::Registry;
use alloc::string::String;
use alloc::vec::Vec;
use zoo_abi::bella;

/// Where a request goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Route<'a> {
    /// A typed command ("open notes", "write bread and butter").
    Typed,
    /// An app's action in its registered words ("add milk to my
    /// notes"): the command, as the model would give it
    /// (`Registry::phrased`).
    Phrased(String),
    /// A layout read from the words (docs/layouts-design.md).
    Layout(Node),
    /// Save the last summary to a file.
    SaveSummary,
    /// A question about the user's files, answered from the index.
    MyFiles,
    /// Summarise this file.
    Summarise(&'a str),
    /// What is on the screen (docs/context-design.md).
    Activity,
    /// A fact to remember (docs/facts-design.md).
    Remember(&'a str),
    /// What Bella changed.
    WhatDid,
    /// What the user told her.
    WhatRemembered,
    /// A fact to forget, asked about first.
    Forget(&'a str),
    /// A question about her last answer (docs/talk-design.md §6).
    AboutAnswer,
    /// Small talk, answered in words.
    SmallTalk,
    /// A fact the user told her answers it, else the model chooses (or
    /// memory, for a phrasing she learned). Facts are asked of the facts
    /// service, so Bella checks them here, not `route`.
    Model,
}

/// What Bella knows that changes the path: none of it is in the words.
#[derive(Debug, Clone, Copy, Default)]
pub struct Known {
    /// She is trying a layout again after a no.
    pub relayout: bool,
    /// She made a summary that could be saved.
    pub summary: bool,
    /// She answered something that a follow-up could be about.
    pub exchange: bool,
}

/// The path for one step of a request (its address already taken off).
pub fn route<'a>(text: &'a str, reg: &Registry, known: Known) -> Route<'a> {
    if bella::typed_command_with(text, |w| reg.find_word(w).is_some()) {
        return Route::Typed;
    }
    if known.relayout || crate::assist::arranges(text) {
        let named = crate::assist::split_request(text).unwrap_or_default();
        let apps: Vec<&str> = named.split(" and ").filter(|a| !a.is_empty()).collect();
        if let Some(tree) = crate::layout::from_words(text, &apps) {
            return Route::Layout(tree);
        }
    }
    if bella::wants_saved(text) && known.summary {
        Route::SaveSummary
    } else if bella::asks_my_files(text) {
        Route::MyFiles
    } else if let Some(path) = bella::file_to_summarise(text) {
        Route::Summarise(path)
    } else if bella::asks_about_activity(text) {
        Route::Activity
    } else if let Some(fact) = bella::fact_to_remember(text) {
        Route::Remember(fact)
    } else if bella::asks_what_bella_did(text) {
        Route::WhatDid
    } else if bella::asks_what_remembered(text) {
        Route::WhatRemembered
    } else if let Some(about) = bella::fact_to_forget(text) {
        Route::Forget(about)
    } else if bella::question_about_answer(text) && known.exchange {
        Route::AboutAnswer
    } else if bella::small_talk(text) {
        Route::SmallTalk
    } else if let Some(cmd) = reg.phrased(text) {
        // Last before the model: every path above keeps its sentences.
        Route::Phrased(cmd)
    } else {
        Route::Model
    }
}

/// The model's margin (in hundredths) below which Bella asks before a
/// choice that changes things (docs/unsure-design.md). Measured on
/// `assist-check` (Qwen3 1.7B, 2026-11-04): right choices that close,
/// move, remove or change were 12.97 and up; "What time is it?" ->
/// `close Network` was 0.67.
pub const UNSURE_MARGIN: i32 = 500;

/// Whether the model's choice of `word` at `margin` (hundredths) is
/// asked about first ("Do you want me to close Network?"): it closes or
/// moves something, or is an app action that changes things, and the
/// model was unsure. Deletes, `trash` and `create` ask anyway; reading,
/// opening and arranging are easily undone and never ask.
pub fn ask_first(word: &str, margin: i32, reg: &Registry) -> bool {
    use crate::registry::Effect;
    let changes = matches!(word, "close" | "move")
        || reg
            .find_word(word)
            .is_some_and(|a| a.effect == Effect::Change);
    changes && margin < UNSURE_MARGIN
}

/// How a whole request starts (docs/regression-design.md): not to be done,
/// several steps, or one request; its address ("hey Bella") taken off.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// "Don't ...": nothing is done (docs/targets-design.md).
    Negated,
    /// Several steps, each routed on its own (docs/multi-step-design.md).
    Steps(Vec<String>),
    /// One request.
    One(String),
}

/// The plan for `text`, said or typed. Bella resolves "it" from the
/// desktop's events between taking the address off and splitting steps;
/// `resolve` does that (the identity on the host).
pub fn plan(text: &str, resolve: impl FnOnce(&str) -> String) -> Plan {
    let stripped = bella::strip_address(text);
    if bella::negated(stripped) {
        return Plan::Negated;
    }
    let resolved = resolve(stripped);
    let steps = crate::steps::split_steps(&resolved);
    if steps.is_empty() {
        Plan::One(resolved)
    } else {
        Plan::Steps(steps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> Registry {
        let mut r = Registry::default();
        let mut skipped = Vec::new();
        r.add_file(
            "app notes\n\naction write\n  word write\n  about write text down in Notes\n  effect change\n  slots text\n",
            &mut skipped,
        );
        assert!(skipped.is_empty(), "{skipped:?}");
        r
    }

    #[test]
    fn paths() {
        let r = reg();
        let k = Known::default();
        assert_eq!(route("open notes", &r, k), Route::Typed);
        assert_eq!(route("write bread and butter", &r, k), Route::Typed);
        assert!(matches!(
            route("split the screen into quarters", &r, k),
            Route::Layout(_)
        ));
        assert_eq!(route("maximize my notes", &r, k), Route::Model);
    }

    #[test]
    fn unsure() {
        let mut r = reg();
        let mut skipped = Vec::new();
        r.add_file(
            "app notes\n\naction remove\n  word remove\n  about take a line out\n  effect delete\n  slots text\n",
            &mut skipped,
        );
        assert!(
            ask_first("close", 67, &r),
            "What time is it? -> close Network"
        );
        assert!(!ask_first("close", 2827, &r), "Quit Notes -> close Notes");
        assert!(ask_first("write", 120, &r));
        assert!(!ask_first("write", 805, &r));
        // Deletes ask anyway; undoable ones never.
        assert!(!ask_first("remove", 10, &r));
        assert!(!ask_first("minimize", 359, &r));
        assert!(!ask_first("open", 0, &r));
    }

    #[test]
    fn plans() {
        let id = |s: &str| String::from(s);
        assert_eq!(plan("Don't delete the milk entry", id), Plan::Negated);
        assert_eq!(plan("Hey Bella, never mind", id), Plan::Negated);
        assert!(matches!(
            plan("Hey Bella, open notes and maximize it", id),
            Plan::Steps(ref s) if s.len() == 2
        ));
        assert_eq!(
            plan("Hey Bella, maximize my notes", id),
            Plan::One(String::from("maximize my notes"))
        );
    }
}
