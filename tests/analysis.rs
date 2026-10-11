//! End-to-end checks of the management surface: reports, rebuild analysis,
//! budgets, storage planning, benchmarks, experiments, worktrees, remediation.

#![cfg(unix)]

mod common;

use common::*;
use serde_json::{json, Value};
use std::fs;
use std::process::Stdio;

fn json_out(cmd: &mut std::process::Command) -> Value {
    let out = ok(cmd);
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("not JSON ({}): {}", e, stdout(&out)))
}

fn reasons(report: &Value) -> Vec<(String, String, String)> {
    let mut v: Vec<(String, String, String)> = report["rebuilds"]["units"].as_array().unwrap().iter().map(|u| (u["package"].as_str().unwrap().to_string(), u["kind"].as_str().unwrap().to_string(), u["reason"].as_str().unwrap().to_string())).collect();
    v.sort();
    v
}

#[test]
fn report_follows_the_versioned_contract() {
    let sb = Sandbox::new("contract");
    let ws = sb.workspace("ws");
    let sidecar = sb.dir.join("out/{session}.json");
    ok(sb.cargo(&ws).arg("build").env("HAUL_REPORT", &sidecar));
    let files = tree(&sb.dir.join("out"));
    assert_eq!(files.len(), 1, "exactly one sidecar, named after the session: {:?}", files);
    let r: Value = serde_json::from_slice(&fs::read(sb.dir.join("out").join(&files[0])).unwrap()).unwrap();
    assert_eq!(r["schema_version"], "1.0");
    assert_eq!(files[0], format!("{}.json", r["run"]["id"].as_str().unwrap()));
    // Fields of the documented contract (spec "CLI JSON contract").
    for path in [
        "run.id", "run.state", "run.started_at", "run.duration_ns", "run.command.family", "run.command.argv_redacted", "run.command.cwd_token", "run.result.cargo_exit_code", "run.result.policy",
        "workspace.id", "workspace.members", "toolchain.cargo_version", "toolchain.rustc_version", "toolchain.host", "toolchain.capabilities",
        "metrics.wall_time_ns", "metrics.critical_path_ns", "metrics.artifact_logical_bytes", "metrics.artifact_allocated_bytes", "metrics.units_observed", "metrics.units_compiled",
        "budget.result", "budget.evaluations",
    ] {
        let mut cur = &r;
        for part in path.split('.') {
            cur = &cur[part];
        }
        assert!(!cur.is_null(), "contract field `{}` is missing", path);
    }
    assert!(r["run"]["parent_session_id"].is_null() && r["run"]["result"]["signal"].is_null());
    assert_eq!(r["run"]["state"], "completed");
    assert_eq!(r["run"]["command"]["argv_redacted"], json!(["cargo", "build"]));
    assert_eq!(r["run"]["command"]["cwd_token"], "$WORKSPACE");
    assert_eq!(r["workspace"]["members"], 4);
    for cap in ["cargo_metadata_v1", "cargo_json_messages", "rustc_wrapper"] {
        assert!(r["toolchain"]["capabilities"].as_array().unwrap().contains(&json!(cap)));
    }
    // AC-004 / AC-005: units, a critical path with per-edge confidence, and the artifact footprint.
    assert_eq!(r["metrics"]["units_compiled"], 5, "util, gen build script, gen, mac, app");
    assert!(r["metrics"]["artifact_allocated_bytes"].as_u64().unwrap() > 100_000);
    let cp = r["critical_path"].as_array().unwrap();
    assert!(cp.len() >= 2 && cp.iter().all(|u| matches!(u["edge_confidence"].as_str(), Some("observed" | "inferred" | "modeled"))));
    assert_eq!(cp.last().unwrap()["package"], "app@0.1.0");
    assert!(r["metrics"]["critical_path_ns"].as_u64().unwrap() <= r["metrics"]["sum_unit_time_ns"].as_u64().unwrap() + r["metrics"]["build_script_run_time_ns"].as_u64().unwrap());
    // DATA-006: fresh units are unknown without Cargo's JSON stream — null with a reason, not zero.
    assert!(r["metrics"]["units_fresh"].is_null());
    assert!(r["unavailable"]["metrics.units_fresh"].is_string());
    // PRIV-004: no machine-specific prefix survives export.
    let text = serde_json::to_string(&r).unwrap();
    assert!(!text.contains(sb.dir.to_str().unwrap()), "the sandbox path leaked into the exported report");
    assert!(!text.contains(&std::env::var("HOME").unwrap()), "the home directory leaked into the exported report");
    // DATA-001: the stored report is immutable; asking again returns the same bytes.
    let a = ok(sb.haul(&ws).args(["report", "--format", "json"]));
    let b = ok(sb.haul(&ws).args(["report", "--format", "json"]));
    assert_eq!(a.stdout, b.stdout);
    assert_eq!(serde_json::from_slice::<Value>(&a.stdout).unwrap()["run"]["id"], r["run"]["id"]);
}

#[test]
fn rebuild_causes_are_classified_from_evidence() {
    // AC-006 across the classifications that can be provoked on a stable toolchain.
    let sb = Sandbox::new("rebuilds");
    let ws = sb.workspace("ws");
    ok(sb.cargo(&ws).arg("build"));
    assert!(reasons(&sb.report(&ws, "latest")).iter().all(|(_, _, r)| r == "new"));

    // Source edit in a leaf: the leaf is `source`, its dependent is `dependency`, nothing else rebuilds.
    fs::write(ws.join("util/src/lib.rs"), "pub fn double(x: u32) -> u32 { x + x }\n").unwrap();
    ok(sb.cargo(&ws).arg("build"));
    let r = sb.report(&ws, "latest");
    assert_eq!(reasons(&r), vec![("app".into(), "bin".into(), "dependency".into()), ("util".into(), "lib".into(), "source".into())]);
    let util = r["rebuilds"]["units"].as_array().unwrap().iter().find(|u| u["package"] == "util").unwrap();
    assert!(util["detail"].as_str().unwrap().contains("util/src/lib.rs (modified)"), "{}", util["detail"]);
    assert_eq!(r["rebuilds"]["summary"]["previously_known"], 2);

    // Feature change.
    ok(sb.cargo(&ws).args(["build", "-p", "util", "--features", "extra"]));
    let r = sb.report(&ws, "latest");
    assert_eq!(reasons(&r), vec![("util".into(), "lib".into(), "feature-set".into())]);
    assert!(r["rebuilds"]["units"][0]["detail"].as_str().unwrap().contains("[] → [extra]"));

    // Flags from the environment.
    ok(sb.cargo(&ws).args(["build", "-p", "util"]).env("RUSTFLAGS", "--cfg haul_test_flag"));
    assert_eq!(reasons(&sb.report(&ws, "latest")), vec![("util".into(), "lib".into(), "rustflags".into())]);

    // Profile change.
    ok(sb.cargo(&ws).args(["build", "-p", "util", "--release"]));
    assert_eq!(reasons(&sb.report(&ws, "latest")), vec![("util".into(), "lib".into(), "profile".into())]);

    // Touch without changing content: Cargo rebuilds; the report says the content is identical.
    let lib = ws.join("util/src/lib.rs");
    let content = fs::read(&lib).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(&lib, content).unwrap();
    ok(sb.cargo(&ws).args(["build", "-p", "util"]));
    let r = sb.report(&ws, "latest");
    let touched = r["rebuilds"]["units"].as_array().unwrap().iter().find(|u| u["package"] == "util").expect("util rebuilt after touch");
    assert_eq!(touched["reason"], "source");
    assert!(touched["detail"].as_str().unwrap().contains("timestamp-only"), "{}", touched["detail"]);

    // The duplication view now knows util in several variants, with their causes.
    let dup = json_out(sb.haul(&ws).args(["analyze", "duplication", "--format", "json"]));
    let util_group = dup["variants"].as_array().unwrap().iter().find(|g| g["package"] == "util").expect("util variants are reported");
    let causes = util_group["causes"].to_string();
    assert!(causes.contains("feature-set") && causes.contains("profile"), "{}", causes);
}

