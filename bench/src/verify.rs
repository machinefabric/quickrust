//! The correctness gate: each workload's own test suite, built and run by upstream Cargo and by
//! every Haul variant, from fresh checkouts. A lever is acceptable only when the outcomes agree.
//!
//! Two rounds per variant. The first starts from empty Haul state; the second keeps the
//! variant's artifact cache, so its tests also run against binaries built from restored units.
//! A test whose outcome differs between the rounds of one variant is flaky and reported as
//! such; a test whose outcome differs between upstream Cargo and a variant, consistently, fails
//! the gate.

use crate::workload::{self, Workload};
use crate::{fail, Ctx, Variant};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// Outcome of every test of one suite run, by name (`ok`, `FAILED`, `ignored`).
type Outcomes = BTreeMap<String, String>;

fn run_suite(ctx: &Ctx, v: &Variant, w: &Workload, co: &Path, only: Option<&str>) -> (i32, Outcomes) {
    let mut cmd = ctx.command(v, w, co, &["test", "--no-fail-fast"], false);
    cmd.args(&w.test_args);
    if let Some(name) = only {
        cmd.args(["--", "--exact", name]);
    }
    let out = cmd.output().unwrap_or_else(|e| fail(&format!("[{} / {}] cargo test: {}", w.name, v.name, e)));
    let mut outcomes = Outcomes::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // libtest: `test path::to::name ... ok` (or FAILED / ignored, possibly with a reason).
        let Some(rest) = line.strip_prefix("test ") else { continue };
        let Some((name, result)) = rest.split_once(" ... ") else { continue };
        let result = result.split_whitespace().next().unwrap_or("");
        if matches!(result, "ok" | "FAILED" | "ignored") {
            outcomes.insert(name.to_string(), result.to_string());
        }
    }
    let code = out.status.code().unwrap_or(-1);
    if outcomes.is_empty() && code != 0 {
        // Nothing ran: the suite did not build. That is a result to compare too, not a crash.
        outcomes.insert("<the test suite did not build>".into(), "FAILED".into());
    }
    (code, outcomes)
}

/// Reruns of each test that differs, alone, on each side: an outcome that changes between
/// runs of the same build is flakiness (timing, scheduling), not a difference of the build.
const CONFIRMATIONS: usize = 4;
/// More differing tests than this is not flakiness; they are reported without reruns.
const MAX_CONFIRMED: usize = 20;

/// Outcomes of `name` over `CONFIRMATIONS` runs alone in the checkout `co`, plus `first`.
fn confirm(ctx: &Ctx, v: &Variant, w: &Workload, co: &Path, name: &str, first: Option<&String>) -> BTreeSet<Option<String>> {
    let mut seen: BTreeSet<Option<String>> = BTreeSet::from([first.cloned()]);
    for _ in 0..CONFIRMATIONS {
        let (_, outcomes) = run_suite(ctx, v, w, co, Some(name));
        seen.insert(outcomes.get(name).cloned());
    }
    seen
}

