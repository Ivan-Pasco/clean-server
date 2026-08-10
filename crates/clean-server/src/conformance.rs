//! CMOD-03 — host conformance (Platform 15 §10.1).
//!
//! The shipping gate: "CI runs all host conformance suites on every
//! release-candidate tag. A host that fails conformance MUST NOT ship."
//!
//! CMOD-03 names four checks:
//!
//! 1. Load a canonical set of Clean-compiled components (`tests/cln/conformance/`).
//! 2. Run each and diff stdout / structured output against expected.
//! 3. Verify every WIT import in the advertised world is actually provided.
//! 4. Verify no import outside the advertised world is silently accepted.
//!
//! **Checks 1 and 2 cannot run yet.** The canonical corpus does not exist
//! anywhere in the workspace, and it cannot be populated until the compiler
//! emits Component Model components — today it emits core modules (see the
//! README's note on the hand-written acceptance guest).
//!
//! This suite therefore reports `Incomplete` rather than passing. A host that
//! has not run checks 1–2 has not been shown to conform, and a green result
//! would claim a gate that was never met — the same failure mode HCV-06 exists
//! to prevent for `host.wit`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use clean_host_core::parity::Registration;
use clean_host_core::validate::InterfaceRef;

/// How one check turned out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Pass(String),
    Fail(String),
    /// Could not run, with the reason. Never treated as a pass.
    Skipped(String),
}

impl CheckOutcome {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Pass(_) => "PASS",
            Self::Fail(_) => "FAIL",
            Self::Skipped(_) => "SKIPPED",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::Pass(d) | Self::Fail(d) | Self::Skipped(d) => d,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub number: u8,
    pub name: &'static str,
    pub outcome: CheckOutcome,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    /// Conformance holds only when every check ran and passed.
    ///
    /// A skipped check is not a pass: CMOD-03 is a gate, and a gate that
    /// reports green on a partial run is worse than no gate.
    pub fn conforms(&self) -> bool {
        self.checks
            .iter()
            .all(|c| matches!(c.outcome, CheckOutcome::Pass(_)))
    }

    pub fn ran(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| !matches!(c.outcome, CheckOutcome::Skipped(_)))
            .count()
    }

    pub fn failed(&self) -> bool {
        self.checks
            .iter()
            .any(|c| matches!(c.outcome, CheckOutcome::Fail(_)))
    }

    pub fn render(&self) -> String {
        let mut out = String::from("CMOD-03 host conformance\n\n");

        for check in &self.checks {
            out.push_str(&format!(
                "  [{}] {:<34} {}\n",
                check.number,
                check.name,
                check.outcome.label()
            ));
            let detail = check.outcome.detail();
            if !detail.is_empty() {
                out.push_str(&format!("      {detail}\n"));
            }
        }

        out.push('\n');
        if self.conforms() {
            out.push_str("RESULT: CONFORMS — all 4 checks ran and passed.\n");
        } else if self.failed() {
            out.push_str("RESULT: FAILED — this host does not conform and MUST NOT ship.\n");
        } else {
            out.push_str(&format!(
                "RESULT: INCOMPLETE — {} of {} checks ran.\n\
                 A host that has not run every check has not been shown to conform\n\
                 (Platform 15 §10.1, CMOD-03). This is not a passing gate.\n",
                self.ran(),
                self.checks.len()
            ));
        }
        out
    }
}

/// Run the suite.
///
/// `world_path` is the repo-root `host.wit`; `registrations` is what the
/// server's linker wiring reports; `corpus` is where the canonical components
/// would live.
pub fn run(world_path: &Path, registrations: &[Registration], corpus: &Path) -> Report {
    let corpus_components = find_corpus(corpus);

    Report {
        checks: vec![
            Check {
                number: 1,
                name: "canonical corpus loads",
                outcome: check_corpus_present(corpus, &corpus_components),
            },
            Check {
                number: 2,
                name: "corpus output matches expected",
                outcome: check_corpus_output(corpus, &corpus_components),
            },
            Check {
                number: 3,
                name: "world imports are all provided",
                outcome: check_world_provided(world_path, registrations),
            },
            Check {
                number: 4,
                name: "no extra-world imports accepted",
                outcome: check_no_extra_imports(world_path, registrations),
            },
        ],
    }
}

/// Components in the corpus directory, if it exists.
fn find_corpus(corpus: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(corpus) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "wasm"))
        .collect();
    found.sort();
    found
}