#[test]
fn toolchain_change_is_reported_as_compiler_drift() {
    // FR-040 acceptance.
    let sb = Sandbox::new("drift");
    let p = sb.package("p", "p");
    if !run(sb.upstream(&p).args(["+nightly", "--version"])).status.success() {
        eprintln!("skipping: nightly toolchain not installed");
        return;
    }
    ok(sb.cargo(&p).args(["check", "--lib"]));
    ok(sb.cargo(&p).args(["+nightly", "check", "--lib"]));
    let r = sb.report(&p, "latest");
    assert_eq!(reasons(&r), vec![("p".into(), "lib".into(), "compiler".into())]);
    // FR-160 acceptance: comparing the two runs states the compiler difference before any delta.
    let cmp = json_out(sb.haul(&p).args(["compare", "latest~1", "latest", "--format", "json"]));
    assert_eq!(cmp["comparability"]["state"], "incomparable");
    assert!(cmp["comparability"]["differences"].as_array().unwrap().iter().any(|d| d["what"] == "compiler version"));
    let text = stdout(&ok(sb.haul(&p).args(["compare", "latest~1", "latest"])));
    let (diff_at, table_at) = (text.find("compiler version").unwrap(), text.find("METRIC").unwrap());
    assert!(diff_at < table_at, "environment differences must precede the deltas:\n{}", text);
}

#[test]
fn build_script_without_rerun_directives_is_flagged_after_it_reruns() {
    // FR-060 acceptance.
    let sb = Sandbox::new("rerun");
    let ws = sb.workspace("ws");
    ok(sb.cargo(&ws).arg("build"));
    let first = sb.report(&ws, "latest");
    assert_eq!(first["build_scripts"][0]["state"], "observed");
    assert!(first["recommendations"].as_array().unwrap().iter().all(|r| r["rule"] != "build_script_broad_invalidation"), "one run is not evidence of repeated cost");
    // An edit unrelated to the script's real input still reruns it.
    fs::write(ws.join("gen/src/lib.rs"), "include!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n// unrelated\n").unwrap();
    ok(sb.cargo(&ws).arg("build"));
    let second = sb.report(&ws, "latest");
    let script = &second["build_scripts"][0];
    assert_eq!((script["ran_in_session"].as_bool(), script["has_rerun_directives"].as_bool(), script["state"].as_str()), (Some(true), Some(false), Some("over-invalidating-candidate")));
    let rec = second["recommendations"].as_array().unwrap().iter().find(|r| r["rule"] == "build_script_broad_invalidation").expect("narrowing opportunity is recommended");
    // AC-015: evidence, confidence, risk and a verification method; the patch class is review-required.
    assert!(rec["evidence"].as_array().unwrap().len() >= 2 && rec["confidence"].is_string() && rec["risk"].is_string() && rec["verification"].is_string());
    assert_eq!(rec["remediation"], "review_required");
    assert_eq!(rec["impact"]["kind"], "measured");
    assert_eq!(rec["impact"]["avoidable_time_ns"], script["duration_ns"]);
    // AC-017: the preview changes nothing in the repository.
    let before = tree(&ws);
    let text = stdout(&ok(sb.haul(&ws).args(["fix", "--dry-run"])));
    assert!(text.contains("guidance-only") && text.contains("rerun-if-changed"), "{}", text);
    assert_eq!(tree(&ws), before);
}

#[test]
fn secrets_never_reach_reports_or_state() {
    // AC-019 / DATA-008 / SEC-016.
    let sb = Sandbox::new("secrets");
    let p = sb.package("p", "p");
    let secrets = ["tok-AAAA1111secret", "pw-BBBB2222secret", "key-CCCC3333secret"];
    let sidecar = sb.dir.join("report.json");
    ok(sb.cargo(&p)
        .arg("build")
        .env("CARGO_REGISTRIES_INTERNAL_TOKEN", secrets[0])
        .env("SCCACHE_REDIS", format!("redis://user:{}@cache.invalid", secrets[1]))
        .env("AWS_SECRET_ACCESS_KEY", secrets[2])
        .env("HAUL_REPORT", &sidecar));
    let mut haystack = vec![fs::read_to_string(&sidecar).unwrap()];
    for f in tree(&sb.home) {
        haystack.push(String::from_utf8_lossy(&fs::read(sb.home.join(&f)).unwrap()).into_owned());
    }
    haystack.push(stdout(&ok(sb.haul(&p).args(["report"]))));
    haystack.push(stdout(&ok(sb.haul(&p).args(["report", "--format", "html"]))));
    for s in secrets {
        assert!(haystack.iter().all(|h| !h.contains(s)), "secret {} was persisted or printed", s);
    }
    let r: Value = serde_json::from_str(&haystack[0]).unwrap();
    assert_eq!(r["environment"]["variables"]["vars"]["SCCACHE_REDIS"], json!({ "set": true, "value": "<redacted>" }));
    assert!(r["environment"]["variables"]["vars"].get("CARGO_REGISTRIES_INTERNAL_TOKEN").is_none(), "variables outside the allowlist are not persisted at all");
}