/// Runs the gate over `workloads` for `variants` (upstream Cargo is always the reference).
/// Returns the report and whether every variant agreed with Cargo.
pub fn verify(ctx: &Ctx, variants: &[Variant], workloads: &[Workload]) -> (Value, bool) {
    const ROUNDS: usize = 2;
    let mut all_agree = true;
    let mut report = Vec::new();
    for w in workloads {
        let mut per_variant: BTreeMap<String, Vec<Outcomes>> = BTreeMap::new();
        for v in variants {
            let base = ctx.work.join("verify").join(&v.name).join(&w.name);
            let _ = fs::remove_dir_all(&base);
            for round in 0..ROUNDS {
                let co = base.join(format!("round-{}", round + 1));
                workload::checkout(&ctx.work, w, &co).unwrap_or_else(|e| fail(&e));
                if round == 0 {
                    // Empty Haul state for the first round; the second keeps the artifact cache.
                    let _ = fs::remove_dir_all(ctx.variant_dir(v, w).join("haul-home"));
                }
                eprintln!("haul-bench: verify {} · {} · round {}/{}", w.name, v.name, round + 1, ROUNDS);
                let (_, outcomes) = run_suite(ctx, v, w, &co, None);
                per_variant.entry(v.name.clone()).or_default().push(outcomes);
            }
        }
        let flaky = |runs: &[Outcomes]| -> BTreeSet<String> {
            let names: BTreeSet<&String> = runs.iter().flat_map(|r| r.keys()).collect();
            names.into_iter().filter(|n| runs.iter().map(|r| r.get(*n)).collect::<BTreeSet<_>>().len() > 1).cloned().collect()
        };
        let reference = &per_variant["cargo"];
        let cargo = variants.iter().find(|v| v.name == "cargo").unwrap_or_else(|| fail("the gate compares against the `cargo` variant"));
        if reference.iter().any(|r| !r.keys().any(|n| !n.starts_with('<'))) {
            // A suite that runs nothing would make every variant agree by construction.
            fail(&format!("[{}] upstream Cargo ran no tests (or the suite did not build): set `test-args` in workloads.toml to a package selection with tests", w.name));
        }
        let reference_flaky = flaky(reference);
        let mut rows = Vec::new();
        println!("\n{} @ {}", w.name, &w.rev[..w.rev.len().min(10)]);
        for v in variants.iter().filter(|v| v.name != "cargo") {
            let runs = &per_variant[&v.name];
            let mut unstable: BTreeSet<String> = flaky(runs).union(&reference_flaky).cloned().collect();
            let names: BTreeSet<&String> = runs.iter().chain(reference.iter()).flat_map(|r| r.keys()).collect();
            let candidates: Vec<String> = names.into_iter().filter(|n| !unstable.contains(*n) && reference[0].get(*n) != runs[0].get(*n)).cloned().collect();
            let mut differ = Vec::new();
            let last = |name: &str| ctx.work.join("verify").join(name).join(&w.name).join(format!("round-{}", ROUNDS));
            for n in &candidates {
                let (theirs, ours) = (reference[0].get(n), runs[0].get(n));
                if candidates.len() <= MAX_CONFIRMED && !n.starts_with('<') {
                    eprintln!("haul-bench: verify {} · {} differs; running it alone on both sides", w.name, n);
                    let (a, b) = (confirm(ctx, cargo, w, &last("cargo"), n, theirs), confirm(ctx, v, w, &last(&v.name), n, ours));
                    if a.len() > 1 || b.len() > 1 {
                        unstable.insert(n.clone());
                        continue;
                    }
                }
                differ.push(json!({ "test": n, "cargo": theirs, "variant": ours }));
            }
            let tests = runs[0].len();
            let failing = runs[0].iter().filter(|(n, r)| *r == "FAILED" && reference[0].get(*n) == Some(*r)).count();
            let agree = differ.is_empty();
            all_agree &= agree;
            println!("  {:<14} {} · {} tests, {} failing under both{} · {}", v.name, if agree { "SAME OUTCOME" } else { "DIFFERENT OUTCOME" }, tests, failing, if unstable.is_empty() { String::new() } else { format!(", {} flaky", unstable.len()) }, if agree { String::new() } else { format!("{} test(s) differ", differ.len()) });
            for d in differ.iter().take(10) {
                println!("      {}: cargo {} · {} {}", d["test"].as_str().unwrap_or(""), d["cargo"].as_str().unwrap_or("absent"), v.name, d["variant"].as_str().unwrap_or("absent"));
            }
            rows.push(json!({ "variant": v.name, "agree": agree, "tests": tests, "failing_under_both": failing, "flaky": unstable, "differ": differ }));
        }
        report.push(json!({ "workload": w.name, "rev": w.rev, "cargo_tests": reference[0].len(), "cargo_flaky": reference_flaky, "variants": rows }));
    }
    (json!(report), all_agree)
}
