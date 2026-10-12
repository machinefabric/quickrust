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

fn run_suite(ctx: &Ctx, v: &Variant, w: &Workload, co: &Path) -> (i32, Outcomes) {
    let out = ctx.command(v, w, co, &["test", "--no-fail-fast"], true).output().unwrap_or_else(|e| fail(&format!("[{} / {}] cargo test: {}", w.name, v.name, e)));
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
                let (_, outcomes) = run_suite(ctx, v, w, &co);
                per_variant.entry(v.name.clone()).or_default().push(outcomes);
            }
        }
        let flaky = |runs: &[Outcomes]| -> BTreeSet<String> {
            let names: BTreeSet<&String> = runs.iter().flat_map(|r| r.keys()).collect();
            names.into_iter().filter(|n| runs.iter().map(|r| r.get(*n)).collect::<BTreeSet<_>>().len() > 1).cloned().collect()
        };
        let reference = &per_variant["cargo"];
        let reference_flaky = flaky(reference);
        let mut rows = Vec::new();
        println!("\n{} @ {}", w.name, &w.rev[..w.rev.len().min(10)]);
        for v in variants.iter().filter(|v| v.name != "cargo") {
            let runs = &per_variant[&v.name];
            let unstable: BTreeSet<String> = flaky(runs).union(&reference_flaky).cloned().collect();
            let mut differ = Vec::new();
            let names: BTreeSet<&String> = runs.iter().chain(reference.iter()).flat_map(|r| r.keys()).collect();
            for n in names.into_iter().filter(|n| !unstable.contains(*n)) {
                let (theirs, ours) = (reference[0].get(n), runs[0].get(n));
                if theirs != ours {
                    differ.push(json!({ "test": n, "cargo": theirs, "variant": ours }));
                }
            }
            let tests = runs[0].len();
            let failing = runs[0].values().filter(|r| *r == "FAILED").count();
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