#[test]
fn budgets_gate_ci_without_touching_cargo_output() {
    // AC-016, FR-130.
    let sb = Sandbox::new("budgets");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).arg("build"));
    let set = stdout(&ok(sb.haul(&p).args(["baseline", "set", "main"])));
    assert!(set.contains("Baseline `main` set"));
    let baseline: Value = serde_json::from_slice(&fs::read(p.join(".haul/baselines/main.json")).unwrap()).unwrap();
    assert_eq!(baseline["baseline"]["name"], "main");
    assert!(!fs::read_to_string(p.join(".haul/baselines/main.json")).unwrap().contains(sb.dir.to_str().unwrap()), "a committed baseline must not embed local paths");

    // No budget file yet: an explicit error, not a silent pass.
    let missing = run(sb.haul(&p).args(["ci", "evaluate"]));
    assert_eq!(missing.status.code(), Some(2));
    assert!(stderr(&missing).contains("budgets.toml"));

    // A generous relative budget passes against the baseline taken from this very run.
    sb.file("p/.haul/budgets.toml", "[policy]\nmode = \"blocking\"\n\n[[budget]]\nmetric = \"wall_time_ns\"\nmax-increase-percent = 50\n\n[[budget]]\nmetric = \"artifact_allocated_bytes\"\nmax-value = \"10GiB\"\n");
    let pass = ok(sb.haul(&p).args(["ci", "evaluate", "--format", "json"]));
    assert_eq!(serde_json::from_slice::<Value>(&pass.stdout).unwrap()["result"], "pass");

    // An impossible absolute budget fails with the policy exit status.
    sb.file("p/.haul/budgets.toml", "[policy]\nmode = \"blocking\"\n\n[[budget]]\nmetric = \"wall_time_ns\"\nmax-value = \"1ns\"\n");
    let fail = run(sb.haul(&p).args(["ci", "evaluate"]));
    assert_eq!(fail.status.code(), Some(3), "{}", stdout(&fail));
    assert!(stdout(&fail).contains("Budget result: FAIL"));
    // Warn-only rollout: same violation, reported, not blocking.
    sb.file("p/.haul/budgets.toml", "[[budget]]\nmetric = \"wall_time_ns\"\nmax-value = \"1ns\"\n");
    let warn = ok(sb.haul(&p).args(["ci", "evaluate", "--format", "json"]));
    assert_eq!(serde_json::from_slice::<Value>(&warn.stdout).unwrap()["result"], "warn");

    // Enforcement on the ordinary cargo invocation: nonzero policy status, JSON stdout untouched.
    sb.file("p/.haul/budgets.toml", "[policy]\nmode = \"blocking\"\n\n[[budget]]\nmetric = \"wall_time_ns\"\nmax-value = \"1ns\"\n");
    sb.file("p/.haul/config.toml", "[ci]\nenforce-on-build = true\n");
    fs::write(p.join("src/lib.rs"), "pub fn add(x: u32, y: u32) -> u32 { y + x }\n").unwrap();
    let gated = run(sb.cargo(&p).args(["build", "--message-format=json"]));
    assert_eq!(gated.status.code(), Some(3), "{}", stderr(&gated));
    assert!(stderr(&gated).contains("haul: budget fail"), "{}", stderr(&gated));
    for line in stdout(&gated).lines() {
        let v: Value = serde_json::from_str(line).expect("stdout stays pure Cargo JSON");
        assert!(v["reason"].is_string());
    }
    assert!(stdout(&gated).contains("\"reason\":\"build-finished\",\"success\":true"), "Cargo itself succeeded");
    // A Cargo failure is never masked by the policy status.
    fs::write(p.join("src/lib.rs"), "this does not compile\n").unwrap();
    assert_eq!(run(sb.cargo(&p).arg("build")).status.code(), Some(101));
}

#[test]
fn inventory_and_gc_plan_are_read_only_and_conservative() {
    // AC-011, AC-012.
    let sb = Sandbox::new("gc");
    let ws = sb.workspace("ws");
    ok(sb.cargo(&ws).arg("build"));
    let inv = json_out(sb.haul(&ws).args(["inventory", "--format", "json"]));
    let root = &inv["build_directories"][0];
    let cats = root["categories"].as_object().unwrap();
    for c in ["final", "dependency", "metadata", "incremental", "fingerprint", "build-script-binary", "build-script-output", "proc-macro"] {
        assert!(cats.contains_key(c), "category {} missing from {:?}", c, cats.keys().collect::<Vec<_>>());
    }
    let sum: u64 = cats.values().map(|c| c["allocated_bytes"].as_u64().unwrap()).sum();
    assert_eq!(sum, root["allocated_bytes"].as_u64().unwrap(), "categories account for all bytes");
    assert_eq!(root["gc_plan"]["candidate_bytes"], 0, "a fresh build has nothing stale");

    // Plant stale output: an old unreferenced unit and an old unknown file.
    let deps = ws.join("target/debug/deps");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(120 * 86_400);
    for (path, size) in [(deps.join("libgone-0123456789abcdef.rlib"), 300_000usize), (ws.join("target/debug/odd/dir/unknown.bin"), 50_000)] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![0u8; size]).unwrap();
        fs::File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
    }
    let before = tree(&ws.join("target"));
    let plan = json_out(sb.haul(&ws).args(["gc", "--dry-run", "--format", "json", "--max-size", "1KiB"]));
    let p = &plan["build_directories"][0]["gc_plan"];
    let groups: Vec<&str> = p["candidates"].as_array().unwrap().iter().map(|c| c["group"].as_str().unwrap()).collect();
    assert_eq!(groups, vec!["unit:gone-0123456789abcdef"], "only the stale unit; live units and unknown files are never candidates");
    assert!(p["candidate_bytes"].as_u64().unwrap() >= 300_000);
    assert_eq!(p["size_budget"]["met_by_plan"], false, "the plan says so when stale output alone does not reach the budget");
    assert!(plan["build_directories"][0]["reclaimability"]["uncertain"]["logical_bytes"].as_u64().unwrap() >= 50_000);
    assert_eq!(tree(&ws.join("target")), before, "dry-run deleted something");
    let text = stdout(&ok(sb.haul(&ws).args(["gc", "--dry-run"])));
    assert!(text.contains("estimated reclaimable") && text.contains("Nothing was deleted"), "{}", text);
    // P0 has no deleting mode, and never plans implicitly.
    for args in [&["gc"][..], &["gc", "--apply"][..]] {
        let out = run(sb.haul(&ws).args(args));
        assert_eq!(out.status.code(), Some(2));
    }
    assert_eq!(tree(&ws.join("target")), before);
}

