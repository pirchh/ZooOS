//! Pure serial-log evaluation: decides pass / fail / still-pending from the
//! guest's COM1 output. Kept free of I/O so it can be unit tested.

use std::fmt;

pub const PREFIX: &str = "ORIGINS_OS ";
const INFO: &str = "ORIGINS_OS INFO ";
const INFO_MODE: &str = "ORIGINS_OS INFO mode=";

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
    pub required: Vec<Matcher>,
}

impl Expectation {
    /// `BOOT version=…`, then `INFO mode=<kernel_mode> …`, then each line in
    /// `tail` exactly, in order.
    pub fn new(version: &str, kernel_mode: &str, tail: &[&str]) -> Self {
        let mut required = vec![
            Matcher::Exact(format!("ORIGINS_OS BOOT version={version}")),
            Matcher::Prefix(format!("ORIGINS_OS INFO mode={kernel_mode} ")),
        ];
        required.extend(tail.iter().map(|t| Matcher::Exact(t.to_string())));
        Expectation { required }
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
/// Every line containing `ORIGINS_OS ` is a marker, except `INFO` lines that
/// do not carry `mode=`. Each marker must match the next required one; any
/// other marker (e.g. PANIC in normal mode, READY in panic mode, or anything
/// after the final marker) fails immediately.
pub fn evaluate(exp: &Expectation, log: &str) -> Verdict {
    let complete = match log.rfind('\n') {
        Some(i) => &log[..=i],
        None => "",
    };
    let mut next = 0;
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
            Some(m) if m.matches(marker) => next += 1,
            Some(m) => return Verdict::Unexpected(format!("got `{marker}` while expecting `{m}`")),
            None => {
                return Verdict::Unexpected(format!(
                    "got `{marker}` after the final expected marker"
                ));
            }
        }
    }
    match exp.required.get(next) {
        None => Verdict::Pass,
        Some(m) => Verdict::Pending(m.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: &str = "0.1.0";

    // M1-shaped expectations keep these tests short; M2 cases only add
    // lines to the tail, which `new` handles the same way.
    fn normal(v: &str) -> Expectation {
        Expectation::new(v, "normal", &["ORIGINS_OS READY"])
    }

    fn panic(v: &str) -> Expectation {
        Expectation::new(v, "panic-test", &["ORIGINS_OS PANIC intentional-test"])
    }
    const NORMAL_OK: &str = "BdsDxe: loading\r\nORIGINS_OS BOOT version=0.1.0\r\n\
        ORIGINS_OS INFO mode=normal memory_regions=9 usable_bytes=1\r\nORIGINS_OS READY\r\n";
    const PANIC_OK: &str = "ORIGINS_OS BOOT version=0.1.0\r\n\
        ORIGINS_OS INFO mode=panic-test memory_regions=9 usable_bytes=1\r\n\
        ORIGINS_OS PANIC intentional-test\r\nORIGINS_OS INFO panic_location=kernel/src/main.rs:40\r\n";

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
        let log = "ORIGINS_OS BOOT version=0.1.0\nORIGINS_OS INFO mode=normal x=1\n";
        assert_eq!(
            evaluate(&normal(V), log),
            Verdict::Pending("ORIGINS_OS READY".into())
        );
    }

    #[test]
    fn empty_log_is_pending() {
        assert!(matches!(evaluate(&normal(V), ""), Verdict::Pending(_)));
    }

    #[test]
    fn partial_last_line_is_not_counted() {
        let log =
            "ORIGINS_OS BOOT version=0.1.0\nORIGINS_OS INFO mode=normal x=1\nORIGINS_OS READY";
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
        let log = "ORIGINS_OS BOOT version=0.1.0\nORIGINS_OS INFO mode=normal x=1\n\
            ORIGINS_OS PANIC something\n";
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
        let log = format!("{PANIC_OK}ORIGINS_OS READY\n");
        assert!(matches!(evaluate(&panic(V), &log), Verdict::Unexpected(_)));
    }

    #[test]
    fn out_of_order_fails() {
        let log = "ORIGINS_OS READY\nORIGINS_OS BOOT version=0.1.0\n";
        assert!(matches!(evaluate(&normal(V), log), Verdict::Unexpected(_)));
    }

    #[test]
    fn wrong_version_fails() {
        let log = "ORIGINS_OS BOOT version=9.9.9\n";
        assert!(matches!(evaluate(&normal(V), log), Verdict::Unexpected(_)));
    }

    #[test]
    fn duplicate_ready_fails() {
        let log = format!("{NORMAL_OK}ORIGINS_OS READY\n");
        assert!(matches!(evaluate(&normal(V), &log), Verdict::Unexpected(_)));
    }

    #[test]
    fn marker_after_escape_sequence_is_found() {
        let log = "\x1b[2J\x1b[01;01HORIGINS_OS BOOT version=0.1.0\r\n";
        assert_eq!(
            evaluate(&normal(V), log),
            Verdict::Pending("ORIGINS_OS INFO mode=normal …".into())
        );
    }

    #[test]
    fn fault_marker_must_match_exactly() {
        let exp = Expectation::new(
            V,
            "fault-heap-guard",
            &[
                "ORIGINS_OS EXCEPTION page-fault access=read cause=not-present addr=0xffffc00000101000",
            ],
        );
        let head = "ORIGINS_OS BOOT version=0.1.0
ORIGINS_OS INFO mode=fault-heap-guard x=1
";
        let good = format!(
            "{head}ORIGINS_OS EXCEPTION page-fault access=read cause=not-present addr=0xffffc00000101000
             ORIGINS_OS INFO exception rip=0x1 rsp=0x2 error=0x0
"
        );
        assert_eq!(evaluate(&exp, &good), Verdict::Pass);
        let wrong_addr = format!(
            "{head}ORIGINS_OS EXCEPTION page-fault access=read cause=not-present addr=0x0000000000000000
"
        );
        assert!(matches!(
            evaluate(&exp, &wrong_addr),
            Verdict::Unexpected(_)
        ));
    }

    #[test]
    fn self_test_failure_panic_is_rejected() {
        let exp = Expectation::new(
            V,
            "normal",
            &["ORIGINS_OS TEST heap ok", "ORIGINS_OS READY"],
        );
        let log = "ORIGINS_OS BOOT version=0.1.0
ORIGINS_OS INFO mode=normal x=1
            ORIGINS_OS PANIC self-test heap: Vec sum 1 wrong
";
        assert!(matches!(evaluate(&exp, log), Verdict::Unexpected(_)));
    }
}
