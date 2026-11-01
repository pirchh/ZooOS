//! Pure serial-log evaluation: decides pass / fail / still-pending from the
//! guest's COM1 output. Kept free of I/O so it can be unit tested.

use std::fmt;

pub const PREFIX: &str = "ZOO_OS ";
const INFO: &str = "ZOO_OS INFO ";
const INFO_MODE: &str = "ZOO_OS INFO mode=";

#[derive(Clone, Debug)]
pub enum Matcher {
    Exact(String),
    Prefix(String),
}

impl Matcher {
    fn matches(&self, line: &str) -> bool {
        match self {
            Matcher::Exact(s) => line == s,
            Matcher::Prefix(p) => line.starts_with(p.as_str()),
        }
    }
}

impl fmt::Display for Matcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Matcher::Exact(s) => write!(f, "{s}"),
            Matcher::Prefix(p) => write!(f, "{p}…"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Expectation {
    /// Markers that must appear, in this order, with no other marker between.
    pub required: Vec<Step>,
}

/// One position in the expected sequence.
#[derive(Clone, Debug)]
pub enum Step {
    One(Matcher),
    /// All of these, each exactly once, in any order (events from separate
    /// processes that may interleave), before the next step.
    AnyOrder(Vec<Matcher>),
    /// Any further markers except a kernel panic or exception: for a
    /// program whose process tree is not a fixed sequence (Wine starting
    /// its Windows services). Must be last; the case's interaction steps
    /// say what has to happen.
    Rest,
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Step::One(m) => write!(f, "{m}"),
            Step::AnyOrder(ms) => {
                write!(f, "any order of [")?;
                for (i, m) in ms.iter().enumerate() {
                    write!(f, "{}`{m}`", if i == 0 { "" } else { ", " })?;
                }
                write!(f, "]")
            }
            Step::Rest => write!(f, "any markers but PANIC or EXCEPTION"),
        }
    }
}

impl Expectation {
    /// `BOOT version=…`, then `INFO mode=<kernel_mode> …`, then each line in
    /// `tail` exactly, in order.
    pub fn new(version: &str, kernel_mode: &str, tail: &[&str]) -> Self {
        let mut required = vec![
            Step::One(Matcher::Exact(format!("ZOO_OS BOOT version={version}"))),
            Step::One(Matcher::Prefix(format!("ZOO_OS INFO mode={kernel_mode} "))),
        ];
        required.extend(
            tail.iter()
                .map(|t| Step::One(Matcher::Exact(t.to_string()))),
        );
        Expectation { required }
    }

    /// Appends exact markers that must follow in order.
    pub fn then(&mut self, lines: &[&str]) -> &mut Self {
        self.required.extend(
            lines
                .iter()
                .map(|t| Step::One(Matcher::Exact(t.to_string()))),
        );
        self
    }

    /// Appends one marker that must start with `prefix` (for lines whose
    /// end varies, such as a count).
    pub fn then_prefix(&mut self, prefix: &str) -> &mut Self {
        self.required
            .push(Step::One(Matcher::Prefix(prefix.to_string())));
        self
    }

    /// Ends the sequence with `Step::Rest`.
    pub fn then_rest(&mut self) -> &mut Self {
        self.required.push(Step::Rest);
        self
    }