#[test]
fn gc_plan_is_blocked_while_a_build_is_running() {
    // QA-028 / FR-110 acceptance: an active build owns its files.
    let sb = Sandbox::new("gcrace");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).arg("build"));
    let stale = p.join("target/debug/deps/libgone-00000000.rlib");
    fs::write(&stale, vec![0u8; 100_000]).unwrap();
    fs::File::options().write(true).open(&stale).unwrap().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(90 * 86_400)).unwrap();
    let marker = sb.dir.join("started");
    sb.file("p/build.rs", &format!("fn main() {{\n    std::fs::write({:?}, \"x\").unwrap();\n    std::thread::sleep(std::time::Duration::from_secs(6));\n}}\n", marker));
    let mut build = sb.cargo(&p).arg("build").stderr(Stdio::null()).spawn().unwrap();
    while !marker.exists() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let during = json_out(sb.haul(&p).args(["gc", "--dry-run", "--format", "json"]));
    assert!(build.wait().unwrap().success());
    let plan = &during["build_directories"][0];
    assert!(plan["gc_plan"]["blocked_by"].is_string(), "{}", plan["gc_plan"]);
    assert_eq!(plan["gc_plan"]["reclaimable_now_bytes"], 0);
    assert!(plan["active"]["cargo_lock_held"] == true || !plan["active"]["haul_sessions"].as_array().unwrap().is_empty());
    let after = json_out(sb.haul(&p).args(["gc", "--dry-run", "--format", "json"]));
    assert!(after["build_directories"][0]["gc_plan"]["blocked_by"].is_null());
    assert!(after["build_directories"][0]["gc_plan"]["reclaimable_now_bytes"].as_u64().unwrap() >= 100_000);
}

#[test]
fn benchmark_scenarios_measure_in_an_owned_directory_and_restore_edits() {
    // FR-020, QA-029 (dirty working tree).
    let sb = Sandbox::new("bench");
    let ws = sb.workspace("ws");
    git(&ws, &["init", "-q"]);
    sb.file("ws/.gitignore", "target\n");
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "init"]);
    // Dirty tree: an uncommitted change the benchmark must leave exactly as it is.
    let lib = ws.join("util/src/lib.rs");
    fs::write(&lib, "pub fn double(x: u32) -> u32 { x * 2 } // uncommitted, no trailing newline").unwrap();
    let original = fs::read(&lib).unwrap();
    ok(sb.cargo(&ws).arg("build"));
    let user_target = tree(&ws.join("target"));

    let b = json_out(sb.haul(&ws).args(["benchmark", "--scenario", "no-change", "--reps", "2", "--warmups", "0", "--format", "json"]));
    assert_eq!(b["state"], "complete", "{}", b);
    assert_eq!((b["samples_valid"].as_u64(), b["samples_discarded"].as_u64()), (Some(2), Some(0)));
    assert_eq!(b["summary"]["samples"]["wall_time_ns"].as_array().unwrap().len(), 2, "raw samples are retained");
    assert_eq!(b["summary"]["metrics"]["units_compiled"], 0, "nothing compiles in a no-change run");
    assert_eq!(b["summary"]["scenario"], json!({ "name": "no-change", "cache_state": "warm" }));
    let phases: Vec<&str> = b["runs"].as_array().unwrap().iter().map(|r| r["phase"].as_str().unwrap()).collect();
    assert_eq!(phases, ["prime", "measured", "measured"]);
    let now = tree(&ws.join("target"));
    assert_eq!(now.iter().filter(|f| !f.starts_with("haul-bench/")).collect::<Vec<_>>(), user_target.iter().collect::<Vec<_>>(), "the user's own build output is untouched");
    assert!(now.iter().any(|f| f.starts_with("haul-bench/no-change-default/")));

    // Without permission and without a terminal, the edit scenario refuses and edits nothing.
    let refused = run(sb.haul(&ws).args(["benchmark", "--scenario", "small-edit", "--edit", "util/src/lib.rs", "--reps", "1", "--warmups", "0"]));
    assert_eq!(refused.status.code(), Some(2));
    assert!(stderr(&refused).contains("--allow-mutation"));
    assert_eq!(fs::read(&lib).unwrap(), original);

    let e = json_out(sb.haul(&ws).args(["benchmark", "--scenario", "small-edit", "--edit", "util/src/lib.rs", "--allow-mutation", "--reps", "2", "--warmups", "0", "--format", "json"]));
    assert_eq!(e["state"], "complete", "{}", e);
    assert_eq!(e["mutation"]["restored"], true);
    assert_eq!(fs::read(&lib).unwrap(), original, "the edited file is restored byte for byte, uncommitted change included");
    assert_eq!(e["summary"]["metrics"]["units_compiled"], 2, "util and its dependent app rebuild after each edit");
    assert!(tree(&sb.home).iter().all(|f| !f.contains("/restore/")), "the restore journal is gone after a clean finish");
    // Files outside the workspace are never edited.
    let outside = sb.file("outside.rs", "fn x() {}\n");
    let out = run(sb.haul(&ws).args(["benchmark", "--scenario", "small-edit", "--allow-mutation", "--edit"]).arg(&outside));
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("outside the workspace"));

    // Baselines can be promoted from a benchmark and compared against.
    ok(sb.haul(&ws).args(["baseline", "set", "edit-loop", "--benchmark", e["id"].as_str().unwrap()]));
    let cmp = json_out(sb.haul(&ws).args(["compare", "baseline:edit-loop", &format!("benchmark:{}", e["id"].as_str().unwrap()), "--format", "json"]));
    assert_eq!(cmp["comparability"]["state"], "comparable");
    let wall = cmp["metrics"].as_array().unwrap().iter().find(|m| m["metric"] == "wall_time_ns").unwrap();
    assert_eq!(wall["delta"], 0);
}

#[test]
fn clean_scenario_and_failed_builds() {
    let sb = Sandbox::new("benchclean");
    let p = sb.package("p", "p");
    let b = json_out(sb.haul(&p).args(["benchmark", "--scenario", "clean", "--reps", "2", "--warmups", "0", "--format", "json"]));
    assert_eq!(b["state"], "complete");
    assert_eq!(b["summary"]["metrics"]["units_compiled"], 2, "every repetition of a clean build compiles everything");
    assert_eq!(b["summary"]["scenario"]["cache_state"], "cold");
    assert!(!p.join("target/debug").exists(), "a benchmark does not build into the user's own profile directory");
    // A build failure makes the benchmark invalid, never a fast sample.
    fs::write(p.join("src/lib.rs"), "does not compile\n").unwrap();
    let out = run(sb.haul(&p).args(["benchmark", "--scenario", "clean", "--reps", "1", "--warmups", "0", "--format", "json"]));
    assert_eq!(out.status.code(), Some(1));
    let failed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(failed["state"], "invalid");
    assert_eq!(failed["samples_valid"], 0);
    assert!(failed["runs"][0]["invalid_reason"].as_str().unwrap().contains("status 101"));
}