fn corpus_missing_reason(corpus: &Path) -> String {
    format!(
        "no components at {} — the canonical corpus does not exist yet, and cannot \
         be populated until the compiler emits Component Model components",
        corpus.display()
    )
}

fn check_corpus_present(corpus: &Path, components: &[PathBuf]) -> CheckOutcome {
    if components.is_empty() {
        CheckOutcome::Skipped(corpus_missing_reason(corpus))
    } else {
        CheckOutcome::Pass(format!("{} component(s)", components.len()))
    }
}

fn check_corpus_output(corpus: &Path, components: &[PathBuf]) -> CheckOutcome {
    if components.is_empty() {
        CheckOutcome::Skipped(corpus_missing_reason(corpus))
    } else {
        // Running the corpus needs the expected-output fixtures that ship
        // alongside it; deferred until there is a corpus to run.
        CheckOutcome::Skipped(
            "corpus present but the expected-output fixtures are not implemented".into(),
        )
    }
}

/// Check 3 — every interface the world advertises is really provided.
///
/// Distinct from HCV-06, which compares `host.wit` against the linker in both
/// directions. This asks the narrower question CMOD-03 poses: could a guest
/// targeting this world find any advertised interface missing at run time?
fn check_world_provided(world_path: &Path, registrations: &[Registration]) -> CheckOutcome {
    let declared = match clean_host_core::parity::parse_host_wit(world_path) {
        Ok(d) => d,
        Err(e) => return CheckOutcome::Fail(format!("cannot read the advertised world: {e}")),
    };

    let provided: BTreeSet<String> = registrations
        .iter()
        .map(|r| InterfaceRef::parse(&r.interface).path)
        .collect();

    let missing: Vec<String> = declared
        .iter()
        .map(|d| InterfaceRef::parse(d))
        .filter(|d| !provided.contains(&d.path))
        .map(|d| d.to_string())
        .collect();

    if missing.is_empty() {
        CheckOutcome::Pass(format!(
            "{}/{} advertised interfaces provided",
            declared.len(),
            declared.len()
        ))
    } else {
        CheckOutcome::Fail(format!(
            "advertised but not provided: {}",
            missing.join(", ")
        ))
    }
}

