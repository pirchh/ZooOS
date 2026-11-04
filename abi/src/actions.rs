//! Application actions (docs/actions-design.md, Bella roadmap Stage 4): what
//! an application declares it can do for another, Bella in practice.
//!
//! An invocation is `do:<program>:<action>:<argument>` from the asking
//! application to the window server, which checks it against this
//! manifest, opens or raises the target, and passes `do:<action>:<argument>`
//! on. The target answers `did:<sentence>`, which the window server hands
//! back to the asker as `window-done:<sentence>`.

/// What an action's argument is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg {
    None,
    /// Free text, at most `MAX_ARG` bytes.
    Text,
}

/// What an action does to the user's things: the window server refuses
/// `Delete` actions until they can ask first (none is declared yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Shows or fetches; changes nothing.
    Read,
    /// Adds to or changes the user's files.
    Change,
    /// Removes something.
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    /// The program that performs it (as in the window server's catalogue).
    pub program: &'static str,
    pub name: &'static str,
    pub arg: Arg,
    pub effect: Effect,
    /// What it does, for people and logs.
    pub about: &'static str,
}

/// Longest argument.
pub const MAX_ARG: usize = 400;

/// Every application action.
pub const ACTIONS: [Action; 2] = [
    Action {
        program: "notes",
        name: "write",
        arg: Arg::Text,
        effect: Effect::Change,
        about: "add a line of text at the end of the open note and save it",
    },
    Action {
        program: "network",
        name: "fetch",
        arg: Arg::Text,
        effect: Effect::Read,
        about: "fetch a web page by name or address (plain HTTP) and show its text",
    },
];

/// `program`'s action `name`, if it declares one.
pub fn find(program: &str, name: &str) -> Option<&'static Action> {
    ACTIONS
        .iter()
        .find(|a| a.program == program && a.name == name)
}

/// An invocation's parts: `<program>:<action>:<argument>` (the text after
/// `do:`), checked against the manifest: the action exists, its argument
/// is given when it takes one and not too long.
pub fn parse(rest: &[u8]) -> Option<(&'static Action, &str)> {
    let rest = core::str::from_utf8(rest).ok()?;
    let (program, rest) = rest.split_once(':')?;
    let (name, arg) = rest.split_once(':').unwrap_or((rest, ""));
    let a = find(program, name)?;
    let arg = arg.trim();
    match a.arg {
        Arg::None if !arg.is_empty() => None,
        Arg::Text if arg.is_empty() || arg.len() > MAX_ARG => None,
        _ => Some((a, arg)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocations() {
        let (a, arg) = parse(b"notes:write:buy milk").unwrap();
        assert_eq!((a.program, a.name, arg), ("notes", "write", "buy milk"));
        assert_eq!(
            parse(b"network:fetch:example.com").unwrap().1,
            "example.com"
        );
        // Unknown, or no argument where one is needed.
        assert!(parse(b"notes:delete:x").is_none());
        assert!(parse(b"files:write:x").is_none());
        assert!(parse(b"notes:write:").is_none());
        assert!(parse(b"notes").is_none());
    }
}