#[test]
fn experiment_rejects_a_candidate_that_fails_verification() {
    // FR-080 acceptance: a faster candidate that fails a project test is invalid.
    let sb = Sandbox::new("experiment");
    let p = sb.package("p", "p");
    sb.file("p/src/lib.rs", "pub fn add(x: u32, y: u32) -> u32 { x + y }\n\n#[test]\nfn needs_debug_assertions() {\n    assert!(cfg!(debug_assertions));\n}\n");
    ok(sb.upstream(&p).arg("generate-lockfile"));
    let before = tree(&p).into_iter().filter(|f| !f.starts_with("target/")).collect::<Vec<_>>();
    let common = ["--scenario", "no-change", "--reps", "1", "--warmups", "0", "--verify", "test", "--format", "json"];
    let bad = run(sb.haul(&p).args(["experiment", "--name", "no-asserts", "--set", "profile.dev.debug-assertions=false"]).args(common).args(["--", "build"]));
    let r: Value = serde_json::from_slice(&bad.stdout).unwrap_or_else(|_| panic!("{}", stderr(&bad)));
    assert_eq!(r["status"], "invalid", "{}", r["reason"]);
    assert_eq!((r["verification"]["performed"].as_bool(), r["verification"]["passed"].as_bool()), (Some(true), Some(false)));
    assert!(r["proposed_config"].is_null(), "an invalid candidate proposes nothing");
    assert_eq!(bad.status.code(), Some(1));

    // A candidate that passes verification reports build and runtime dimensions separately.
    let good = json_out(sb.haul(&p).args(["experiment", "codegen", "--codegen-units", "4"]).args(common).args(["--", "build"]));
    assert_eq!(good["verification"]["passed"], true);
    assert!(matches!(good["status"].as_str(), Some("verified" | "inconclusive")), "{}", good["status"]);
    assert_eq!(good["candidate_settings"]["cargo_config"], json!(["profile.dev.codegen-units=4"]));
    assert!(good["dimensions"]["build"].is_object());
    assert!(good["dimensions"]["runtime_and_debugging"][0]["risk"].as_str().unwrap().contains("generated-code performance"));
    // The overlay was ephemeral: no file in the repository changed.
    assert_eq!(tree(&p).into_iter().filter(|f| !f.starts_with("target/")).collect::<Vec<_>>(), before);
    // An experiment without a candidate setting is an error, not a self-comparison.
    assert_eq!(run(sb.haul(&p).args(["experiment", "--", "build"])).status.code(), Some(2));
}