    /// Appends exact markers that must all follow, in any order.
    pub fn then_any_order(&mut self, lines: &[&str]) -> &mut Self {
        self.required.push(Step::AnyOrder(
            lines
                .iter()
                .map(|t| Matcher::Exact(t.to_string()))
                .collect(),
        ));
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// All required markers seen, nothing unexpected.
    Pass,
    /// Waiting for this marker.
    Pending(String),
    /// A wrong, out-of-order, duplicate, or trailing marker was seen.
    Unexpected(String),
}

/// Evaluates complete lines only (a partially written last line is ignored).
///
/// Every line containing `ZOO_OS ` is a marker, except `INFO` lines that
/// do not carry `mode=`. Each marker must match the next required one; any
/// other marker (e.g. PANIC in normal mode, READY in panic mode, or anything
/// after the final marker) fails immediately.
pub fn evaluate(exp: &Expectation, log: &str) -> Verdict {
    let complete = match log.rfind('\n') {
        Some(i) => &log[..=i],
        None => "",
    };
    let mut next = 0;
    // Members of the current `AnyOrder` step already matched.
    let mut used: Vec<bool> = Vec::new();
    for raw in complete.lines() {
        // Firmware may leave escape sequences on the same line before kernel
        // output, so look for the marker anywhere in the line.
        let Some(pos) = raw.find(PREFIX) else {
            continue;
        };
        let marker = raw[pos..].trim_end();
        if marker.starts_with(INFO) && !marker.starts_with(INFO_MODE) {
            continue;
        }
        match exp.required.get(next) {
            Some(Step::Rest) => {
                if marker.starts_with("ZOO_OS PANIC") || marker.starts_with("ZOO_OS EXCEPTION") {
                    return Verdict::Unexpected(format!(
                        "got `{marker}` after the expected markers"
                    ));
                }
            }
            Some(Step::One(m)) if m.matches(marker) => next += 1,
            Some(Step::AnyOrder(ms)) => {
                if used.len() != ms.len() {
                    used = vec![false; ms.len()];
                }
                let Some(j) = (0..ms.len()).find(|&j| !used[j] && ms[j].matches(marker)) else {
                    return Verdict::Unexpected(format!(
                        "got `{marker}` while expecting {}",
                        exp.required[next]
                    ));
                };
                used[j] = true;
                if used.iter().all(|&u| u) {
                    used.clear();
                    next += 1;
                }
            }
            Some(s) => return Verdict::Unexpected(format!("got `{marker}` while expecting `{s}`")),
            None => {
                return Verdict::Unexpected(format!(
                    "got `{marker}` after the final expected marker"
                ));
            }
        }
    }
    match exp.required.get(next) {
        None | Some(Step::Rest) => Verdict::Pass,
        Some(s) => Verdict::Pending(s.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: &str = "0.1.0";

    // M1-shaped expectations keep these tests short; M2 cases only add
    // lines to the tail, which `new` handles the same way.
    fn normal(v: &str) -> Expectation {
        Expectation::new(v, "normal", &["ZOO_OS READY"])
    }

    fn panic(v: &str) -> Expectation {
        Expectation::new(v, "panic-test", &["ZOO_OS PANIC intentional-test"])
    }
    const NORMAL_OK: &str = "BdsDxe: loading\r\nZOO_OS BOOT version=0.1.0\r\n\
        ZOO_OS INFO mode=normal memory_regions=9 usable_bytes=1\r\nZOO_OS READY\r\n";
    const PANIC_OK: &str = "ZOO_OS BOOT version=0.1.0\r\n\
        ZOO_OS INFO mode=panic-test memory_regions=9 usable_bytes=1\r\n\
        ZOO_OS PANIC intentional-test\r\nZOO_OS INFO panic_location=kernel/src/main.rs:40\r\n";

    #[test]
    fn rest_accepts_more_but_not_a_panic() {
        let mut e = normal(V);
        e.then_rest();
        assert_eq!(evaluate(&e, NORMAL_OK), Verdict::Pass);
        let more = format!(
            "{NORMAL_OK}ZOO_OS PROC start pid=10 name=wine
"
        );
        assert_eq!(evaluate(&e, &more), Verdict::Pass);
        let bad = format!(
            "{more}ZOO_OS EXCEPTION page-fault
"
        );
        assert!(matches!(evaluate(&e, &bad), Verdict::Unexpected(_)));
        // Without it, the same extra marker fails.
        assert!(matches!(
            evaluate(&normal(V), &more),
            Verdict::Unexpected(_)
        ));
    }

    #[test]
    fn normal_passes() {
        assert_eq!(evaluate(&normal(V), NORMAL_OK), Verdict::Pass);
    }

    #[test]
    fn panic_passes_and_ignores_location_info() {
        assert_eq!(evaluate(&panic(V), PANIC_OK), Verdict::Pass);
    }

    #[test]
    fn missing_ready_is_pending() {
        let log = "ZOO_OS BOOT version=0.1.0\nZOO_OS INFO mode=normal x=1\n";
        assert_eq!(
            evaluate(&normal(V), log),
            Verdict::Pending("ZOO_OS READY".into())
        );
    }

    #[test]
    fn empty_log_is_pending() {
        assert!(matches!(evaluate(&normal(V), ""), Verdict::Pending(_)));
    }

    #[test]
    fn partial_last_line_is_not_counted() {
        let log = "ZOO_OS BOOT version=0.1.0\nZOO_OS INFO mode=normal x=1\nZOO_OS READY";
        assert!(matches!(evaluate(&normal(V), log), Verdict::Pending(_)));
    }

    #[test]
    fn panic_output_fails_normal_expectation() {
        assert!(matches!(
            evaluate(&normal(V), PANIC_OK),
            Verdict::Unexpected(_)
        ));
    }

    #[test]
    fn panic_in_normal_mode_fails() {
        let log = "ZOO_OS BOOT version=0.1.0\nZOO_OS INFO mode=normal x=1\n\
            ZOO_OS PANIC something\n";
        assert!(matches!(evaluate(&normal(V), log), Verdict::Unexpected(_)));
    }

    #[test]
    fn host_text_without_marker_lines_is_pending() {
        // Text that merely mentions markers mid-sentence must still be a full
        // marker from that point on to count; anything else is ignored.
        let log = "INFO : Jumping to kernel entry point\nnot a marker\n";
        assert!(matches!(evaluate(&normal(V), log), Verdict::Pending(_)));
    }

    #[test]
    fn ready_fails_panic_expectation() {
        assert!(matches!(
            evaluate(&panic(V), NORMAL_OK),
            Verdict::Unexpected(_)
        ));
    }

    #[test]
    fn ready_after_panic_fails() {
        let log = format!("{PANIC_OK}ZOO_OS READY\n");
        assert!(matches!(evaluate(&panic(V), &log), Verdict::Unexpected(_)));
    }

    #[test]
    fn out_of_order_fails() {
        let log = "ZOO_OS READY\nZOO_OS BOOT version=0.1.0\n";
        assert!(matches!(evaluate(&normal(V), log), Verdict::Unexpected(_)));
    }

    #[test]
    fn wrong_version_fails() {
        let log = "ZOO_OS BOOT version=9.9.9\n";
        assert!(matches!(evaluate(&normal(V), log), Verdict::Unexpected(_)));
    }

    #[test]
    fn duplicate_ready_fails() {
        let log = format!("{NORMAL_OK}ZOO_OS READY\n");
        assert!(matches!(evaluate(&normal(V), &log), Verdict::Unexpected(_)));
    }

    #[test]
    fn marker_after_escape_sequence_is_found() {
        let log = "\x1b[2J\x1b[01;01HZOO_OS BOOT version=0.1.0\r\n";
        assert_eq!(
            evaluate(&normal(V), log),
            Verdict::Pending("ZOO_OS INFO mode=normal …".into())
        );
    }

    #[test]
    fn fault_marker_must_match_exactly() {
        let exp = Expectation::new(
            V,
            "fault-heap-guard",
            &["ZOO_OS EXCEPTION page-fault access=read cause=not-present addr=0xffffc00000101000"],
        );
        let head = "ZOO_OS BOOT version=0.1.0
ZOO_OS INFO mode=fault-heap-guard x=1
";
        let good = format!(
            "{head}ZOO_OS EXCEPTION page-fault access=read cause=not-present addr=0xffffc00000101000
             ZOO_OS INFO exception rip=0x1 rsp=0x2 error=0x0
"
        );
        assert_eq!(evaluate(&exp, &good), Verdict::Pass);
        let wrong_addr = format!(
            "{head}ZOO_OS EXCEPTION page-fault access=read cause=not-present addr=0x0000000000000000
"
        );
        assert!(matches!(
            evaluate(&exp, &wrong_addr),
            Verdict::Unexpected(_)
        ));
    }

    #[test]
    fn any_order_group_accepts_permutations_only() {
        let mut exp = Expectation::new(V, "normal", &["ZOO_OS A"]);
        exp.then_any_order(&["ZOO_OS X", "ZOO_OS Y"])
            .then(&["ZOO_OS Z"]);
        let head = "ZOO_OS BOOT version=0.1.0\nZOO_OS INFO mode=normal x=1\nZOO_OS A\n";
        for order in ["ZOO_OS X\nZOO_OS Y\n", "ZOO_OS Y\nZOO_OS X\n"] {
            let log = format!("{head}{order}ZOO_OS Z\n");
            assert_eq!(evaluate(&exp, &log), Verdict::Pass, "{order}");
        }
        // Z before the group is complete, a duplicate, or a stranger: rejected.
        for bad in [
            "ZOO_OS X\nZOO_OS Z\n",
            "ZOO_OS X\nZOO_OS X\n",
            "ZOO_OS X\nZOO_OS W\n",
        ] {
            let log = format!("{head}{bad}");
            assert!(
                matches!(evaluate(&exp, &log), Verdict::Unexpected(_)),
                "{bad}"
            );
        }
        let partial = format!("{head}ZOO_OS Y\n");
        assert!(matches!(evaluate(&exp, &partial), Verdict::Pending(_)));
    }

    #[test]
    fn self_test_failure_panic_is_rejected() {
        let exp = Expectation::new(V, "normal", &["ZOO_OS TEST heap ok", "ZOO_OS READY"]);
        let log = "ZOO_OS BOOT version=0.1.0
ZOO_OS INFO mode=normal x=1
            ZOO_OS PANIC self-test heap: Vec sum 1 wrong
";
        assert!(matches!(evaluate(&exp, log), Verdict::Unexpected(_)));
    }
}