/// Check 4 — nothing outside the advertised world is silently accepted.
///
/// A registration the world does not declare is reachable by a guest that
/// knows its name while being invisible to every static check — which is the
/// capability leak SRVH-01 is meant to prevent.
fn check_no_extra_imports(world_path: &Path, registrations: &[Registration]) -> CheckOutcome {
    let declared = match clean_host_core::parity::parse_host_wit(world_path) {
        Ok(d) => d,
        Err(e) => return CheckOutcome::Fail(format!("cannot read the advertised world: {e}")),
    };

    let declared_paths: BTreeSet<String> = declared
        .iter()
        .map(|d| InterfaceRef::parse(d).path)
        .collect();

    let extra: Vec<String> = registrations
        .iter()
        .filter(|r| !declared_paths.contains(&InterfaceRef::parse(&r.interface).path))
        .map(|r| r.interface.clone())
        .collect();

    if extra.is_empty() {
        CheckOutcome::Pass("no registrations outside the world".into())
    } else {
        CheckOutcome::Fail(format!(
            "registered but not advertised: {} — reachable by a guest that knows \
             the name, invisible to every static check",
            extra.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clean_host_core::parity::RegistrationKind;
    use std::io::Write;

    fn wit_file(body: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.wit");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        (dir, path)
    }

    const WIT: &str = r#"
package clean:http@0.1.0;

interface routing {
    register: func(path: string);
}

interface request {
    method: func() -> string;
}

world server {
    export routing;
    export request;
}
"#;

    fn real(iface: &str) -> Registration {
        Registration {
            interface: iface.to_string(),
            kind: RegistrationKind::Real,
        }
    }

    fn empty_corpus() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conformance");
        std::fs::create_dir_all(&path).unwrap();
        (dir, path)
    }

    #[test]
    fn a_fully_provided_world_passes_check_three() {
        let (_d, wit) = wit_file(WIT);
        let (_c, corpus) = empty_corpus();
        let report = run(
            &wit,
            &[
                real("clean:http/routing@0.1.0"),
                real("clean:http/request@0.1.0"),
            ],
            &corpus,
        );

        let check = &report.checks[2];
        assert!(
            matches!(check.outcome, CheckOutcome::Pass(_)),
            "{:?}",
            check.outcome
        );
    }

    #[test]
    fn an_advertised_but_unprovided_interface_fails_check_three() {
        // A guest targeting this world would find `request` missing at run
        // time, which is precisely what CMOD-03 check 3 asks about.
        let (_d, wit) = wit_file(WIT);
        let (_c, corpus) = empty_corpus();
        let report = run(&wit, &[real("clean:http/routing@0.1.0")], &corpus);

        let check = &report.checks[2];
        assert!(matches!(check.outcome, CheckOutcome::Fail(_)));
        assert!(report.failed());
        assert!(!report.conforms());
    }

    #[test]
    fn a_registration_outside_the_world_fails_check_four() {
        // Reachable by a guest that knows the name, invisible to static checks.
        let (_d, wit) = wit_file(WIT);
        let (_c, corpus) = empty_corpus();
        let report = run(
            &wit,
            &[
                real("clean:http/routing@0.1.0"),
                real("clean:http/request@0.1.0"),
                real("clean:secret/backdoor@0.1.0"),
            ],
            &corpus,
        );

        let check = &report.checks[3];
        assert!(matches!(check.outcome, CheckOutcome::Fail(_)));
        assert!(check.outcome.detail().contains("clean:secret/backdoor"));
    }

    #[test]
    fn an_absent_corpus_is_skipped_not_passed() {
        // The distinction the whole gate rests on.
        let (_d, wit) = wit_file(WIT);
        let (_c, corpus) = empty_corpus();
        let report = run(
            &wit,
            &[
                real("clean:http/routing@0.1.0"),
                real("clean:http/request@0.1.0"),
            ],
            &corpus,
        );

        assert!(matches!(report.checks[0].outcome, CheckOutcome::Skipped(_)));
        assert!(matches!(report.checks[1].outcome, CheckOutcome::Skipped(_)));
        assert!(
            !report.conforms(),
            "a partial run must never report conformance"
        );
        assert!(!report.failed(), "skipped is not failed");
        assert_eq!(report.ran(), 2);
    }

    #[test]
    fn the_incomplete_result_says_so_in_plain_words() {
        let (_d, wit) = wit_file(WIT);
        let (_c, corpus) = empty_corpus();
        let report = run(
            &wit,
            &[
                real("clean:http/routing@0.1.0"),
                real("clean:http/request@0.1.0"),
            ],
            &corpus,
        );

        let rendered = report.render();
        assert!(rendered.contains("INCOMPLETE"), "{rendered}");
        assert!(rendered.contains("2 of 4"), "{rendered}");
        assert!(rendered.contains("CMOD-03"), "{rendered}");
        // The reason a reader most needs: why the corpus checks did not run.
        assert!(rendered.contains("does not exist yet"), "{rendered}");
    }

    #[test]
    fn a_failing_check_says_the_host_must_not_ship() {
        let (_d, wit) = wit_file(WIT);
        let (_c, corpus) = empty_corpus();
        let report = run(&wit, &[], &corpus);

        let rendered = report.render();
        assert!(rendered.contains("MUST NOT ship"), "{rendered}");
    }

    #[test]
    fn an_unreadable_world_fails_rather_than_skipping() {
        // A missing host.wit is a conformance failure, not an excuse to skip:
        // HCV-02 requires it to exist.
        let (_c, corpus) = empty_corpus();
        let report = run(Path::new("/nonexistent/host.wit"), &[], &corpus);

        assert!(matches!(report.checks[2].outcome, CheckOutcome::Fail(_)));
        assert!(report.failed());
    }

    #[test]
    fn a_populated_corpus_stops_skipping_check_one() {
        // Guards against the skip becoming permanent by accident: the moment
        // components appear, check 1 starts running.
        let (_d, wit) = wit_file(WIT);
        let (_c, corpus) = empty_corpus();
        std::fs::write(corpus.join("case.wasm"), b"\0asm\x0d\0\x01\0").unwrap();

        let report = run(
            &wit,
            &[
                real("clean:http/routing@0.1.0"),
                real("clean:http/request@0.1.0"),
            ],
            &corpus,
        );
        assert!(matches!(report.checks[0].outcome, CheckOutcome::Pass(_)));
        // Check 2 still needs its expected-output fixtures.
        assert!(matches!(report.checks[1].outcome, CheckOutcome::Skipped(_)));
    }
}