#[test]
fn worktrees_report_identical_units_built_twice() {
    // AC-013 / FR-120 acceptance.
    let sb = Sandbox::new("worktrees");
    // A dependency outside the repository, shared by absolute path: the same unit in both checkouts.
    let shared = sb.package("vendor/shared", "shared");
    fs::remove_file(shared.join("src/main.rs")).unwrap();
    let repo = sb.dir.join("repo");
    sb.file("repo/Cargo.toml", &format!("[package]\nname = \"proj\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nshared = {{ path = {:?} }}\n", shared));
    sb.file("repo/src/lib.rs", "pub fn f() -> u32 { shared::add(1, 1) }\n");
    sb.file("repo/.gitignore", "target\n");
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    let wt = sb.dir.join("repo-feature");
    git(&repo, &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feature"]);
    ok(sb.cargo(&repo).arg("build"));
    ok(sb.cargo(&wt).arg("build"));
    let w = json_out(sb.haul(&repo).args(["analyze", "worktrees", "--format", "json"]));
    let other = w["others"].as_array().unwrap().iter().find(|o| o["root"].as_str().unwrap().ends_with("repo-feature")).expect("the linked worktree is found");
    assert_eq!(other["same_repository"], true);
    assert_eq!(other["state"], "isolated");
    // Same commit in both checkouts: the shared dependency and the project itself are identical units.
    assert_eq!(other["identical_units"], 2);
    assert_eq!(other["sample"], json!(["proj@0.1.0", "shared@0.1.0"]));
    assert_eq!(w["summary"]["duplicate_units"], 2);
    assert!(w["summary"]["duplicate_compile_ns"].as_u64().unwrap() > 0);
    // Once the branches diverge, only the third-party unit is still the same work done twice.
    fs::write(wt.join("src/lib.rs"), "pub fn f() -> u32 { shared::add(2, 2) }\n").unwrap();
    ok(sb.cargo(&wt).arg("build"));
    let w = json_out(sb.haul(&repo).args(["analyze", "worktrees", "--format", "json"]));
    let other = w["others"].as_array().unwrap().iter().find(|o| o["root"].as_str().unwrap().ends_with("repo-feature")).unwrap();
    assert_eq!(other["sample"], json!(["shared@0.1.0"]), "a unit with different source content is not a duplicate");
    assert_eq!(w["summary"]["duplicate_units"], 1);
    // FR-120.3: the advice never proposes one shared writable target directory.
    let advice = w["advice"].to_string();
    assert!(advice.contains("sccache") && advice.contains("does not point several worktrees at one writable target directory"), "{}", advice);
    let text = stdout(&ok(sb.haul(&repo).args(["analyze", "worktrees"])));
    assert!(text.contains("potential cross-worktree reuse: 1 unit(s)"), "{}", text);
}

fn sccache_available() -> bool {
    std::process::Command::new("sccache").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

#[test]
fn sccache_policy_statistics_conflict_and_safe_remediation() {
    // QA-013, AC-009, AC-010, FR-090.2, FR-170 with a real, isolated sccache server.
    if !sccache_available() {
        eprintln!("skipping: sccache not installed");
        return;
    }
    let sb = Sandbox::new("sccache");
    let ws = sb.workspace("ws");
    let port = 20_000 + (std::process::id() % 20_000);
    let isolate = |c: &mut std::process::Command| {
        c.env("SCCACHE_DIR", sb.dir.join("sccache-cache")).env("SCCACHE_SERVER_PORT", port.to_string()).env("SCCACHE_IDLE_TIMEOUT", "60");
    };
    struct StopServer(std::path::PathBuf, u32);
    impl Drop for StopServer {
        fn drop(&mut self) {
            let _ = std::process::Command::new("sccache").arg("--stop-server").env("SCCACHE_DIR", &self.0).env("SCCACHE_SERVER_PORT", self.1.to_string()).output();
        }
    }
    let _stop = StopServer(sb.dir.join("sccache-cache"), port);

    sb.file("ws/.haul/config.toml", "[cache]\nprovider = \"sccache\"\n");
    let mut build = sb.cargo(&ws);
    isolate(build.arg("build"));
    ok(&mut build);
    let mut rep = sb.haul(&ws);
    isolate(rep.args(["report", "--format", "json"]));
    let r = json_out(&mut rep);
    assert_eq!(r["policy"]["cache"]["state"], "local");
    assert!(r["run"]["wrappers"]["inner"].as_str().unwrap().ends_with("sccache"));
    assert_eq!(r["run"]["wrappers"]["inner_source"], "policy cache.provider");
    // AC-009: requests, hits, misses and non-cacheable are separate numbers.
    let c = &r["cache"];
    assert_eq!(c["provider"], "sccache");
    assert!(c["requests"].as_u64().unwrap() >= 5, "{}", c);
    assert_eq!(c["hits"], 0);
    assert!(c["non_cacheable"].as_u64().unwrap() >= 1, "incremental and linking units are counted apart from misses: {}", c);
    assert!(c["read_bytes"].is_null() && c["unavailable"]["read_bytes"].is_string());
    // AC-010: the incremental/sccache conflict is detected and explained.
    let inc = &r["policy"]["incremental"];
    assert_eq!(inc["sccache_conflict"], true);
    assert!(inc["explanation"].as_str().unwrap().contains("sccache cannot cache incremental"));
    assert_eq!(inc["decision"], "cargo-default", "enabling sccache did not silently turn incremental off");
    let rec = r["recommendations"].as_array().unwrap().iter().find(|x| x["rule"] == "cache_strategy_choice").expect("cache strategy choice");
    assert_eq!(rec["alternatives"][0]["id"], "keep-incremental", "on a developer machine keeping incremental is the first alternative");
    assert!(rec["impact"].get("avoidable_time_ns").is_none());

    // Preview the other alternative, apply it (a change to Haul's own config is `safe`), verify, roll back.
    let mut fix = sb.haul(&ws);
    isolate(fix.args(["fix", "--dry-run", "rec:cache-strategy:incremental-vs-sccache", "--option", "disable-incremental", "--format", "json"]));
    let preview = json_out(&mut fix);
    assert_eq!((preview[0]["class"].as_str(), preview[0]["file"].as_str()), (Some("safe"), Some(".haul/config.toml")));
    assert!(preview[0]["diff"].as_str().unwrap().contains("+local-dev = \"off\""));
    assert_eq!(fs::read_to_string(ws.join(".haul/config.toml")).unwrap(), "[cache]\nprovider = \"sccache\"\n", "the preview applied nothing");
    let id = preview[0]["id"].as_str().unwrap();
    ok(sb.haul(&ws).args(["fix", "--apply", id]));
    assert_eq!(fs::read_to_string(ws.join(".haul/config.toml")).unwrap(), "[cache]\nprovider = \"sccache\"\n\n[incremental]\nlocal-dev = \"off\"\n");

    // With the policy in place the change is recorded, units are no longer incremental, and a clean rebuild hits the cache.
    for _ in 0..2 {
        fs::remove_dir_all(ws.join("target")).unwrap();
        let mut b = sb.cargo(&ws);
        isolate(b.arg("build"));
        ok(&mut b);
    }
    let mut rep = sb.haul(&ws);
    isolate(rep.args(["report", "--format", "json"]));
    let r2 = json_out(&mut rep);
    let inc2 = &r2["policy"]["incremental"];
    assert_eq!((inc2["decision"].as_str(), inc2["applied_env"].as_str()), (Some("force-off"), Some("CARGO_INCREMENTAL=0")));
    assert!(inc2["rationale"].as_str().unwrap().contains(".haul/config.toml"), "the decision names its origin: {}", inc2["rationale"]);
    assert_eq!(inc2["sccache_conflict"], false);
    assert_eq!(inc2["observed"]["units_incremental"], 0);
    assert!(r2["cache"]["hits"].as_u64().unwrap() >= 2, "library units are served from the cache on the second clean build: {}", r2["cache"]);
    let text = {
        let mut c = sb.haul(&ws);
        isolate(c.args(["analyze", "cache"]));
        stdout(&ok(&mut c))
    };
    assert!(text.contains(&format!("{}/{} cacheable requests", r2["cache"]["hits"], r2["cache"]["cacheable_requests"])), "{}", text);
    // An explicit user choice outranks the policy and is recorded as such.
    let mut explicit = sb.cargo(&ws);
    isolate(explicit.arg("build").env("CARGO_INCREMENTAL", "1"));
    ok(&mut explicit);
    let last = sb.sessions().pop().unwrap();
    assert_eq!(last["policy"]["incremental"]["decision"], "cargo-default");
    assert!(last["policy"]["incremental"]["rationale"].as_str().unwrap().contains("explicit CARGO_INCREMENTAL=1 is respected"));

    ok(sb.haul(&ws).args(["fix", "--rollback", id]));
    assert_eq!(fs::read_to_string(ws.join(".haul/config.toml")).unwrap(), "[cache]\nprovider = \"sccache\"\n");
}

#[test]
fn text_and_html_reports_render_every_section() {
    let sb = Sandbox::new("render");
    let ws = sb.workspace("ws");
    ok(sb.cargo(&ws).arg("build"));
    let text = stdout(&ok(sb.haul(&ws).arg("analyze")));
    for heading in ["Timing", "Rebuild reasons", "Duplication", "Build scripts", "Procedural macros", "Codegen and linking", "Compiler cache", "Storage", "Recommendations"] {
        assert!(text.contains(heading), "section {} missing", heading);
    }
    assert!(text.contains("Proc-macro execution timing\n    Unavailable:"), "unavailable data is labelled as such");
    assert!(text.contains("Link phase timing\n    Unavailable:"));
    ok(sb.haul(&ws).args(["report", "--format", "html", "-o", "report.html"]));
    let html = fs::read_to_string(ws.join("report.html")).unwrap();
    assert!(html.starts_with("<!doctype html>") && html.contains("<html lang=\"en\">"));
    assert!(html.contains("<caption>") && html.contains("<th scope=\"col\">") && !html.contains("<script"), "tables with headers, no scripts");
    for section in ["analyze timing", "analyze rebuilds", "analyze scripts", "analyze proc-macros", "analyze codegen", "analyze storage", "analyze recommendations", "runs", "inspect", "policy explain", "cache status"] {
        ok(sb.haul(&ws).args(section.split(' ')));
    }
    assert_eq!(run(sb.haul(&ws).args(["analyze", "nonsense"])).status.code(), Some(2));
    assert_eq!(run(sb.haul(&ws).args(["analyze", "--bogus-flag"])).status.code(), Some(2));
}

#[test]
fn analysis_without_any_run_explains_what_to_do() {
    let sb = Sandbox::new("norun");
    let p = sb.package("p", "p");
    let out = run(sb.haul(&p).arg("analyze"));
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("no recorded build runs"), "{}", stderr(&out));
    assert!(stdout(&ok(sb.haul(&p).arg("runs"))).contains("No recorded runs"));
}

#[test]
fn policy_explain_shows_each_setting_with_its_origin() {
    // CFG-001, CFG-002.
    let sb = Sandbox::new("policy");
    let p = sb.package("p", "p");
    sb.file("p/.haul/config.toml", "[cache]\nfail = \"closed\"\n");
    sb.file("haul-home/config.toml", "[artifacts]\nstale-days = 7\n");
    let v = json_out(sb.haul(&p).args(["policy", "explain", "--format", "json"]).env("HAUL_INCREMENTAL_CI", "off").env("HAUL_REPORT", "r.json"));
    let get = |key: &str| v["settings"].as_array().unwrap().iter().find(|s| s["key"] == key).unwrap().clone();
    assert_eq!((get("cache.fail")["value"].as_str(), get("cache.fail")["origin"].as_str()), (Some("\"closed\""), Some("$WORKSPACE/.haul/config.toml")));
    assert_eq!(get("artifacts.stale-days")["value"], "7");
    assert!(get("artifacts.stale-days")["origin"].as_str().unwrap().ends_with("config.toml"));
    assert_eq!(get("incremental.ci")["origin"], "env HAUL_INCREMENTAL_CI");
    assert_eq!(get("measurement.report")["origin"], "env HAUL_REPORT");
    assert_eq!(get("cache.provider")["origin"], "default");
    // Repository settings override user settings; the environment overrides both.
    sb.file("p/.haul/config.toml", "[artifacts]\nstale-days = 3\n");
    let v2 = json_out(sb.haul(&p).args(["policy", "explain", "--format", "json"]));
    assert_eq!(v2["settings"].as_array().unwrap().iter().find(|s| s["key"] == "artifacts.stale-days").unwrap()["value"], "3");
    let v3 = json_out(sb.haul(&p).args(["policy", "explain", "--format", "json"]).env("HAUL_ARTIFACTS_STALE_DAYS", "1"));
    assert_eq!(v3["settings"].as_array().unwrap().iter().find(|s| s["key"] == "artifacts.stale-days").unwrap()["value"], "1");
    // A misspelt variable in Haul's namespace is an error rather than a silently ignored setting.
    let bad = run(sb.haul(&p).args(["policy", "explain"]).env("HAUL_CACHE_PROVIDR", "sccache"));
    assert_eq!(bad.status.code(), Some(2));
    assert!(stderr(&bad).contains("HAUL_CACHE_PROVIDR"));
}

#[test]
fn doctor_install_and_uninstall() {
    // MIG-001, MIG-002.
    let sb = Sandbox::new("install");
    let p = sb.package("p", "p");
    let doctor = stdout(&ok(sb.haul(&p).arg("doctor")));
    for item in ["shim path", "real Cargo path", "rustup", "active toolchain", "CARGO ", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "target directory", "build directory", "cache policy", "capabilities", "No problems found."] {
        assert!(doctor.contains(item), "doctor output lacks `{}`:\n{}", item, doctor);
    }
    let dir = sb.dir.join("installed-shim");
    let out = stdout(&ok(sb.haul(&p).args(["install-shim", "--dir"]).arg(&dir)));
    assert!(out.contains("export PATH="));
    let shim = dir.join("cargo");
    assert_eq!(fs::canonicalize(&shim).unwrap(), fs::canonicalize(env!("CARGO_BIN_EXE_haul")).unwrap());
    // The installed shim works as cargo.
    let mut path = vec![dir.clone()];
    path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let via = run(std::process::Command::new("cargo").current_dir(&p).arg("--version").env("PATH", std::env::join_paths(path).unwrap()).env("HAUL_HOME", &sb.home));
    assert!(stdout(&via).starts_with("cargo 1."), "{}", stdout(&via));
    // A second install is refused; an existing cargo is never overwritten.
    assert_eq!(run(sb.haul(&p).args(["install-shim", "--dir"]).arg(&dir)).status.code(), Some(2));
    let occupied = sb.file("occupied/cargo", "#!/bin/sh\necho someone-elses-cargo\n");
    ok(sb.haul(&p).arg("uninstall-shim"));
    assert!(!shim.exists() && !dir.exists(), "uninstall removes exactly what install created");
    let refused = run(sb.haul(&p).args(["install-shim", "--dir"]).arg(occupied.parent().unwrap()));
    assert_eq!(refused.status.code(), Some(2));
    assert_eq!(fs::read_to_string(&occupied).unwrap(), "#!/bin/sh\necho someone-elses-cargo\n");
    assert_eq!(run(sb.haul(&p).arg("uninstall-shim")).status.code(), Some(2), "nothing recorded, nothing to remove");
}

#[test]
fn retention_bounds_recorded_state() {
    // NFR-SPACE-001 / R-014.
    let sb = Sandbox::new("retention");
    let p = sb.package("p", "p");
    sb.file("haul-home/config.toml", "[measurement]\nkeep-runs = 3\nkeep-raw-events = 1\nfinalize = \"sync\"\n");
    for i in 0..6 {
        fs::write(p.join("src/lib.rs"), format!("pub fn add(x: u32, y: u32) -> u32 {{ x + y + {} }}\n", i)).unwrap();
        ok(sb.cargo(&p).arg("build"));
    }
    let store = &sb.store_dirs()[0];
    let count = |sub: &str| fs::read_dir(store.join(sub)).map(|d| d.count()).unwrap_or(0);
    assert_eq!(count("sessions"), 3);
    assert_eq!(count("runs"), 3);
    assert_eq!(count("events"), 1, "raw events are kept for fewer runs than summaries (DATA-009)");
    // The unit index still explains the latest rebuild after older runs were pruned.
    assert_eq!(reasons(&sb.report(&p, "latest"))[0].2, "dependency");
}

#[test]
fn stale_session_filesystem_facts_are_marked_unavailable() {
    // A run analysed after later builds must not present today's directory as its own footprint.
    let sb = Sandbox::new("lazy");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).arg("build"));
    fs::write(p.join("src/lib.rs"), "pub fn add(x: u32, y: u32) -> u32 { y + x }\n").unwrap();
    ok(sb.cargo(&p).arg("build"));
    let old = sb.report(&p, "latest~1");
    assert!(old["artifacts"].is_null() && old["metrics"]["artifact_allocated_bytes"].is_null());
    assert!(old["unavailable"]["artifacts"].as_str().unwrap().contains("later build"));
    assert_eq!(old["metrics"]["units_compiled"], 2, "rustc-level evidence was recorded at build time and is intact");
    let latest = sb.report(&p, "latest");
    assert!(latest["metrics"]["artifact_allocated_bytes"].as_u64().unwrap() > 0);
    assert_eq!(reasons(&latest), vec![("p".into(), "bin".into(), "dependency".into()), ("p".into(), "lib".into(), "source".into())]);
}

#[test]
fn fast_mode_applies_its_settings_unless_the_user_made_them() {
    let sb = Sandbox::new("fast");
    let p = sb.package("p", "p");
    let debuginfo = |r: &Value| -> Vec<String> { r["units"].as_array().unwrap().iter().filter(|u| u["freshness"] == "compiled").map(|u| u["codegen"]["debuginfo"].as_str().unwrap_or("0").to_string()).collect() };

    ok(sb.cargo(&p).arg("build"));
    let r = sb.report(&p, "latest");
    assert!(!debuginfo(&r).is_empty() && debuginfo(&r).iter().all(|d| d == "line-tables-only"), "{:?}", debuginfo(&r));
    assert_eq!(r["policy"]["fast"]["applied"].as_array().unwrap().len(), 2);
    assert_eq!(r["run"]["command"]["argv_redacted"], json!(["cargo", "build"]), "the record shows the user's command, not Haul's overlays");

    // compat: exactly what upstream Cargo builds.
    ok(sb.cargo(&p).arg("build").env("HAUL_MODE", "compat"));
    let r = sb.report(&p, "latest");
    assert!(debuginfo(&r).iter().all(|d| d == "2"), "{:?}", debuginfo(&r));
    assert!(r["policy"]["fast"]["applied"].as_array().unwrap().is_empty());

    // A setting the user made wins, and the report says why Haul stood back.
    let manifest = p.join("Cargo.toml");
    let text = fs::read_to_string(&manifest).unwrap();
    fs::write(&manifest, format!("{}\n[profile.dev]\ndebug = 1\n", text)).unwrap();
    ok(sb.cargo(&p).arg("build"));
    let r = sb.report(&p, "latest");
    assert!(debuginfo(&r).iter().all(|d| d == "1"), "{:?}", debuginfo(&r));
    let yielded = r["policy"]["fast"]["yielded"].as_array().unwrap();
    assert_eq!(yielded.len(), 2);
    assert!(yielded[0]["because"].as_str().unwrap().contains("Cargo.toml"), "{}", yielded[0]);
}

/// Events of the latest instrumented run: (crate name, cache result).
fn cache_results(sb: &Sandbox) -> Vec<(String, String)> {
    let s = sb.sessions().into_iter().rev().find(|s| s["instrumented"] == json!(true)).unwrap();
    let dir = std::path::PathBuf::from(s["events_dir"].as_str().unwrap());
    let mut out: Vec<(String, String)> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| serde_json::from_slice::<Value>(&fs::read(e.unwrap().path()).ok()?).ok())
        .filter(|ev| ev["kind"] == "compile")
        .map(|ev| {
            let args: Vec<String> = ev["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_string()).collect();
            let name = args.iter().position(|a| a == "--crate-name").map(|i| args[i + 1].clone()).unwrap_or_default();
            (name, ev["cache"]["result"].as_str().unwrap_or("").to_string())
        })
        .collect();
    out.sort();
    out
}

#[test]
fn artifact_cache_restores_dependencies_across_cleans_and_checkouts() {
    let sb = Sandbox::new("artifacts");
    // A dependency whose sources sit in the (sandbox) Cargo home's registry: immutable, cacheable.
    // Its warning must reach the user on every build, cached or not.
    let dep = sb.dir.join("cargo-home/registry/src/fake-0000/greet-1.0.0");
    sb.file("cargo-home/registry/src/fake-0000/greet-1.0.0/Cargo.toml", "[package]\nname = \"greet\"\nversion = \"1.0.0\"\nedition = \"2021\"\n");
    // Like serde: a build script generates code that the library includes through OUT_DIR,
    // whose path differs in every target directory.
    sb.file("cargo-home/registry/src/fake-0000/greet-1.0.0/build.rs", "fn main() {\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    std::fs::write(format!(\"{out}/greeting.rs\"), \"const GREETING: &str = \\\"hello from the cache\\\";\").unwrap();\n    println!(\"cargo::rerun-if-changed=build.rs\");\n}\n");
    sb.file("cargo-home/registry/src/fake-0000/greet-1.0.0/src/lib.rs", "include!(concat!(env!(\"OUT_DIR\"), \"/greeting.rs\"));\n\npub fn greet() -> &'static str {\n    let unused = 1;\n    GREETING\n}\n");
    let app = |rel: &str| {
        // A path dependency is compiled incrementally, which a registry one never is.
        sb.file(&format!("{}/Cargo.toml", rel), &format!("[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ngreet = {{ path = {:?} }}\n\n[profile.dev.package.greet]\nincremental = false\n", dep.display().to_string()));
        sb.file(&format!("{}/src/main.rs", rel), "fn main() {\n    println!(\"{}\", greet::greet());\n}\n");
        sb.dir.join(rel)
    };
    let (a, b) = (app("a"), app("b"));

    let first = ok(sb.cargo(&a).arg("build"));
    assert!(stderr(&first).contains("unused variable"), "{}", stderr(&first));
    assert_eq!(cache_results(&sb), vec![("app".to_string(), "stored".to_string()), ("build_script_build".to_string(), "stored".to_string()), ("greet".to_string(), "stored".to_string())]);

    ok(sb.cargo(&a).arg("clean"));
    let again = ok(sb.cargo(&a).args(["run", "-q"]));
    assert_eq!(stdout(&again), "hello from the cache\n");
    assert!(stderr(&again).contains("unused variable"), "a cached unit's warnings are replayed: {}", stderr(&again));
    assert!(cache_results(&sb).iter().all(|(_, r)| r == "hit"), "after a clean, the same checkout is restored entirely: {:?}", cache_results(&sb));

    // A workspace source that changed is never served from the cache; reverting it is.
    let main = a.join("src/main.rs");
    let original = fs::read_to_string(&main).unwrap();
    fs::write(&main, original.replace("greet::greet()", "greet::greet().to_uppercase()")).unwrap();
    assert_eq!(stdout(&ok(sb.cargo(&a).args(["run", "-q"]))), "HELLO FROM THE CACHE\n");
    assert!(cache_results(&sb).contains(&("app".to_string(), "stored".to_string())));
    fs::write(&main, &original).unwrap();
    assert_eq!(stdout(&ok(sb.cargo(&a).args(["run", "-q"]))), "hello from the cache\n");
    assert!(cache_results(&sb).contains(&("app".to_string(), "hit".to_string())));

    // Another checkout reuses the dependency, never the workspace crate (its debug info names its checkout),
    // and Cargo then considers everything fresh.
    ok(sb.cargo(&b).arg("build"));
    assert!(cache_results(&sb).contains(&("greet".to_string(), "hit".to_string())));
    assert!(cache_results(&sb).contains(&("app".to_string(), "stored".to_string())));
    let fresh = ok(sb.cargo(&b).arg("build"));
    assert!(!stderr(&fresh).contains("Compiling"), "a restored unit must be fresh for Cargo afterwards: {}", stderr(&fresh));
    assert_eq!(stdout(&ok(sb.cargo(&b).args(["run", "-q"]))), "hello from the cache\n");

    // compat mode never uses the cache.
    ok(sb.cargo(&a).arg("clean"));
    ok(sb.cargo(&a).arg("build").env("HAUL_MODE", "compat"));
    assert!(cache_results(&sb).iter().all(|(_, r)| r == "off"), "{:?}", cache_results(&sb));
}
