//! End-to-end checks of the management surface: reports, rebuild analysis,
//! budgets, storage planning, benchmarks, experiments, worktrees, remediation.

#![cfg(unix)]

mod common;

use common::*;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
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
    ok(sb.cargo(&ws).arg("build").env("QRUST_REPORT", &sidecar));
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
    let a = ok(sb.qrust(&ws).args(["report", "--format", "json"]));
    let b = ok(sb.qrust(&ws).args(["report", "--format", "json"]));
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
    assert!(r["rebuilds"]["units"][0]["detail"].as_str().unwrap().contains("[] -> [extra]"));

    // Flags from the environment.
    ok(sb.cargo(&ws).args(["build", "-p", "util"]).env("RUSTFLAGS", "--cfg qrust_test_flag"));
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
    let dup = json_out(sb.qrust(&ws).args(["analyze", "duplication", "--format", "json"]));
    let util_group = dup["variants"].as_array().unwrap().iter().find(|g| g["package"] == "util").expect("util variants are reported");
    let causes = util_group["causes"].to_string();
    assert!(causes.contains("feature-set") && causes.contains("profile"), "{}", causes);
}

#[test]
fn toolchain_change_is_reported_as_compiler_drift() {
    // FR-040 acceptance.
    let sb = Sandbox::new("drift");
    let p = sb.package("p", "p");
    // A second compiler: a pinned release, installed once into the suites' toolchain store.
    ok(sb.qrust(&p).args(["toolchain", "install", "1.80.0", "--profile", "minimal"]));
    ok(sb.cargo(&p).args(["check", "--lib"]));
    ok(sb.cargo(&p).args(["+1.80.0", "check", "--lib"]));
    let r = sb.report(&p, "latest");
    assert_eq!(reasons(&r), vec![("p".into(), "lib".into(), "compiler".into())]);
    // FR-160 acceptance: comparing the two runs states the compiler difference before any delta.
    let cmp = json_out(sb.qrust(&p).args(["compare", "latest~1", "latest", "--format", "json"]));
    assert_eq!(cmp["comparability"]["state"], "incomparable");
    assert!(cmp["comparability"]["differences"].as_array().unwrap().iter().any(|d| d["what"] == "compiler version"));
    let text = stdout(&ok(sb.qrust(&p).args(["compare", "latest~1", "latest"])));
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
    let text = stdout(&ok(sb.qrust(&ws).args(["fix", "--dry-run"])));
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
        .env("QRUST_REPORT", &sidecar));
    let mut haystack = vec![fs::read_to_string(&sidecar).unwrap()];
    for f in tree(&sb.home) {
        haystack.push(String::from_utf8_lossy(&fs::read(sb.home.join(&f)).unwrap()).into_owned());
    }
    haystack.push(stdout(&ok(sb.qrust(&p).args(["report"]))));
    haystack.push(stdout(&ok(sb.qrust(&p).args(["report", "--format", "html"]))));
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
    let set = stdout(&ok(sb.qrust(&p).args(["baseline", "set", "main"])));
    assert!(set.contains("Baseline `main` set"));
    let baseline: Value = serde_json::from_slice(&fs::read(p.join(".qrust/baselines/main.json")).unwrap()).unwrap();
    assert_eq!(baseline["baseline"]["name"], "main");
    assert!(!fs::read_to_string(p.join(".qrust/baselines/main.json")).unwrap().contains(sb.dir.to_str().unwrap()), "a committed baseline must not embed local paths");

    // No budget file yet: an explicit error, not a silent pass.
    let missing = run(sb.qrust(&p).args(["ci", "evaluate"]));
    assert_eq!(missing.status.code(), Some(2));
    assert!(stderr(&missing).contains("budgets.toml"));

    // A generous relative budget passes against the baseline taken from this very run.
    sb.file("p/.qrust/budgets.toml", "[policy]\nmode = \"blocking\"\n\n[[budget]]\nmetric = \"wall_time_ns\"\nmax-increase-percent = 50\n\n[[budget]]\nmetric = \"artifact_allocated_bytes\"\nmax-value = \"10GiB\"\n");
    let pass = ok(sb.qrust(&p).args(["ci", "evaluate", "--format", "json"]));
    assert_eq!(serde_json::from_slice::<Value>(&pass.stdout).unwrap()["result"], "pass");

    // An impossible absolute budget fails with the policy exit status.
    sb.file("p/.qrust/budgets.toml", "[policy]\nmode = \"blocking\"\n\n[[budget]]\nmetric = \"wall_time_ns\"\nmax-value = \"1ns\"\n");
    let fail = run(sb.qrust(&p).args(["ci", "evaluate"]));
    assert_eq!(fail.status.code(), Some(3), "{}", stdout(&fail));
    assert!(stdout(&fail).contains("Budget result: FAIL"));
    // Warn-only rollout: same violation, reported, not blocking.
    sb.file("p/.qrust/budgets.toml", "[[budget]]\nmetric = \"wall_time_ns\"\nmax-value = \"1ns\"\n");
    let warn = ok(sb.qrust(&p).args(["ci", "evaluate", "--format", "json"]));
    assert_eq!(serde_json::from_slice::<Value>(&warn.stdout).unwrap()["result"], "warn");

    // Enforcement on the ordinary cargo invocation: nonzero policy status, JSON stdout untouched.
    sb.file("p/.qrust/budgets.toml", "[policy]\nmode = \"blocking\"\n\n[[budget]]\nmetric = \"wall_time_ns\"\nmax-value = \"1ns\"\n");
    sb.file("p/.qrust/config.toml", "[ci]\nenforce-on-build = true\n");
    fs::write(p.join("src/lib.rs"), "pub fn add(x: u32, y: u32) -> u32 { y + x }\n").unwrap();
    let gated = run(sb.cargo(&p).args(["build", "--message-format=json"]));
    assert_eq!(gated.status.code(), Some(3), "{}", stderr(&gated));
    assert!(stderr(&gated).contains("FAIL budget: "), "{}", stderr(&gated));
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
fn inventory_and_its_stale_plan_are_read_only_and_conservative() {
    // AC-011, AC-012.
    let sb = Sandbox::new("gc");
    let ws = sb.workspace("ws");
    ok(sb.cargo(&ws).arg("build"));
    let inv = json_out(sb.qrust(&ws).args(["inventory", "--format", "json"]));
    let root = &inv["build_directories"][0];
    let cats = root["categories"].as_object().unwrap();
    for c in ["final", "dependency", "metadata", "incremental", "fingerprint", "build-script-binary", "build-script-output", "proc-macro"] {
        assert!(cats.contains_key(c), "category {} missing from {:?}", c, cats.keys().collect::<Vec<_>>());
    }
    let sum: u64 = cats.values().map(|c| c["allocated_bytes"].as_u64().unwrap()).sum();
    assert_eq!(sum, root["allocated_bytes"].as_u64().unwrap(), "categories account for all bytes");
    assert_eq!(root["gc_plan"]["candidate_bytes"], 0, "a fresh build has nothing stale");

    // Plant stale output: an old unreferenced unit and an old unknown file.
    let deps = sb.target_dir(&ws).join("debug/deps");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(120 * 86_400);
    for (path, size) in [(deps.join("libgone-0123456789abcdef.rlib"), 300_000usize), (sb.target_dir(&ws).join("debug/odd/dir/unknown.bin"), 50_000)] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![0u8; size]).unwrap();
        fs::File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
    }
    let before = tree(&sb.target_dir(&ws));
    let plan = json_out(sb.qrust(&ws).args(["inventory", "--format", "json", "--max-size", "1KiB"]));
    let p = &plan["build_directories"][0]["gc_plan"];
    let groups: Vec<&str> = p["candidates"].as_array().unwrap().iter().map(|c| c["group"].as_str().unwrap()).collect();
    assert_eq!(groups, vec!["unit:gone-0123456789abcdef"], "only the stale unit; live units and unknown files are never candidates");
    assert!(p["candidate_bytes"].as_u64().unwrap() >= 300_000);
    assert_eq!(p["size_budget"]["met_by_plan"], false, "the plan says so when stale output alone does not reach the budget");
    assert!(plan["build_directories"][0]["reclaimability"]["uncertain"]["logical_bytes"].as_u64().unwrap() >= 50_000);
    assert_eq!(tree(&sb.target_dir(&ws)), before, "dry-run deleted something");
    let text = stdout(&ok(sb.qrust(&ws).arg("inventory")));
    assert!(text.contains("estimated reclaimable") && text.contains("Nothing was deleted"), "{}", text);
    assert_eq!(tree(&sb.target_dir(&ws)), before);
}

#[test]
fn what_qrust_starts_in_the_background_is_qrust_whatever_name_it_runs_by() {
    // On Windows QuickRust's `cargo` is a hard link, and the running binary's path names it
    // `cargo.exe`; a hard link here makes the same so. It is made beside the binary (a link must
    // be on its filesystem); writing a copy instead could leave it busy for a parallel test's
    // fork when it is run.
    let sb = Sandbox::new("itself");
    let p = sb.package("p", "p");
    let linked = std::path::Path::new(env!("CARGO_BIN_EXE_qrust")).with_file_name(format!("as-cargo-{}", std::process::id()));
    let _ = fs::remove_dir_all(&linked);
    fs::create_dir_all(&linked).unwrap();
    let cargo = linked.join("cargo");
    fs::hard_link(env!("CARGO_BIN_EXE_qrust"), &cargo).unwrap();
    // A cache last counted long ago: the build starts a collection.
    sb.file("qrust-home/cache/usage.json", r#"{"format": 1, "scanned_ns": 0, "total_bytes": 0, "free_at_scan": 0}"#);
    ok(sb.with_shim(&cargo.display().to_string(), &p).arg("build"));
    // The collection counts the cache when it is done (a command only records that it never was).
    let usage = sb.home.join("cache/usage.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !fs::read(&usage).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).is_some_and(|u| u["counted"] == json!(true)) {
        assert!(std::time::Instant::now() < deadline, "no collection ran as QuickRust; its log: {:?}", fs::read_to_string(sb.home.join("cache/gc.log")));
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let commands: Vec<String> = sb.sessions().iter().map(|s| s["command"]["argv"].to_string()).collect();
    fs::remove_dir_all(&linked).unwrap();
    assert_eq!(commands.len(), 1, "only the build was a Cargo command: {:?}", commands);
}

#[test]
fn quickrusts_cache_removes_what_is_gone_then_the_least_recently_used_and_builds_come_back() {
    let sb = Sandbox::new("collect");
    let (a, b, c) = (sb.package("a", "a"), sb.package("b", "b"), sb.package("c", "c"));
    for p in [&a, &b, &c] {
        ok(sb.cargo(p).arg("build"));
    }
    let (ta, tb, tc) = (sb.target_dir(&a), sb.target_dir(&b), sb.target_dir(&c));
    // A collection first: it waits for one the builds started in the background, and counts the
    // cache, so that no other starts before the ones this test runs.
    ok(sb.qrust(&a).arg("gc"));
    // b was last used three hours ago, a two; c just now. The workspace of the third is deleted.
    let backdate = |t: &std::path::Path, hours: u64| {
        let meta = t.with_file_name(format!("{}.json", t.file_name().unwrap().to_string_lossy()));
        let mut v: Value = serde_json::from_slice(&fs::read(&meta).unwrap()).unwrap();
        let used = v["last_used_ns"].as_u64().unwrap() - hours * 3_600_000_000_000;
        v["last_used_ns"] = json!(used);
        fs::write(&meta, v.to_string()).unwrap();
    };
    backdate(&ta, 2);
    backdate(&tb, 3);
    let gone = sb.package("gone", "gone");
    ok(sb.cargo(&gone).arg("build"));
    let tg = sb.target_dir(&gone);
    backdate(&tg, 1);
    fs::remove_dir_all(&gone).unwrap();
    assert!(tg.exists());
    // Within bounds: only the build directory of the workspace that is gone goes.
    let plain = json_out(sb.qrust(&a).args(["gc", "--format", "json"]));
    assert!(!tg.exists() && ta.exists() && tb.exists() && tc.exists(), "{}", plain);
    assert!(plain["removed"].as_array().unwrap().iter().any(|r| r["why"].as_str().unwrap().contains("is gone")));
    // Down to almost nothing: the least recently used first; the one just used stays.
    let dry = json_out(sb.qrust(&a).args(["gc", "--dry-run", "--to", "1", "--format", "json"]));
    assert!(ta.exists() && tb.exists(), "a dry run removes nothing");
    let paths = |v: &Value| -> Vec<String> { v["removed"].as_array().unwrap().iter().filter(|r| r["kind"] == "build directories").map(|r| r["path"].as_str().unwrap().to_string()).collect() };
    assert_eq!(paths(&dry), [tb.display().to_string(), ta.display().to_string()]);
    let done = json_out(sb.qrust(&a).args(["gc", "--to", "1", "--format", "json"]));
    assert_eq!(paths(&done), paths(&dry));
    assert!(!ta.exists() && !tb.exists() && tc.exists());
    assert!(done["skipped_in_use"].as_u64().unwrap() >= 1);
    // A removed directory is simply built again.
    ok(sb.cargo(&a).arg("build"));
    assert_eq!(stdout(&ok(sb.cargo(&a).args(["run", "-q"]))), "3\n");
    let status = stdout(&ok(sb.qrust(&a).args(["cache", "status"])));
    assert!(status.contains("QuickRust's cache:") && status.contains("build directories"), "{}", status);
    // When, as people say it.
    assert!(status.lines().any(|l| l.trim_start().starts_with("collected") && (l.contains("just now,") || l.contains(" ago,"))), "{}", status);
}

#[test]
fn crates_download_into_quickrusts_cargo_home_and_cargo_collects_them_when_space_is_short() {
    let sb = Sandbox::new("registry-gc");
    let reg = common::registry::Registry::start();
    reg.publish(&common::registry::Crate::lib("greet", "1.0.0", "pub fn greet() -> &'static str {\n    \"hello from the registry\"\n}\n"));
    reg.serve_crates_io_for(&sb.home);
    let p = sb.package("p", "p");
    sb.file("p/Cargo.toml", "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ngreet = \"1.0\"\n");
    sb.file("p/src/main.rs", "fn main() {\n    println!(\"{}\", greet::greet());\n}\n");
    sb.file("p/src/lib.rs", "");
    assert_eq!(stdout(&ok(sb.cargo(&p).args(["run", "-q"]))), "hello from the registry\n");
    let downloads = || reg.requests().iter().filter(|r| r.starts_with("/dl/greet")).count();
    assert_eq!(downloads(), 1);
    let sources = |h: &std::path::Path| fs::read_dir(h.join("registry/src")).map(|d| d.flatten().flat_map(|i| fs::read_dir(i.path()).into_iter().flatten().flatten()).count()).unwrap_or(0);
    assert_eq!(sources(&sb.cargo_home()), 1, "QuickRust's Cargo home holds the source");
    // Within bounds, nothing of the downloads goes (they were just used).
    let plain = json_out(sb.qrust(&p).args(["gc", "--format", "json"]));
    assert!(plain["removed"].as_array().unwrap().iter().all(|r| r["kind"] != "crate and git downloads"), "{}", plain);
    assert_eq!(sources(&sb.cargo_home()), 1);
    // Down to nothing, what was just used still stays: downloads like the build directory.
    let tight = json_out(sb.qrust(&p).args(["gc", "--to", "1", "--format", "json"]));
    assert!(tight["removed"].as_array().unwrap().iter().all(|r| r["kind"] != "crate and git downloads"), "{}", tight);
    assert_eq!(sources(&sb.cargo_home()), 1);
    // An hour later by Cargo's clock (which Cargo's own tests move) and for the downloads' files
    // (whose times say what is old enough to look at), Cargo removes them; the build directory,
    // by QuickRust's clock, stays.
    let hour_later = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 3_600).to_string();
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3_600);
    for dir in ["registry/cache", "registry/src"] {
        for group in fs::read_dir(sb.cargo_home().join(dir)).unwrap().flatten() {
            for e in fs::read_dir(group.path()).unwrap().flatten() {
                fs::File::open(e.path()).unwrap().set_modified(hour_ago).unwrap();
            }
        }
    }
    let later = json_out(sb.qrust(&p).args(["gc", "--to", "1", "--format", "json"]).env("__CARGO_TEST_LAST_USE_NOW", &hour_later));
    let by_cargo = later["removed"].as_array().unwrap().iter().find(|r| r["kind"] == "crate and git downloads").unwrap_or_else(|| panic!("{}", later));
    assert!(by_cargo["bytes"].as_u64().unwrap() > 0 && by_cargo["why"].as_str().unwrap().starts_with("Cargo: Removed "), "{}", by_cargo);
    assert_eq!(sources(&sb.cargo_home()), 0);
    assert!(sb.target_dir(&p).join("debug/p").is_file());
    // What needs it again downloads it again.
    ok(sb.cargo(&p).arg("clean"));
    assert_eq!(stdout(&ok(sb.cargo(&p).args(["run", "-q"]))), "hello from the registry\n");
    assert_eq!(downloads(), 2);
}

#[test]
fn every_command_speaks_quickrusts_language_in_a_terminal() {
    use common::registry::{Crate, Registry};
    let sb = Sandbox::new("screens");
    let reg = Registry::start();
    reg.publish(&Crate::lib("greet", "1.0.0", "pub fn greet() -> &'static str {\n    \"hello\"\n}\n"));
    reg.publish(&Crate::lib("greet", "1.1.0", "pub fn greet() -> &'static str {\n    \"hello again\"\n}\n"));
    reg.serve_crates_io_for(&sb.home);
    let p = sb.package("p", "p");
    let Some(mut add) = sb.cargo_in_terminal(&p, "add greet@=1.0.0") else { return };
    let shown = screen(&run(&mut add).stdout);
    // The duration that closes a line, `[0.92s]`, as `[<time>]`.
    let durations = |s: &str| s.lines().map(|l| match l.rfind('[') { Some(i) if l.ends_with(']') && l[i + 1..].starts_with(|c: char| c.is_ascii_digit()) => format!("{}[<time>]", &l[..i]), _ => l.to_string() }).collect::<Vec<_>>().join("\n");
    assert_eq!(durations(&shown), "     added      greet                    =1.0.0 to dependencies\n     locked +greet 1.0.0\nOK   [<time>]", "{}", shown);
    // A build that downloads: one line for the downloads, the summary.
    sb.file("p/src/main.rs", "fn main() {\n    println!(\"{}\", greet::greet());\n}\n");
    let mut build = sb.cargo_in_terminal(&p, "build").unwrap();
    let shown = screen(&run(&mut build).stdout);
    let lines: Vec<&str> = shown.lines().collect();
    assert!(lines.iter().any(|l| l.starts_with("     Downloaded 1 crate")), "{}", shown);
    assert!(lines.last().is_some_and(|l| l.starts_with("OK   Built: ")), "{}", shown);
    assert!(!shown.contains("Downloading") && !shown.contains("Compiling") && !shown.contains("Updating crates.io index"), "nothing of Cargo's own progress stays on screen:\n{}", shown);
    // The lockfile moved on: listed, then summed up.
    sb.file("p/Cargo.toml", &fs::read_to_string(p.join("Cargo.toml")).unwrap().replace("\"=1.0.0\"", "\"1\""));
    let mut update = sb.cargo_in_terminal(&p, "update").unwrap();
    let shown = screen(&run(&mut update).stdout);
    assert_eq!(durations(&shown), "     updated    greet                    1.0.0 -> 1.1.0\nOK   Lockfile updated: 1 updated [<time>]", "{}", shown);
    let mut clean = sb.cargo_in_terminal(&p, "clean").unwrap();
    let shown = screen(&run(&mut clean).stdout);
    assert!(shown.starts_with("OK   Removed ") && shown.lines().count() == 1, "{}", shown);
}

#[test]
fn the_stale_plan_is_blocked_while_a_build_is_running() {
    // QA-028 / FR-110 acceptance: an active build owns its files.
    let sb = Sandbox::new("gcrace");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).arg("build"));
    let stale = sb.target_dir(&p).join("debug/deps/libgone-00000000.rlib");
    fs::write(&stale, vec![0u8; 100_000]).unwrap();
    fs::File::options().write(true).open(&stale).unwrap().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(90 * 86_400)).unwrap();
    let marker = sb.dir.join("started");
    sb.file("p/build.rs", &format!("fn main() {{\n    std::fs::write({:?}, \"x\").unwrap();\n    std::thread::sleep(std::time::Duration::from_secs(6));\n}}\n", marker));
    let mut build = sb.cargo(&p).arg("build").stderr(Stdio::null()).spawn().unwrap();
    while !marker.exists() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let during = json_out(sb.qrust(&p).args(["inventory", "--format", "json"]));
    assert!(build.wait().unwrap().success());
    let plan = &during["build_directories"][0];
    assert!(plan["gc_plan"]["blocked_by"].is_string(), "{}", plan["gc_plan"]);
    assert_eq!(plan["gc_plan"]["reclaimable_now_bytes"], 0);
    assert!(plan["active"]["cargo_lock_held"] == true || !plan["active"]["qrust_sessions"].as_array().unwrap().is_empty());
    let after = json_out(sb.qrust(&p).args(["inventory", "--format", "json"]));
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
    let user_target = tree(&sb.target_dir(&ws));

    let b = json_out(sb.qrust(&ws).args(["benchmark", "--scenario", "no-change", "--reps", "2", "--warmups", "0", "--format", "json"]));
    assert_eq!(b["state"], "complete", "{}", b);
    assert_eq!((b["samples_valid"].as_u64(), b["samples_discarded"].as_u64()), (Some(2), Some(0)));
    assert_eq!(b["summary"]["samples"]["wall_time_ns"].as_array().unwrap().len(), 2, "raw samples are retained");
    // Each measured run shows the wall time the summary was made of, not another measure.
    let measured: Vec<&Value> = b["runs"].as_array().unwrap().iter().filter(|r| r["phase"] == "measured").map(|r| &r["wall_time_ns"]).collect();
    assert_eq!(measured, b["summary"]["samples"]["wall_time_ns"].as_array().unwrap().iter().collect::<Vec<_>>(), "{}", b["runs"]);
    assert_eq!(b["summary"]["metrics"]["units_compiled"], 0, "nothing compiles in a no-change run");
    assert_eq!(b["summary"]["scenario"], json!({ "name": "no-change", "cache_state": "warm" }));
    let phases: Vec<&str> = b["runs"].as_array().unwrap().iter().map(|r| r["phase"].as_str().unwrap()).collect();
    assert_eq!(phases, ["prime", "measured", "measured"]);
    let now = tree(&sb.target_dir(&ws));
    assert_eq!(now.iter().filter(|f| !f.starts_with("qrust-bench/")).collect::<Vec<_>>(), user_target.iter().collect::<Vec<_>>(), "the user's own build output is untouched");
    assert!(now.iter().any(|f| f.starts_with("qrust-bench/no-change-default/")));

    // Without permission and without a terminal, the edit scenario refuses and edits nothing.
    let refused = run(sb.qrust(&ws).args(["benchmark", "--scenario", "small-edit", "--edit", "util/src/lib.rs", "--reps", "1", "--warmups", "0"]));
    assert_eq!(refused.status.code(), Some(2));
    assert!(stderr(&refused).contains("--allow-mutation"));
    assert_eq!(fs::read(&lib).unwrap(), original);

    let e = json_out(sb.qrust(&ws).args(["benchmark", "--scenario", "small-edit", "--edit", "util/src/lib.rs", "--allow-mutation", "--reps", "2", "--warmups", "0", "--format", "json"]));
    assert_eq!(e["state"], "complete", "{}", e);
    assert_eq!(e["mutation"]["restored"], true);
    assert_eq!(fs::read(&lib).unwrap(), original, "the edited file is restored byte for byte, uncommitted change included");
    assert_eq!(e["summary"]["metrics"]["units_compiled"], 2, "util and its dependent app rebuild after each edit");
    assert!(tree(&sb.home).iter().all(|f| !f.contains("/restore/")), "the restore journal is gone after a clean finish");
    // Files outside the workspace are never edited.
    let outside = sb.file("outside.rs", "fn x() {}\n");
    let out = run(sb.qrust(&ws).args(["benchmark", "--scenario", "small-edit", "--allow-mutation", "--edit"]).arg(&outside));
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("outside the workspace"));

    // Baselines can be promoted from a benchmark and compared against.
    ok(sb.qrust(&ws).args(["baseline", "set", "edit-loop", "--benchmark", e["id"].as_str().unwrap()]));
    let cmp = json_out(sb.qrust(&ws).args(["compare", "baseline:edit-loop", &format!("benchmark:{}", e["id"].as_str().unwrap()), "--format", "json"]));
    assert_eq!(cmp["comparability"]["state"], "comparable");
    let wall = cmp["metrics"].as_array().unwrap().iter().find(|m| m["metric"] == "wall_time_ns").unwrap();
    assert_eq!(wall["delta"], 0);
}

#[test]
fn clean_scenario_and_failed_builds() {
    let sb = Sandbox::new("benchclean");
    let p = sb.package("p", "p");
    let b = json_out(sb.qrust(&p).args(["benchmark", "--scenario", "clean", "--reps", "2", "--warmups", "0", "--format", "json"]));
    assert_eq!(b["state"], "complete");
    assert_eq!(b["summary"]["metrics"]["units_compiled"], 2, "every repetition of a clean build compiles everything");
    assert_eq!(b["summary"]["scenario"]["cache_state"], "cold");
    assert!(!sb.target_dir(&p).join("debug").exists(), "a benchmark does not build into the user's own profile directory");
    // A build failure makes the benchmark invalid, never a fast sample.
    fs::write(p.join("src/lib.rs"), "does not compile\n").unwrap();
    let out = run(sb.qrust(&p).args(["benchmark", "--scenario", "clean", "--reps", "1", "--warmups", "0", "--format", "json"]));
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
    let bad = run(sb.qrust(&p).args(["experiment", "--name", "no-asserts", "--set", "profile.dev.debug-assertions=false"]).args(common).args(["--", "build"]));
    let r: Value = serde_json::from_slice(&bad.stdout).unwrap_or_else(|_| panic!("{}", stderr(&bad)));
    assert_eq!(r["status"], "invalid", "{}", r["reason"]);
    assert_eq!((r["verification"]["performed"].as_bool(), r["verification"]["passed"].as_bool()), (Some(true), Some(false)));
    assert!(r["proposed_config"].is_null(), "an invalid candidate proposes nothing");
    assert_eq!(bad.status.code(), Some(1));

    // A candidate that passes verification reports build and runtime dimensions separately.
    let good = json_out(sb.qrust(&p).args(["experiment", "codegen", "--codegen-units", "4"]).args(common).args(["--", "build"]));
    assert_eq!(good["verification"]["passed"], true);
    assert!(matches!(good["status"].as_str(), Some("verified" | "inconclusive")), "{}", good["status"]);
    assert_eq!(good["candidate_settings"]["cargo_config"], json!(["profile.dev.codegen-units=4"]));
    assert!(good["dimensions"]["build"].is_object());
    assert!(good["dimensions"]["runtime_and_debugging"][0]["risk"].as_str().unwrap().contains("generated-code performance"));
    // The overlay was ephemeral: no file in the repository changed.
    assert_eq!(tree(&p).into_iter().filter(|f| !f.starts_with("target/")).collect::<Vec<_>>(), before);
    // An experiment without a candidate setting is an error, not a self-comparison.
    assert_eq!(run(sb.qrust(&p).args(["experiment", "--", "build"])).status.code(), Some(2));
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
    let w = json_out(sb.qrust(&repo).args(["analyze", "worktrees", "--format", "json"]));
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
    let w = json_out(sb.qrust(&repo).args(["analyze", "worktrees", "--format", "json"]));
    let other = w["others"].as_array().unwrap().iter().find(|o| o["root"].as_str().unwrap().ends_with("repo-feature")).unwrap();
    assert_eq!(other["sample"], json!(["shared@0.1.0"]), "a unit with different source content is not a duplicate");
    assert_eq!(w["summary"]["duplicate_units"], 1);
    // FR-120.3: the advice never proposes one shared writable target directory.
    let advice = w["advice"].to_string();
    assert!(advice.contains("sccache") && advice.contains("does not point several worktrees at one writable target directory"), "{}", advice);
    let text = stdout(&ok(sb.qrust(&repo).args(["analyze", "worktrees"])));
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

    sb.file("ws/.qrust/config.toml", "[cache]\nprovider = \"sccache\"\n");
    let mut build = sb.cargo(&ws);
    isolate(build.arg("build"));
    ok(&mut build);
    let mut rep = sb.qrust(&ws);
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

    // Preview the other alternative, apply it (a change to QuickRust's own config is `safe`), verify, roll back.
    let mut fix = sb.qrust(&ws);
    isolate(fix.args(["fix", "--dry-run", "rec:cache-strategy:incremental-vs-sccache", "--option", "disable-incremental", "--format", "json"]));
    let preview = json_out(&mut fix);
    assert_eq!((preview[0]["class"].as_str(), preview[0]["file"].as_str()), (Some("safe"), Some(".qrust/config.toml")));
    assert!(preview[0]["diff"].as_str().unwrap().contains("+local-dev = \"off\""));
    assert_eq!(fs::read_to_string(ws.join(".qrust/config.toml")).unwrap(), "[cache]\nprovider = \"sccache\"\n", "the preview applied nothing");
    let id = preview[0]["id"].as_str().unwrap();
    ok(sb.qrust(&ws).args(["fix", "--apply", id]));
    assert_eq!(fs::read_to_string(ws.join(".qrust/config.toml")).unwrap(), "[cache]\nprovider = \"sccache\"\n\n[incremental]\nlocal-dev = \"off\"\n");

    // With the policy in place the change is recorded, units are no longer incremental, and a clean rebuild hits the cache.
    for _ in 0..2 {
        fs::remove_dir_all(sb.target_dir(&ws)).unwrap();
        let mut b = sb.cargo(&ws);
        isolate(b.arg("build"));
        ok(&mut b);
    }
    let mut rep = sb.qrust(&ws);
    isolate(rep.args(["report", "--format", "json"]));
    let r2 = json_out(&mut rep);
    let inc2 = &r2["policy"]["incremental"];
    assert_eq!((inc2["decision"].as_str(), inc2["applied_env"].as_str()), (Some("force-off"), Some("CARGO_INCREMENTAL=0")));
    assert!(inc2["rationale"].as_str().unwrap().contains(".qrust/config.toml"), "the decision names its origin: {}", inc2["rationale"]);
    assert_eq!(inc2["sccache_conflict"], false);
    assert_eq!(inc2["observed"]["units_incremental"], 0);
    assert!(r2["cache"]["hits"].as_u64().unwrap() >= 2, "library units are served from the cache on the second clean build: {}", r2["cache"]);
    let text = {
        let mut c = sb.qrust(&ws);
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

    ok(sb.qrust(&ws).args(["fix", "--rollback", id]));
    assert_eq!(fs::read_to_string(ws.join(".qrust/config.toml")).unwrap(), "[cache]\nprovider = \"sccache\"\n");
}

#[test]
fn text_and_html_reports_render_every_section() {
    let sb = Sandbox::new("render");
    let ws = sb.workspace("ws");
    ok(sb.cargo(&ws).arg("build"));
    let text = stdout(&ok(sb.qrust(&ws).arg("analyze")));
    for heading in ["Timing", "Rebuild reasons", "Duplication", "Build scripts", "Procedural macros", "Codegen and linking", "Compiler cache", "Storage", "Recommendations"] {
        assert!(text.contains(heading), "section {} missing", heading);
    }
    assert!(text.contains("Proc-macro execution timing\n    Unavailable:"), "unavailable data is labelled as such");
    assert!(text.contains("Link phase timing\n    Unavailable:"));
    ok(sb.qrust(&ws).args(["report", "--format", "html", "-o", "report.html"]));
    let html = fs::read_to_string(ws.join("report.html")).unwrap();
    assert!(html.starts_with("<!doctype html>") && html.contains("<html lang=\"en\">"));
    assert!(html.contains("<caption>") && html.contains("<th scope=\"col\">") && !html.contains("<script"), "tables with headers, no scripts");
    for section in ["analyze timing", "analyze rebuilds", "analyze scripts", "analyze proc-macros", "analyze codegen", "analyze storage", "analyze recommendations", "runs", "inspect", "policy explain", "cache status"] {
        ok(sb.qrust(&ws).args(section.split(' ')));
    }
    assert_eq!(run(sb.qrust(&ws).args(["analyze", "nonsense"])).status.code(), Some(2));
    assert_eq!(run(sb.qrust(&ws).args(["analyze", "--bogus-flag"])).status.code(), Some(2));
}

#[test]
fn analysis_without_any_run_explains_what_to_do() {
    let sb = Sandbox::new("norun");
    let p = sb.package("p", "p");
    let out = run(sb.qrust(&p).arg("analyze"));
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("no recorded build runs"), "{}", stderr(&out));
    assert!(stdout(&ok(sb.qrust(&p).arg("runs"))).contains("No recorded runs"));
}

#[test]
fn policy_explain_shows_each_setting_with_its_origin() {
    // CFG-001, CFG-002.
    let sb = Sandbox::new("policy");
    let p = sb.package("p", "p");
    sb.file("p/.qrust/config.toml", "[cache]\nfail = \"closed\"\n");
    sb.file("qrust-home/config.toml", "[artifacts]\nstale-days = 7\n");
    let v = json_out(sb.qrust(&p).args(["policy", "explain", "--format", "json"]).env("QRUST_INCREMENTAL_CI", "off").env("QRUST_REPORT", "r.json"));
    let get = |key: &str| v["settings"].as_array().unwrap().iter().find(|s| s["key"] == key).unwrap().clone();
    assert_eq!((get("cache.fail")["value"].as_str(), get("cache.fail")["origin"].as_str()), (Some("\"closed\""), Some("$WORKSPACE/.qrust/config.toml")));
    assert_eq!(get("artifacts.stale-days")["value"], "7");
    assert!(get("artifacts.stale-days")["origin"].as_str().unwrap().ends_with("config.toml"));
    assert_eq!(get("incremental.ci")["origin"], "env QRUST_INCREMENTAL_CI");
    assert_eq!(get("measurement.report")["origin"], "env QRUST_REPORT");
    assert_eq!(get("cache.provider")["origin"], "default");
    // Repository settings override user settings; the environment overrides both.
    sb.file("p/.qrust/config.toml", "[artifacts]\nstale-days = 3\n");
    let v2 = json_out(sb.qrust(&p).args(["policy", "explain", "--format", "json"]));
    assert_eq!(v2["settings"].as_array().unwrap().iter().find(|s| s["key"] == "artifacts.stale-days").unwrap()["value"], "3");
    let v3 = json_out(sb.qrust(&p).args(["policy", "explain", "--format", "json"]).env("QRUST_ARTIFACTS_STALE_DAYS", "1"));
    assert_eq!(v3["settings"].as_array().unwrap().iter().find(|s| s["key"] == "artifacts.stale-days").unwrap()["value"], "1");
    // A misspelt variable in QuickRust's namespace is an error rather than a silently ignored setting.
    let bad = run(sb.qrust(&p).args(["policy", "explain"]).env("QRUST_CACHE_PROVIDR", "sccache"));
    assert_eq!(bad.status.code(), Some(2));
    assert!(stderr(&bad).contains("QRUST_CACHE_PROVIDR"));
}

#[test]
fn doctor_setup_and_uninstall() {
    // MIG-001, MIG-002.
    let sb = Sandbox::new("install");
    let p = sb.package("p", "p");
    let doctor = stdout(&run(sb.qrust(&p).arg("doctor")));
    for item in ["set up             no", "active toolchain", "selected by: default toolchain", "  Cargo  ", "  rustc  ", "cargo home", "CARGO ", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "target directory", "build directory", "cache policy", "capabilities", "\nOK\n"] {
        assert!(doctor.contains(item), "doctor output lacks `{}`:\n{}", item, doctor);
    }
    // A user with rustup's ~/.cargo: credentials, a registry, and settings of other kinds.
    let home = sb.dir.join("user");
    sb.file("user/.cargo/credentials.toml", "[registries.corp]\ntoken = \"s3cret\"\n");
    sb.file("user/.cargo/config.toml", "[registries.corp]\nindex = \"sparse+https://corp.example/index/\"\n\n[build]\ntarget-dir = \"/elsewhere\"\n");
    sb.file("user/.bashrc", "export EDITOR=vi\n");
    let setup = |extra: &[&str]| run(sb.qrust(&p).arg("setup").args(["--toolchain", "none"]).args(extra).env("HOME", &home));
    let out = setup(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let said = format!("{}{}", stdout(&out), stderr(&out));
    assert!(said.contains("installed in") && said.contains("imported from") && said.contains("credentials, [registries]") && said.contains("not imported: [build]"), "{}", said);
    let bin = sb.home.join("bin");
    assert_ne!(fs::canonicalize(bin.join("qrust")).unwrap(), fs::canonicalize(env!("CARGO_BIN_EXE_qrust")).unwrap(), "a copy of its own, not the binary setup ran from");
    assert!(bin.join("qrust-rustc-wrapper").is_file());
    for proxy in ["cargo", "rustc", "rustfmt", "cargo-clippy", "rust-analyzer"] {
        assert_eq!(fs::read_link(bin.join(proxy)).unwrap(), std::path::Path::new("qrust"), "{}", proxy);
    }
    let cargo_home = sb.cargo_home();
    assert_eq!(fs::read_to_string(cargo_home.join("credentials.toml")).unwrap(), "[registries.corp]\ntoken = \"s3cret\"\n");
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(cargo_home.join("credentials.toml")).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let config = fs::read_to_string(cargo_home.join("config.toml")).unwrap();
    assert!(config.contains("[registries.corp]") && config.contains("sparse+https://corp.example/index/") && !config.contains("target-dir"), "{}", config);
    // The shell startup files source QuickRust's environment, which puts its bin first.
    for f in [".profile", ".bashrc"] {
        assert_eq!(fs::read_to_string(home.join(f)).unwrap().matches("qrust-home/env\"").count(), 1, "{}", f);
    }
    assert!(fs::read_to_string(home.join(".bashrc")).unwrap().starts_with("export EDITOR=vi\n"));
    let path = stdout(&ok(std::process::Command::new("sh").arg("-c").arg(". \"$1\"; printf %s \"$PATH\"").arg("sh").arg(sb.home.join("env")).env("HOME", &home).env("PATH", "/usr/bin:/bin")));
    assert!(path.starts_with(&format!("{}:", bin.display())), "{}", path);
    // QuickRust as cargo, through the installed proxy.
    let via = run(std::process::Command::new(bin.join("cargo")).current_dir(&p).arg("--version").env("QRUST_HOME", &sb.home).env("PATH", &path));
    assert!(stdout(&via).starts_with("cargo 1."), "{}", stderr(&via));
    // Doctor sees the setup, and that its bin is not first on this test's PATH.
    let doctor = run(sb.qrust(&p).arg("doctor"));
    assert_eq!(doctor.status.code(), Some(1));
    assert!(stdout(&doctor).contains("its bin is NOT first on PATH"), "{}", stdout(&doctor));
    // Again: repairs, never imports twice or edits a file twice.
    fs::write(home.join(".cargo/credentials.toml"), "changed").unwrap();
    fs::remove_file(bin.join("rustc")).unwrap();
    let again = setup(&[]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert!(!stderr(&again).contains("imported"), "{}", stderr(&again));
    assert!(stderr(&again).contains("     PATH: already set up"), "{}", stderr(&again));
    assert!(bin.join("rustc").exists());
    assert_eq!(fs::read_to_string(cargo_home.join("credentials.toml")).unwrap(), "[registries.corp]\ntoken = \"s3cret\"\n");
    assert_eq!(fs::read_to_string(home.join(".profile")).unwrap().matches("qrust-home/env\"").count(), 1);
    // Something of someone else's where a proxy goes is never replaced.
    fs::remove_file(bin.join("rustfmt")).unwrap();
    fs::write(bin.join("rustfmt"), "#!/bin/sh\necho theirs\n").unwrap();
    let refused = setup(&[]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(stderr(&refused).contains("is not QuickRust"), "{}", stderr(&refused));
    assert_eq!(fs::read_to_string(bin.join("rustfmt")).unwrap(), "#!/bin/sh\necho theirs\n");
    // Uninstalling needs --yes, then removes QuickRust's home (not what its links point to) and the PATH lines.
    assert_eq!(run(sb.qrust(&p).args(["self", "uninstall"]).env("HOME", &home)).status.code(), Some(2));
    ok(sb.qrust(&p).args(["self", "uninstall", "--yes"]).env("HOME", &home));
    assert!(!sb.home.exists());
    assert!(toolchain_store().join("channels.json").exists(), "the linked toolchain store is untouched");
    assert_eq!(fs::read_to_string(home.join(".bashrc")).unwrap(), "export EDITOR=vi\n");
    assert!(!fs::read_to_string(home.join(".profile")).unwrap().contains("qrust"));
    // What setup says to run, pasted into a shell, puts QuickRust's bin first (`~/.qrust` as usual).
    let qrust_home = home.join(".qrust");
    let said = stderr(&ok(sb.qrust(&p).args(["setup", "--toolchain", "none"]).env("HOME", &home).env("QRUST_HOME", &qrust_home)));
    let pasted = said.lines().find_map(|l| l.strip_prefix("NOTE open a new shell, or run:  ")).and_then(|l| l.split("  (fish").next()).unwrap_or_else(|| panic!("{}", said));
    let path = stdout(&ok(std::process::Command::new("sh").arg("-c").arg(format!("{}\nprintf %s \"$PATH\"", pasted)).env("HOME", &home).env("PATH", "/usr/bin:/bin")));
    assert_eq!(path, format!("{}:/usr/bin:/bin", qrust_home.join("bin").display()), "{}", pasted);
}

#[test]
#[cfg(target_os = "linux")]
fn qrust_updates_itself_from_its_releases_and_only_to_what_they_say() {
    use std::io::Write as _;
    let sb = Sandbox::new("selfupdate");
    let home = sb.dir.join("user");
    fs::create_dir_all(&home).unwrap();
    ok(sb.qrust(&sb.dir).args(["setup", "--toolchain", "none", "--no-modify-path"]).env("HOME", &home));
    let qrust = sb.home.join("bin/qrust");
    let target = if cfg!(target_arch = "aarch64") { "aarch64-unknown-linux-musl" } else { "x86_64-unknown-linux-musl" };
    // A release: QuickRust's archive (here, scripts that say what they are) and its checksum.
    let release = |says: &str| -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, body) in [("qrust", format!("#!/bin/sh\necho '{}'\n", says)), ("qrust-rustc-wrapper", "#!/bin/sh\n".to_string())] {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o755);
            b.append_data(&mut h, format!("qrust-{}/{}", target, name), body.as_bytes()).unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&b.into_inner().unwrap()).unwrap();
        gz.finish().unwrap()
    };
    let server = common::registry::Registry::start();
    let publish = |version: &str, says: &str, sum_of: Option<&[u8]>| {
        let archive = release(says);
        let sum = sha256_hex(sum_of.unwrap_or(&archive));
        server.put("/version.txt", format!("{}\n", version).into_bytes());
        server.put(&format!("/qrust-{}.tar.gz", target), archive);
        server.put(&format!("/qrust-{}.tar.gz.sha256", target), format!("{}  qrust-{}.tar.gz\n", sum, target).into_bytes());
    };
    let update = || run(sb.qrust(&sb.dir).args(["self", "update"]).env("QRUST_UPDATE_URL", &server.url));
    let current = env!("CARGO_PKG_VERSION");
    // Nothing published yet.
    let none = update();
    assert_eq!(none.status.code(), Some(2));
    assert!(stderr(&none).contains(&format!("no QuickRust release is published at {} yet", server.url)), "{}", stderr(&none));
    let before = fs::read(&qrust).unwrap();
    // Not newer: nothing to do.
    publish(current, "qrust old", None);
    let out = update();
    assert!(out.status.success() && stderr(&out).contains("is the latest"), "{}", stderr(&out));
    // Tampered with: refused, nothing replaced.
    publish("999.0.0", "qrust 999.0.0", Some(b"something else"));
    let out = update();
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("is not the published file"), "{}", stderr(&out));
    // Not the version it was published as: refused.
    publish("999.0.0", "qrust 998.0.0", None);
    let out = update();
    assert!(stderr(&out).contains("says \"qrust 998.0.0\", not qrust 999.0.0"), "{}", stderr(&out));
    assert_eq!(fs::read(&qrust).unwrap(), before);
    // A newer release: installed, and the proxies are it.
    publish("999.0.0", "qrust 999.0.0", None);
    let out = update();
    assert!(out.status.success() && stderr(&out).contains("QuickRust 999.0.0 installed"), "{}", stderr(&out));
    assert_eq!(stdout(&ok(std::process::Command::new(&qrust).arg("--version"))), "qrust 999.0.0\n");
    assert_eq!(stdout(&ok(std::process::Command::new(sb.home.join("bin/cargo")).arg("--version"))), "qrust 999.0.0\n");
}

#[test]
#[cfg(target_os = "linux")]
fn the_install_script_checks_the_release_and_sets_qrust_up() {
    use std::io::Write as _;
    let sb = Sandbox::new("installsh");
    let home = sb.dir.join("user");
    fs::create_dir_all(&home).unwrap();
    let target = if cfg!(target_arch = "aarch64") { "aarch64-unknown-linux-musl" } else { "x86_64-unknown-linux-musl" };
    // The release archive, of the QuickRust under test.
    let mut b = tar::Builder::new(Vec::new());
    let qrust = std::path::Path::new(env!("CARGO_BIN_EXE_qrust"));
    for name in ["qrust", "qrust-rustc-wrapper"] {
        b.append_path_with_name(qrust.with_file_name(name), format!("qrust-{}/{}", target, name)).unwrap();
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&b.into_inner().unwrap()).unwrap();
    let archive = gz.finish().unwrap();
    let server = common::registry::Registry::start();
    server.put(&format!("/qrust-{}.tar.gz", target), archive.clone());
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh");
    let install = || {
        let mut c = sb.with_shim("sh", &sb.dir);
        c.arg(&script).args(["--toolchain", "none", "--no-modify-path"]).env("HOME", &home).env("QRUST_UPDATE_URL", &server.url);
        run(&mut c)
    };
    server.put(&format!("/qrust-{}.tar.gz.sha256", target), format!("{}  x\n", "0".repeat(64)).into_bytes());
    let refused = install();
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr(&refused).contains("is not the published file"), "{}", stderr(&refused));
    assert!(!sb.home.join("bin/qrust").exists());
    server.put(&format!("/qrust-{}.tar.gz.sha256", target), format!("{}  qrust.tar.gz\n", sha256_hex(&archive)).into_bytes());
    let out = install();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("installed in"), "{}", stderr(&out));
    let cargo = run(std::process::Command::new(sb.home.join("bin/cargo")).arg("--version").env("QRUST_HOME", &sb.home).env("HOME", &home).env_remove("CARGO_HOME"));
    assert!(stdout(&cargo).starts_with("cargo 1."), "{}", stderr(&cargo));
}

#[cfg(target_os = "linux")]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{:02x}", b)).collect()
}

#[test]
fn retention_bounds_recorded_state() {
    // NFR-SPACE-001 / R-014.
    let sb = Sandbox::new("retention");
    let p = sb.package("p", "p");
    sb.file("qrust-home/config.toml", "[measurement]\nkeep-runs = 3\nkeep-raw-events = 1\nfinalize = \"sync\"\n");
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
fn fast_mode_keeps_stable_rejecting_unstable_features() {
    // Fast mode unlocks `-Z` flags on a stable compiler with RUSTC_BOOTSTRAP, which on its own
    // would also accept `#![feature]`: code that upstream Cargo rejects must stay rejected.
    let sb = Sandbox::new("features");
    let p = sb.package("p", "p");
    let gated = "#![feature(never_type)]\npub fn add(x: u32, y: u32) -> u32 { x + y }\npub fn never() -> Option<!> { None }\n";
    fs::write(p.join("src/main.rs"), "fn main() {}\n").unwrap();
    // An incremental rebuild (compiled here before) and a first compilation (after a clean).
    ok(sb.cargo(&p).arg("build"));
    fs::write(p.join("src/lib.rs"), gated).unwrap();
    let rebuild = sb.cargo(&p).arg("build").output().unwrap();
    ok(sb.cargo(&p).arg("clean"));
    let first = sb.cargo(&p).arg("build").output().unwrap();
    let upstream = sb.upstream(&p).arg("build").output().unwrap();
    assert!(!upstream.status.success(), "the stable toolchain under test accepted #![feature]: {}", stderr(&upstream));
    for (what, qrust) in [("an incremental rebuild", rebuild), ("a first compilation", first)] {
        assert!(!qrust.status.success(), "fast mode accepted #![feature] on a stable compiler in {}: {}", what, stderr(&qrust));
        assert!(stderr(&qrust).contains("feature"), "{}", stderr(&qrust));
    }
}

#[test]
fn fast_mode_applies_its_settings_unless_the_user_made_them() {
    let sb = Sandbox::new("fast");
    let p = sb.package("p", "p");
    let debuginfo = |r: &Value| -> Vec<String> { r["units"].as_array().unwrap().iter().filter(|u| u["freshness"] == "compiled").map(|u| u["codegen"]["debuginfo"].as_str().unwrap_or("0").to_string()).collect() };
    // The binary counts as quick to link however busy the machine is (from its first
    // compilation on: that is the one that shows it small).
    let quick = "600000";

    ok(sb.cargo(&p).arg("build").env("QRUST_CACHE_ARTIFACTS_MIN_LINK_MS", quick));
    let r = sb.report(&p, "latest");
    assert!(!debuginfo(&r).is_empty() && debuginfo(&r).iter().all(|d| d == "line-tables-only"), "{:?}", debuginfo(&r));
    let applied: Vec<&str> = r["policy"]["fast"]["applied"].as_array().unwrap().iter().map(|a| a["key"].as_str().unwrap()).collect();
    assert!(applied.contains(&"profile.dev.debug") && applied.contains(&"profile.dev.package.\"*\".debug"), "{:?}", applied);
    // The parallel frontend (on machines with at least four cores) reaches units compiled from
    // scratch, including a workspace unit's first compilation in a target directory, never an
    // incremental rebuild: it slows those down on small crates.
    if std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) >= 4 {
        assert!(applied.contains(&"rustc -Zthreads"), "{:?}", applied);
        let threads_by_unit = |sb: &Sandbox| -> Vec<(bool, bool)> {
            let s = sb.sessions().into_iter().rev().find(|s| s["instrumented"] == json!(true)).unwrap();
            fs::read_dir(s["events_dir"].as_str().unwrap())
                .unwrap()
                .filter_map(|e| serde_json::from_slice::<Value>(&fs::read(e.unwrap().path()).ok()?).ok())
                .filter(|e| e["kind"] == "compile")
                .map(|e| {
                    let args: Vec<&str> = e["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
                    (args.iter().any(|a| a.starts_with("incremental=")), args.iter().any(|a| a.starts_with("-Zthreads=")))
                })
                .collect()
        };
        let first = threads_by_unit(&sb);
        assert!(!first.is_empty() && first.iter().all(|(incremental, threads)| *incremental && *threads), "{:?}", first);
        fs::write(p.join("src/lib.rs"), "pub fn add(x: u32, y: u32) -> u32 { y + x }\n").unwrap();
        ok(sb.cargo(&p).arg("build").env("QRUST_CACHE_ARTIFACTS_MIN_LINK_MS", quick));
        let rebuild = threads_by_unit(&sb);
        assert!(!rebuild.is_empty() && rebuild.iter().all(|(incremental, threads)| *incremental && !*threads), "{:?}", rebuild);
        // After a clean the library comes from the cache; the binary, which links in a moment and
        // so is not stored, is compiled again from scratch. Its first compilation, from scratch,
        // showed it small, and edits since have not changed that: no parallel frontend.
        ok(sb.cargo(&p).arg("clean"));
        ok(sb.cargo(&p).arg("build").env("QRUST_CACHE_ARTIFACTS_MIN_LINK_MS", quick));
        assert_eq!(cache_results(&sb), vec![("p".to_string(), "hit".to_string()), ("p".to_string(), "miss".to_string())], "{:#?}", cache_details(&sb));
        assert_eq!(threads_by_unit(&sb), vec![(true, false), (true, false)], "{:#?}", cache_details(&sb));
        ok(sb.cargo(&p).args(["build", "--release"]));
        let release = threads_by_unit(&sb);
        assert!(!release.is_empty() && release.iter().all(|(incremental, threads)| !*incremental && *threads), "{:?}", release);
    }
    assert_eq!(r["run"]["command"]["argv_redacted"], json!(["cargo", "build"]), "the record shows the user's command, not QuickRust's overlays");

    // A `--config` of the user's after the subcommand makes Cargo ignore every `--config` before
    // it: QuickRust's settings go with it, so the build is the same one, fresh.
    ok(sb.cargo(&p).args(["build", "--config", "build.jobs=2"]));
    assert!(debuginfo(&sb.report(&p, "latest")).is_empty(), "rebuilt without fast mode's settings: {:?}", debuginfo(&sb.report(&p, "latest")));

    // compat: exactly what upstream Cargo builds.
    ok(sb.cargo(&p).arg("build").env("QRUST_MODE", "compat"));
    let r = sb.report(&p, "latest");
    assert!(debuginfo(&r).iter().all(|d| d == "2"), "{:?}", debuginfo(&r));
    assert!(r["policy"]["fast"]["applied"].as_array().unwrap().is_empty());

    // A setting the user made wins, and the report says why QuickRust stood back.
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

/// Every compile of the latest instrumented run with its cache result and the reason recorded
/// for it, for assertion messages.
fn cache_details(sb: &Sandbox) -> Vec<String> {
    let s = sb.sessions().into_iter().rev().find(|s| s["instrumented"] == json!(true)).unwrap();
    fs::read_dir(s["events_dir"].as_str().unwrap())
        .unwrap()
        .filter_map(|e| serde_json::from_slice::<Value>(&fs::read(e.unwrap().path()).ok()?).ok())
        .filter(|ev| ev["kind"] == "compile")
        .map(|ev| format!("{}: {}", ev["args"].as_array().unwrap().iter().skip_while(|a| *a != "--crate-name").nth(1).and_then(Value::as_str).unwrap_or("?"), ev["cache"]))
        .collect()
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
fn an_output_rewritten_in_place_after_it_was_stored_is_never_restored() {
    let sb = Sandbox::new("rewritten");
    let dep = sb.dir.join("qrust-home/cargo/registry/src/fake-0000/greet-1.0.0");
    sb.file("qrust-home/cargo/registry/src/fake-0000/greet-1.0.0/Cargo.toml", "[package]\nname = \"greet\"\nversion = \"1.0.0\"\nedition = \"2021\"\n");
    sb.file("qrust-home/cargo/registry/src/fake-0000/greet-1.0.0/src/lib.rs", "pub fn greet() -> &'static str {\n    \"hello\"\n}\n");
    sb.file("app/Cargo.toml", &format!("[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ngreet = {{ path = {:?} }}\n\n[profile.dev.package.greet]\nincremental = false\n", dep.display().to_string()));
    sb.file("app/src/main.rs", "fn main() {\n    println!(\"{}\", greet::greet());\n}\n");
    let app = sb.dir.join("app");

    ok(sb.cargo(&app).arg("build"));
    assert!(cache_results(&sb).contains(&("greet".to_string(), "stored".to_string())), "{:#?}", cache_details(&sb));
    // A tool that rewrites an output in place (rather than replacing the file, as rustc and
    // the linkers do) also rewrites whatever shares that file.
    let rlib = fs::read_dir(sb.target_dir(&app).join("debug/deps")).unwrap().map(|e| e.unwrap().path()).find(|p| p.file_name().unwrap().to_string_lossy().starts_with("libgreet-") && p.extension().is_some_and(|x| x == "rlib")).unwrap();
    let mut bytes = fs::read(&rlib).unwrap();
    bytes.truncate(bytes.len() / 2);
    fs::OpenOptions::new().write(true).truncate(true).open(&rlib).unwrap().write_all(&bytes).unwrap();

    ok(sb.cargo(&app).arg("clean"));
    let run = ok(sb.cargo(&app).args(["run", "-q"]));
    assert_eq!(stdout(&run), "hello\n");
    assert!(cache_results(&sb).contains(&("greet".to_string(), "stored".to_string())), "the damaged entry was discarded, compiled and stored again: {:#?}", cache_details(&sb));

    ok(sb.cargo(&app).arg("clean"));
    ok(sb.cargo(&app).arg("build"));
    assert!(cache_results(&sb).contains(&("greet".to_string(), "hit".to_string())), "{:#?}", cache_details(&sb));
}

#[test]
fn compiler_probes_run_by_build_scripts_are_never_cached() {
    let sb = Sandbox::new("probes");
    // Like autocfg: a build script compiles small programs with the same rustc (through the
    // wrapper Cargo hands it) to learn what the compiler supports. Probes must run every time and
    // may fail by design; neither outcome is an artifact to keep.
    sb.file("qrust-home/cargo/registry/src/fake-0000/probed-1.0.0/Cargo.toml", "[package]\nname = \"probed\"\nversion = \"1.0.0\"\nedition = \"2021\"\n");
    sb.file(
        "qrust-home/cargo/registry/src/fake-0000/probed-1.0.0/build.rs",
        r#"use std::process::Command;

fn probe(name: &str, code: &str) -> bool {
    let out = std::env::var("OUT_DIR").unwrap();
    let src = format!("{out}/{name}.rs");
    std::fs::write(&src, code).unwrap();
    let rustc = std::env::var("RUSTC").unwrap();
    let mut cmd = match std::env::var("RUSTC_WRAPPER") {
        Ok(w) if !w.is_empty() => { let mut c = Command::new(w); c.arg(rustc); c }
        _ => Command::new(rustc),
    };
    cmd.args(["--crate-name", name, "--crate-type", "lib", "--emit", "metadata", "--out-dir", &out, &src]);
    cmd.status().unwrap().success()
}

fn main() {
    assert!(probe("probe_works", "pub fn f() {}"));
    assert!(!probe("probe_fails", "pub fn f() -> u8 { \"no\" }"));
    println!("cargo::rustc-cfg=probed");
    println!("cargo::rustc-check-cfg=cfg(probed)");
}
"#,
    );
    sb.file("qrust-home/cargo/registry/src/fake-0000/probed-1.0.0/src/lib.rs", "#[cfg(probed)]\npub fn probed() -> bool {\n    true\n}\n");
    sb.file("app/Cargo.toml", &format!("[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nprobed = {{ path = {:?} }}\n\n[profile.dev.package.probed]\nincremental = false\n", sb.dir.join("qrust-home/cargo/registry/src/fake-0000/probed-1.0.0").display().to_string()));
    sb.file("app/src/main.rs", "fn main() {\n    assert!(probed::probed());\n}\n");
    let app = sb.dir.join("app");

    for round in ["first build", "after clean, with the cache warm"] {
        ok(sb.cargo(&app).arg("build"));
        let results = cache_results(&sb);
        for probe in ["probe_works", "probe_fails"] {
            let seen: Vec<_> = results.iter().filter(|(n, _)| n == probe).map(|(_, r)| r.as_str()).collect();
            assert_eq!(seen, vec!["off"], "{round}: {probe} ran through the wrapper and was kept out of the cache: {:#?}", cache_details(&sb));
        }
        ok(sb.cargo(&app).arg("clean"));
    }
}

#[test]
fn early_cutoff_rebuilds_what_an_edit_can_reach_and_links_the_edit() {
    let sb = Sandbox::new("cutoff");
    sb.file("w/Cargo.toml", "[workspace]\nmembers = [\"base\", \"mid\", \"app\"]\nresolver = \"2\"\n");
    sb.file("w/base/Cargo.toml", "[package]\nname = \"base\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    let base = sb.file("w/base/src/lib.rs", "pub fn greeting() -> String {\n    format!(\"{}-{}\", \"hello\", 1)\n}\n\npub fn wrap<T: std::fmt::Display>(t: T) -> String {\n    format!(\"<{}>\", t)\n}\n");
    sb.file("w/mid/Cargo.toml", "[package]\nname = \"mid\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nbase = { path = \"../base\" }\n");
    sb.file("w/mid/src/lib.rs", "pub fn message() -> String {\n    format!(\"{} {}\", base::greeting(), base::wrap(2))\n}\n");
    // The binary names only `mid`; `base` reaches it through `mid`.
    sb.file("w/app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nmid = { path = \"../mid\" }\n");
    sb.file("w/app/src/main.rs", "fn main() {\n    println!(\"{}\", mid::message());\n}\n");
    let w = sb.dir.join("w");
    build_driver(&sb, &w);
    // The binary links in a moment; by default such a unit is neither stored nor compiled apart.
    let cargo = || {
        let mut c = sb.cargo(&w);
        c.env("QRUST_EARLY_CUTOFF", "auto").env("QRUST_CACHE_ARTIFACTS_MIN_LINK_MS", "0");
        c
    };
    assert_eq!(stdout(&ok(cargo().args(["run", "-q"]))), "hello-1 <2>\n");
    assert_eq!(links(&sb), vec![("app".to_string(), "objects kept".to_string())], "{:#?}", cache_details(&sb));
    assert!(cache_details(&sb).iter().all(|d| !d.contains("driver_failure")), "{:#?}", cache_details(&sb));
    // The driver says when its outputs are written; the build does not wait for rustc to save
    // its incremental state.
    assert!(early_exits(&sb) >= 3, "{}", early_exits(&sb));

    // A body `mid` cannot observe: `mid` is restored as it was, `base` is compiled, and the
    // binary, which links `base`'s code, is linked anew.
    let text = fs::read_to_string(&base).unwrap();
    fs::write(&base, text.replace("\"hello\"", "\"howdy\"")).unwrap();
    assert_eq!(stdout(&ok(cargo().args(["run", "-q"]))), "howdy-1 <2>\n");
    let results = cache_results(&sb);
    assert!(results.contains(&("mid".to_string(), "hit".to_string())), "{:#?}", cache_details(&sb));
    // Its own objects unchanged (same sources, same interfaces below), the binary is only linked again.
    assert_eq!(links(&sb), vec![("app".to_string(), "relinked".to_string())], "{:#?}", cache_details(&sb));

    // A macro call added to that body (on the same line: lines below would move `wrap`, which
    // `mid` instantiates) is invisible to `mid` too.
    let text = fs::read_to_string(&base).unwrap();
    fs::write(&base, text.replace("    format!(\"{}-{}\", \"howdy\", 1)", "    { eprintln!(\"traced\"); format!(\"{}-{}\", \"howdy\", 1) }")).unwrap();
    let traced = ok(cargo().args(["run", "-q"]));
    assert_eq!((stdout(&traced).as_str(), stderr(&traced).contains("traced")), ("howdy-1 <2>\n", true), "{}", stderr(&traced));
    assert!(cache_results(&sb).contains(&("mid".to_string(), "hit".to_string())), "{:#?}", cache_details(&sb));
    assert_eq!(links(&sb), vec![("app".to_string(), "relinked".to_string())], "{:#?}", cache_details(&sb));
    assert!(results.iter().any(|(n, r)| n == "base" && r != "hit") && results.iter().any(|(n, r)| n == "app" && r != "hit"), "{:#?}", cache_details(&sb));

    // A generic body is compiled into `mid`: `mid` is compiled again.
    let text = fs::read_to_string(&base).unwrap();
    fs::write(&base, text.replace("\"<{}>\"", "\"[{}]\"")).unwrap();
    assert_eq!(stdout(&ok(cargo().args(["run", "-q"]))), "howdy-1 [2]\n");
    assert!(cache_results(&sb).iter().any(|(n, r)| n == "mid" && r != "hit"), "{:#?}", cache_details(&sb));
    // `mid`'s interface changed, which the binary's own code is compiled against.
    assert_eq!(links(&sb), vec![("app".to_string(), "objects kept".to_string())], "{:#?}", cache_details(&sb));

    // Kept objects are linked only as they were written: one changed since, and the binary is
    // compiled again rather than linked from it.
    let kept = fs::read_dir(sb.target_dir(&w).join("debug/deps")).unwrap().map(|e| e.unwrap().path()).find(|p| p.to_string_lossy().ends_with(".qrust-objects")).expect("the binary's objects are kept");
    let object = fs::read_dir(&kept).unwrap().next().unwrap().unwrap().path();
    let mut bytes = fs::read(&object).unwrap();
    bytes.push(0);
    fs::write(&object, bytes).unwrap();
    let text = fs::read_to_string(&base).unwrap();
    fs::write(&base, text.replace("\"howdy\"", "\"hiya!\"")).unwrap();
    assert_eq!(stdout(&ok(cargo().args(["run", "-q"]))), "hiya!-1 [2]\n");
    assert_eq!(links(&sb), vec![("app".to_string(), "objects kept".to_string())], "{:#?}", cache_details(&sb));
}

/// How many compilations of the latest instrumented run returned once their outputs were written.
fn early_exits(sb: &Sandbox) -> usize {
    let s = sb.sessions().into_iter().rev().find(|s| s["instrumented"] == json!(true)).unwrap();
    fs::read_dir(s["events_dir"].as_str().unwrap()).unwrap().filter_map(|e| serde_json::from_slice::<Value>(&fs::read(e.unwrap().path()).ok()?).ok()).filter(|ev| ev["early_exit"] == json!(true)).count()
}

/// The latest instrumented run's units QuickRust relinks: whether linked again or compiled (crate, how).
fn links(sb: &Sandbox) -> Vec<(String, String)> {
    let s = sb.sessions().into_iter().rev().find(|s| s["instrumented"] == json!(true)).unwrap();
    let mut out: Vec<(String, String)> = fs::read_dir(s["events_dir"].as_str().unwrap())
        .unwrap()
        .filter_map(|e| serde_json::from_slice::<Value>(&fs::read(e.unwrap().path()).ok()?).ok())
        .filter(|ev| ev["kind"] == "compile" && ev["link"].is_string())
        .map(|ev| {
            let args: Vec<&str> = ev["args"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
            let name = args.iter().position(|a| *a == "--crate-name").map(|i| args[i + 1].to_string()).unwrap_or_default();
            (name, ev["link"].as_str().unwrap().to_string())
        })
        .collect();
    out.sort();
    out
}

#[test]
fn a_crate_that_reexports_another_is_compiled_again_after_any_edit_of_it() {
    // `mid` re-exports `base`'s names: its metadata records where they are in `base`'s source, by
    // byte offset, which an edit inside a body moves without changing `base`'s interface.
    let sb = Sandbox::new("cutoff-reexport");
    sb.file("w/Cargo.toml", "[workspace]\nmembers = [\"base\", \"mid\", \"app\"]\nresolver = \"2\"\n");
    sb.file("w/base/Cargo.toml", "[package]\nname = \"base\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    let base = sb.file("w/base/src/lib.rs", "fn private() -> u32 {\n    1\n}\n\npub fn shown() -> u32 {\n    private() + 1\n}\n");
    sb.file("w/mid/Cargo.toml", "[package]\nname = \"mid\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nbase = { path = \"../base\" }\n");
    sb.file("w/mid/src/lib.rs", "pub use base::*;\n");
    sb.file("w/app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nmid = { path = \"../mid\" }\n");
    sb.file("w/app/src/main.rs", "fn main() {\n    println!(\"{}\", mid::shown());\n}\n");
    let w = sb.dir.join("w");
    build_driver(&sb, &w);
    let cargo = || {
        let mut c = sb.cargo(&w);
        c.env("QRUST_EARLY_CUTOFF", "auto");
        c
    };
    assert_eq!(stdout(&ok(cargo().args(["run", "-q"]))), "2\n");
    fs::write(&base, fs::read_to_string(&base).unwrap().replace("    1\n", "    1000\n")).unwrap();
    assert_eq!(stdout(&ok(cargo().args(["run", "-q"]))), "1001\n");
    assert!(cache_results(&sb).iter().any(|(n, r)| n == "mid" && r != "hit"), "{:#?}", cache_details(&sb));
}

#[test]
fn cargo_check_caches_binaries_too() {
    // A binary `cargo check` checks writes no linked output: it is keyed as a unit that links nothing.
    let sb = Sandbox::new("check-bins");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).arg("check"));
    assert!(cache_results(&sb).iter().all(|(_, r)| r == "stored"), "{:#?}", cache_details(&sb));
    ok(sb.cargo(&p).arg("clean"));
    ok(sb.cargo(&p).arg("check"));
    assert!(cache_results(&sb).iter().all(|(_, r)| r == "hit"), "{:#?}", cache_details(&sb));
}

#[test]
fn a_compiler_driver_that_cannot_be_loaded_never_fails_the_build() {
    // As when the toolchain it was built in is gone: the loader fails with 127.
    let sb = Sandbox::new("driver-unloadable");
    let p = sb.package("p", "p");
    let driver = build_driver(&sb, &p);
    fs::write(&driver, "#!/bin/sh\necho \"qrust-rustc-driver: error while loading shared libraries: librustc_driver.so: cannot open shared object file\" >&2\nexit 127\n").unwrap();
    make_executable(&driver);
    let out = ok(sb.cargo(&p).arg("build").env("QRUST_EARLY_CUTOFF", "auto"));
    assert!(stderr(&out).contains("QuickRust's compiler driver failed on crate `p`; compiled with rustc instead"), "{}", stderr(&out));
    assert_eq!(stdout(&ok(sb.cargo(&p).args(["run", "-q"]).env("QRUST_EARLY_CUTOFF", "auto"))), "3\n");
}

#[test]
fn cargo_clippy_runs_clippy_when_the_compiler_driver_is_built() {
    let sb = Sandbox::new("driver-clippy");
    let p = sb.package("p", "p");
    if !run(sb.upstream(&p).args(["clippy", "--version"])).status.success() {
        return;
    }
    sb.file("p/src/lib.rs", "pub fn add(x: u32, y: u32) -> u32 {\n    x + y\n}\n\npub fn len(x: &Vec<u8>) -> usize {\n    x.len()\n}\n");
    build_driver(&sb, &p);
    // clippy-driver describes itself exactly as rustc does; it is still clippy that runs.
    let out = ok(sb.cargo(&p).args(["clippy"]).env("QRUST_EARLY_CUTOFF", "auto"));
    assert!(stderr(&out).contains("writing `&Vec` instead of `&[_]`"), "{}", stderr(&out));
}

#[test]
fn a_compiler_driver_that_crashes_never_fails_the_build() {
    let sb = Sandbox::new("driver-crash");
    let p = sb.package("p", "p");
    let driver = build_driver(&sb, &p);
    // A driver that fails as an internal compiler error does, wherever QuickRust looks for this
    // compiler's driver.
    fs::write(&driver, "#!/bin/sh\necho \"thread 'rustc' panicked at compiler/rustc_middle/src/lib.rs:1:1:\" >&2\necho \"error: the compiler unexpectedly panicked. this is a bug.\" >&2\nexit 101\n").unwrap();
    make_executable(&driver);

    let cargo = || {
        let mut c = sb.cargo(&p);
        c.env("QRUST_EARLY_CUTOFF", "auto");
        c
    };
    let first = ok(cargo().arg("build"));
    let err = stderr(&first);
    assert!(err.contains("QuickRust's compiler driver failed on crate `p`; compiled with rustc instead"), "{}", err);
    assert!(!err.contains("panicked"), "the driver's report is kept aside, not shown: {}", err);
    let reports: Vec<_> = fs::read_dir(driver.with_file_name("failed")).unwrap().map(|e| fs::read_to_string(e.unwrap().path()).unwrap()).collect();
    assert!(!reports.is_empty() && reports.iter().all(|r| r.contains("panicked")), "{:?}", reports);
    assert!(cache_details(&sb).iter().any(|d| d.contains("compiled by rustc after QuickRust's compiler driver failed")), "{:#?}", cache_details(&sb));
    assert_eq!(stdout(&ok(cargo().args(["run", "-q"]))), "3\n");

    // The units it failed on are compiled by rustc from then on, without a word.
    let lib = p.join("src/lib.rs");
    fs::write(&lib, fs::read_to_string(&lib).unwrap().replace("x + y", "y + x")).unwrap();
    let again = ok(cargo().arg("build"));
    assert!(!stderr(&again).contains("compiler driver failed"), "{}", stderr(&again));
    assert_eq!(stdout(&ok(cargo().args(["run", "-q"]))), "3\n");
}

#[test]
fn setup_prepares_the_compiler_driver_so_the_first_build_uses_it() {
    // The driver is part of an artifact-cache entry's key: what a first build stored without it,
    // every later build (with it) would miss.
    let sb = Sandbox::new("driver-setup");
    let p = sb.package("p", "p");
    let out = run(sb.qrust(&p).args(["setup", "--toolchain", "1.99.0", "--no-modify-path"]).env("HOME", sb.dir.join("user")).env("QRUST_EARLY_CUTOFF", "auto"));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("Compiler driver for Rust 1.99.0 ready"), "{}", stderr(&out));
    assert!(stdout(&ok(sb.qrust(&p).args(["driver", "status"]))).starts_with("built:"));
    let Some(mut first) = sb.cargo_in_terminal(&p, "build") else { return };
    first.env("QRUST_EARLY_CUTOFF", "auto");
    let screen = String::from_utf8_lossy(&run(&mut first).stdout).into_owned();
    assert!(!screen.contains("compiler driver"), "{}", screen);
    let s = sb.sessions().into_iter().rev().find(|s| s["instrumented"] == json!(true)).unwrap();
    let compiled: Vec<Value> = fs::read_dir(s["events_dir"].as_str().unwrap()).unwrap().filter_map(|e| serde_json::from_slice::<Value>(&fs::read(e.unwrap().path()).ok()?).ok()).filter(|ev| ev["kind"] == "compile").collect();
    assert!(!compiled.is_empty() && compiled.iter().all(|ev| ev["driver"].is_string()), "{:#?}", compiled.iter().map(|ev| &ev["driver"]).collect::<Vec<_>>());
}

#[test]
fn the_compiler_driver_is_built_in_the_background_once_and_then_used() {
    let sb = Sandbox::new("driver-background");
    let p = sb.package("p", "p");
    let Some(mut first) = sb.cargo_in_terminal(&p, "build") else { return };
    first.env("QRUST_EARLY_CUTOFF", "auto");
    let screen = String::from_utf8_lossy(&run(&mut first).stdout).into_owned();
    assert!(screen.contains("building QuickRust's compiler driver for Rust") && screen.contains("in the background"), "{}", screen);
    // Built once: a build while it runs, or after, starts no other.
    let status = || stdout(&ok(sb.qrust(&p).args(["driver", "status"])));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    while !status().starts_with("built:") {
        assert!(!status().starts_with("could not be built"), "{}", status());
        assert!(std::time::Instant::now() < deadline, "the background build did not finish: {}", status());
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let lib = p.join("src/lib.rs");
    fs::write(&lib, fs::read_to_string(&lib).unwrap().replace("x + y", "y + x")).unwrap();
    let mut second = sb.cargo_in_terminal(&p, "build").unwrap();
    second.env("QRUST_EARLY_CUTOFF", "auto");
    let screen = String::from_utf8_lossy(&run(&mut second).stdout).into_owned();
    assert!(!screen.contains("compiler driver"), "{}", screen);
    let s = sb.sessions().into_iter().rev().find(|s| s["instrumented"] == json!(true)).unwrap();
    let compiled: Vec<Value> = fs::read_dir(s["events_dir"].as_str().unwrap()).unwrap().filter_map(|e| serde_json::from_slice::<Value>(&fs::read(e.unwrap().path()).ok()?).ok()).filter(|ev| ev["kind"] == "compile").collect();
    assert!(!compiled.is_empty() && compiled.iter().all(|ev| ev["driver"].is_string()), "{:#?}", compiled.iter().map(|ev| &ev["driver"]).collect::<Vec<_>>());
}

#[test]
fn artifact_cache_restores_dependencies_across_cleans_and_checkouts() {
    let sb = Sandbox::new("artifacts");
    // A dependency whose sources sit in the (sandbox) Cargo home's registry: immutable, cacheable.
    // Its warning must reach the user on every build, cached or not.
    let dep = sb.dir.join("qrust-home/cargo/registry/src/fake-0000/greet-1.0.0");
    sb.file("qrust-home/cargo/registry/src/fake-0000/greet-1.0.0/Cargo.toml", "[package]\nname = \"greet\"\nversion = \"1.0.0\"\nedition = \"2021\"\n");
    // Like serde: a build script generates code that the library includes through OUT_DIR,
    // whose path differs in every target directory.
    sb.file("qrust-home/cargo/registry/src/fake-0000/greet-1.0.0/build.rs", "fn main() {\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    std::fs::write(format!(\"{out}/greeting.rs\"), \"const GREETING: &str = \\\"hello from the cache\\\";\").unwrap();\n    println!(\"cargo::rerun-if-changed=build.rs\");\n}\n");
    sb.file("qrust-home/cargo/registry/src/fake-0000/greet-1.0.0/src/lib.rs", "include!(concat!(env!(\"OUT_DIR\"), \"/greeting.rs\"));\n\npub fn greet() -> &'static str {\n    let unused = 1;\n    GREETING\n}\n");
    let app = |rel: &str| {
        // A path dependency is compiled incrementally, which a registry one never is.
        sb.file(&format!("{}/Cargo.toml", rel), &format!("[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ngreet = {{ path = {:?} }}\n\n[profile.dev.package.greet]\nincremental = false\n", dep.display().to_string()));
        sb.file(&format!("{}/src/main.rs", rel), "fn main() {\n    println!(\"{}\", greet::greet());\n}\n");
        sb.dir.join(rel)
    };
    let (a, b) = (app("a"), app("b"));
    // The binary links in milliseconds; by default such linked workspace outputs are not stored.
    let cargo = |dir: &std::path::Path| {
        let mut c = sb.cargo(dir);
        c.env("QRUST_CACHE_ARTIFACTS_MIN_LINK_MS", "0");
        c
    };

    let first = ok(cargo(&a).arg("build"));
    assert!(stderr(&first).contains("unused variable"), "{}", stderr(&first));
    assert_eq!(cache_results(&sb), vec![("app".to_string(), "stored".to_string()), ("build_script_build".to_string(), "stored".to_string()), ("greet".to_string(), "stored".to_string())], "{:#?}", cache_details(&sb));

    ok(cargo(&a).arg("clean"));
    let again = ok(cargo(&a).args(["run", "-q"]));
    assert_eq!(stdout(&again), "hello from the cache\n");
    assert!(stderr(&again).contains("unused variable"), "a cached unit's warnings are replayed: {}", stderr(&again));
    assert!(cache_results(&sb).iter().all(|(_, r)| r == "hit"), "after a clean, the same checkout is restored entirely: {:?}", cache_results(&sb));
    // The report tells restored units apart from compiled ones.
    let r = sb.report(&a, "latest");
    assert_eq!((r["metrics"]["units_compiled"].as_u64(), r["metrics"]["units_restored"].as_u64()), (Some(0), Some(3)), "{}", r["metrics"]);
    assert_eq!(r["cache"]["artifacts"]["restored"], 3);
    assert_eq!(r["cache"]["artifacts"]["restored_with_foreign_out_dir"], 0, "the same target directory: its own OUT_DIR");
    assert!(r["units"].as_array().unwrap().iter().filter(|u| u["kind"] != "build-script-run" && u["freshness"] != "fresh").all(|u| u["freshness"] == "restored"));

    // A workspace source that changed is never served from the cache; reverting it is.
    let main = a.join("src/main.rs");
    let original = fs::read_to_string(&main).unwrap();
    fs::write(&main, original.replace("greet::greet()", "greet::greet().to_uppercase()")).unwrap();
    assert_eq!(stdout(&ok(cargo(&a).args(["run", "-q"]))), "HELLO FROM THE CACHE\n");
    assert!(cache_results(&sb).contains(&("app".to_string(), "stored".to_string())));
    fs::write(&main, &original).unwrap();
    assert_eq!(stdout(&ok(cargo(&a).args(["run", "-q"]))), "hello from the cache\n");
    assert!(cache_results(&sb).contains(&("app".to_string(), "hit".to_string())));

    // Another checkout reuses the dependency, never the workspace crate (its debug info names its checkout),
    // and Cargo then considers everything fresh.
    ok(cargo(&b).arg("build"));
    assert!(cache_results(&sb).contains(&("greet".to_string(), "hit".to_string())));
    assert!(cache_results(&sb).contains(&("app".to_string(), "stored".to_string())));
    // `greet` includes code its build script generated in checkout a's target directory: the
    // user is told, and the report counts it.
    assert!(cache_details(&sb).iter().any(|d| d.starts_with("greet:") && d.contains("foreign_out_dir") && d.contains(&format!("{}/", sb.target_dir(&a).display()))), "{:#?}", cache_details(&sb));
    assert_eq!(sb.report(&b, "latest")["cache"]["artifacts"]["restored_with_foreign_out_dir"], 1);
    // In a terminal, QuickRust says so once.
    let (c, d) = (app("c"), app("d"));
    if let Some(mut first) = sb.cargo_in_terminal(&c, "build") {
        let screen = String::from_utf8_lossy(&run(&mut first).stdout).into_owned();
        assert!(screen.contains("NOTE ") && screen.contains("another target directory"), "{}", screen);
        let screen = String::from_utf8_lossy(&run(&mut sb.cargo_in_terminal(&d, "build").unwrap()).stdout).into_owned();
        assert!(!screen.contains("another target directory"), "shown once: {}", screen);
    }
    let fresh = ok(cargo(&b).arg("build"));
    assert!(!stderr(&fresh).contains("Compiling"), "a restored unit must be fresh for Cargo afterwards: {}", stderr(&fresh));
    assert_eq!(stdout(&ok(cargo(&b).args(["run", "-q"]))), "hello from the cache\n");

    // compat mode never uses the cache.
    ok(cargo(&a).arg("clean"));
    ok(cargo(&a).arg("build").env("QRUST_MODE", "compat"));
    assert!(cache_results(&sb).iter().all(|(_, r)| r == "off"), "{:?}", cache_results(&sb));
}

#[test]
fn a_build_in_a_terminal_is_rendered_and_its_output_kept() {
    let sb = Sandbox::new("render");
    let p = sb.package("p", "p");
    sb.file("p/src/lib.rs", "pub fn add(x: u32, y: u32) -> u32 {\n    let unused = 1;\n    x + y\n}\n");
    let Some(mut build) = sb.cargo_in_terminal(&p, "build") else {
        eprintln!("skipped: no util-linux `script` for a pseudo-terminal");
        return;
    };
    let out = run(&mut build);
    let screen = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{}", screen);
    assert!(screen.contains("OK   Built: "), "a summary: {}", screen);
    assert!(!screen.contains("   Compiling p v0.1.0"), "no line per crate: {}", screen);
    assert!(!screen.contains("let unused = 1"), "warnings are counted, not shown: {}", screen);
    assert!(screen.contains("WARN 1 warning in p: unused_variables x1"), "{}", screen);
    // Everything is kept: the warning is shown on request, in rustc's words.
    let shown = stdout(&ok(sb.qrust(&p).arg("warnings")));
    assert!(shown.contains("unused variable: `unused`") && shown.contains("src/lib.rs:2"), "{}", shown);
    assert!(stdout(&ok(sb.qrust(&p).args(["warnings", "--lint", "dead_code"]))).contains("No warnings"));

    // An error is shown in full, with a summary that points at it.
    sb.file("p/src/lib.rs", "pub fn add(x: u32, y: u32) -> u32 {\n    x + \"y\"\n}\n");
    let out = run(&mut sb.cargo_in_terminal(&p, "build").unwrap());
    let screen = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(!out.status.success());
    assert!(screen.contains("cannot add `&str` to `u32`"), "the error in full: {}", screen);
    assert!(screen.contains("FAIL Build: ") && screen.contains("first at src/lib.rs:2:"), "{}", screen);
    assert!(stdout(&ok(sb.qrust(&p).arg("errors"))).contains("cannot add `&str` to `u32`"));

    // Through a pipe, Cargo's own output is untouched.
    let piped = run(sb.cargo(&p).arg("build"));
    assert!(stderr(&piped).contains("error[E0277]") && !stderr(&piped).contains("FAIL Build"));

    // With colour, the same holds and rustc's own colours are kept.
    sb.file("p/src/lib.rs", "pub fn add(x: u32, y: u32) -> u32 {\n    let unused = 1;\n    x + y\n}\n");
    let out = run(sb.cargo_in_terminal(&p, "build").unwrap().env("CARGO_TERM_COLOR", "always"));
    let screen = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success() && screen.contains("\x1b[1;32mOK  \x1b[0m Built: "), "the summary's tag in colour: {:?}", screen);
    assert!(stdout(&ok(sb.qrust(&p).arg("warnings"))).contains("unused variable: `unused`"));

    // A unit restored from QuickRust's cache is counted once, as restored, though Cargo reports it
    // as compiled (it ran the compiler, QuickRust's wrapper, for it). Of the two units, the library
    // is restored; the binary too, unless it linked faster than the cache stores.
    ok(sb.cargo(&p).arg("clean"));
    let out = run(&mut sb.cargo_in_terminal(&p, "build").unwrap());
    let screen = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{}", screen);
    let summary = screen.lines().find_map(|l| l.split("OK   Built: ").nth(1)).and_then(|s| s.split(" [").next()).unwrap_or_else(|| panic!("no summary: {}", screen));
    let count = |what: &str| summary.split(", ").find_map(|c| c.strip_suffix(what)).map_or(0, |n| n.parse::<u32>().unwrap());
    let (compiled, restored) = (count(" compiled"), count(" restored from QuickRust's cache"));
    assert!(restored >= 1 && compiled + restored == 2, "each unit once: {}", summary);
}

#[test]
fn run_and_test_in_a_terminal_build_first_then_hand_the_program_the_terminal() {
    let sb = Sandbox::new("render-run");
    let p = sb.package("p", "p");
    // The programs report the Cargo settings they inherit: QuickRust's settings for the Cargo whose
    // output it renders (a progress bar 1000 columns wide, drawn into a pipe) are not theirs.
    sb.file("p/src/lib.rs", "pub fn add(x: u32, y: u32) -> u32 {\n    let unused = 1;\n    x + y\n}\n\n#[test]\nfn adds() {\n    assert_eq!(add(2, 2), 4);\n    assert_eq!(std::env::vars().filter(|(k, _)| k.starts_with(\"CARGO_TERM_PROGRESS\")).count(), 0);\n}\n");
    sb.file("p/src/main.rs", "use std::io::IsTerminal;\nfn main() {\n    let args: Vec<String> = std::env::args().skip(1).collect();\n    let term: Vec<String> = std::env::vars().map(|(k, _)| k).filter(|k| k.starts_with(\"CARGO_TERM_PROGRESS\")).collect();\n    println!(\"terminal={} args={:?} sum={} term={:?}\", std::io::stdout().is_terminal(), args, p::add(1, 2), term);\n}\n");
    let Some(mut first) = sb.cargo_in_terminal(&p, "run -- one two") else {
        eprintln!("skipped: no util-linux `script` for a pseudo-terminal");
        return;
    };
    // Every unit compiled in the latest session, by its `-C metadata`: one compilation each.
    let compiled_once = |sb: &Sandbox| {
        let s = sb.sessions().into_iter().rev().find(|s| s["instrumented"] == json!(true)).unwrap();
        let mut units: Vec<String> = fs::read_dir(s["events_dir"].as_str().unwrap())
            .unwrap()
            .filter_map(|e| serde_json::from_slice::<Value>(&fs::read(e.unwrap().path()).ok()?).ok())
            .filter(|e| e["kind"] == "compile")
            .map(|e| e["args"].as_array().unwrap().iter().filter_map(Value::as_str).find(|a| a.starts_with("metadata=")).unwrap_or("").to_string())
            .collect();
        let n = units.len();
        units.sort();
        units.dedup();
        assert!(n > 0 && units.len() == n, "a unit was compiled twice: {:?}", units);
    };

    let out = run(&mut first);
    let screen = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{}", screen);
    assert!(screen.contains("OK   Built: ") && screen.contains("WARN 1 warning in p: unused_variables x1"), "QuickRust's summary of the build: {}", screen);
    assert!(screen.contains(r#"terminal=true args=["one", "two"] sum=3 term=[]"#), "the program has the terminal, its arguments and none of QuickRust's Cargo settings: {}", screen);
    // The Cargo that runs the program draws no progress either (nor reports it to the terminal).
    assert!(!screen.contains("\x1b]9;4"), "{:?}", screen);
    assert_eq!(screen.matches("terminal=").count(), 1, "the program ran once: {}", screen);
    assert!(!screen.contains("let unused = 1") && !screen.contains(crate_noop()), "nothing replayed, nothing of the build's mechanics: {}", screen);
    compiled_once(&sb);

    // Nothing changed: QuickRust knows the build is fresh and goes straight to the program; Cargo,
    // running it, still checks.
    let out = run(&mut sb.cargo_in_terminal(&p, "run -- again").unwrap());
    let screen = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success() && screen.contains("OK   Up to date") && screen.contains("WARN 1 warning in p"), "{}", screen);
    assert!(screen.contains(r#"terminal=true args=["again"] sum=3 term=[]"#), "{}", screen);
    // The warning that line counts is there to read, from the run that said it.
    let shown = stdout(&ok(sb.qrust(&p).arg("warnings")));
    assert!(shown.contains("unused variable: `unused`"), "{}", shown);
    // An edit is built again.
    sb.file("p/src/lib.rs", "pub fn add(x: u32, y: u32) -> u32 {\n    let unused = 1;\n    y + x\n}\n\n#[test]\nfn adds() {\n    assert_eq!(add(2, 2), 4);\n}\n");
    let out = run(&mut sb.cargo_in_terminal(&p, "run").unwrap());
    let screen = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success() && screen.contains("OK   Built: ") && !screen.contains("Up to date") && screen.contains("sum=3"), "{}", screen);
    compiled_once(&sb);

    let out = run(&mut sb.cargo_in_terminal(&p, "test").unwrap());
    let screen = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success() && screen.contains("OK   Built: ") && screen.contains("test adds ... ") && screen.contains("OK   Tests: 1 passed"), "{}", screen);
    assert!(!screen.contains("let unused = 1") && !screen.contains("Compiling p v0.1.0"), "{}", screen);
    compiled_once(&sb);

    // A build that fails runs nothing, and the exit status is Cargo's.
    sb.file("p/src/main.rs", "fn main() {\n    let x: u32 = \"no\";\n}\n");
    let out = run(&mut sb.cargo_in_terminal(&p, "run").unwrap());
    let screen = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(101), "{}", screen);
    assert!(screen.contains("FAIL Build: ") && screen.contains("mismatched types") && !screen.contains("terminal="), "{}", screen);
}

/// The argument under which QuickRust is the do-nothing runner of `cargo run`'s build.
fn crate_noop() -> &'static str {
    "__qrust-noop-runner"
}

#[test]
fn quick_workspace_binaries_are_not_stored_but_libraries_always_are() {
    // Copying a freshly linked binary on every edit costs about what restoring it saves; a library
    // is cheap to store and sits on the path a clean rebuild waits for.
    let sb = Sandbox::new("store-rule");
    let p = sb.package("p", "p");
    // A threshold no compilation here reaches, however busy the machine: the rule, not its speed.
    ok(sb.cargo(&p).arg("build").env("QRUST_CACHE_ARTIFACTS_MIN_LINK_MS", "600000"));
    let s = sb.sessions().into_iter().rev().find(|s| s["instrumented"] == json!(true)).unwrap();
    let results: Vec<(String, String, String)> = fs::read_dir(s["events_dir"].as_str().unwrap())
        .unwrap()
        .filter_map(|e| serde_json::from_slice::<Value>(&fs::read(e.unwrap().path()).ok()?).ok())
        .filter(|e| e["kind"] == "compile")
        .map(|e| {
            let args: Vec<&str> = e["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
            let kind = args.iter().position(|a| *a == "--crate-type").map(|i| args[i + 1].to_string()).unwrap_or_default();
            (kind, e["cache"]["result"].as_str().unwrap_or("").to_string(), e["cache"]["not_stored"].as_str().unwrap_or("").to_string())
        })
        .collect();
    assert!(results.iter().any(|(k, r, _)| k == "lib" && r == "stored"), "{:#?}", cache_details(&sb));
    assert!(results.iter().any(|(k, r, why)| k == "bin" && r == "miss" && why == "quick to link"), "{:?}", results);
}

/// Which linker produced a binary, from its `.comment` section ("Linker: Wild ..." / "Linker: LLD ...").
fn linked_by(binary: &std::path::Path) -> &'static str {
    let bytes = fs::read(binary).unwrap();
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    if has(b"Linker: Wild") {
        "wild"
    } else if has(b"Linker: LLD") {
        "lld"
    } else {
        "other"
    }
}

#[test]
fn fast_mode_links_with_wild_unless_a_linker_is_chosen() {
    if !cfg!(all(target_os = "linux", target_env = "gnu")) || std::process::Command::new("wild").arg("--version").output().is_err() {
        eprintln!("skipped: wild is not installed (cargo install wild-linker)");
        return;
    }
    let sb = Sandbox::new("wild");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).arg("build"));
    assert_eq!(linked_by(&sb.target_dir(&p).join("debug/p")), "wild");
    assert_eq!(stdout(&ok(sb.cargo(&p).args(["run", "-q"]))), "3\n", "the wild-linked program works");
    let r = sb.report(&p, "latest");
    assert!(r["policy"]["fast"]["applied"].as_array().unwrap().iter().any(|a| a["key"] == "linker"), "{}", r["policy"]["fast"]);
    // A linker the user chose wins; compat mode keeps rustc's own.
    fs::remove_dir_all(sb.target_dir(&p)).unwrap();
    ok(sb.cargo(&p).arg("build").env("RUSTFLAGS", "-C link-arg=-fuse-ld=lld"));
    assert_eq!(linked_by(&sb.target_dir(&p).join("debug/p")), "lld");
    fs::remove_dir_all(sb.target_dir(&p)).unwrap();
    ok(sb.cargo(&p).arg("build").env("QRUST_MODE", "compat"));
    assert_eq!(linked_by(&sb.target_dir(&p).join("debug/p")), "lld");
}


#[test]
fn compatible_versions_of_one_crate_from_two_sources_are_found_in_a_real_resolution() {
    // Cargo resolves one SemVer-compatible version per source, so two compatible versions side
    // by side come from different sources: here two path crates named `dup`, one reached as a
    // dev-dependency, the other through a platform-specific dependency of `mid`.
    let sb = Sandbox::new("versions");
    sb.file("dup-a/Cargo.toml", "[package]\nname = \"dup\"\nversion = \"1.1.0\"\nedition = \"2021\"\n");
    sb.file("dup-a/src/lib.rs", "pub fn which() -> &'static str { \"1.1\" }\n");
    sb.file("dup-b/Cargo.toml", "[package]\nname = \"dup\"\nversion = \"1.2.0\"\nedition = \"2021\"\n");
    sb.file("dup-b/src/lib.rs", "pub fn which() -> &'static str { \"1.2\" }\n");
    sb.file("mid/Cargo.toml", "[package]\nname = \"mid\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[target.'cfg(unix)'.dependencies]\ndup = { path = \"../dup-b\" }\n");
    sb.file("mid/src/lib.rs", "pub fn which() -> &'static str { dup::which() }\n");
    sb.file("app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nmid = { path = \"../mid\" }\n\n[dev-dependencies]\ndup = { path = \"../dup-a\" }\n");
    sb.file("app/src/main.rs", "fn main() {\n    println!(\"{}\", mid::which());\n}\n");
    let app = sb.dir.join("app");
    ok(sb.cargo(&app).arg("build"));
    let r = sb.report(&app, "latest");

    let dup = r["duplication"]["versions"].as_array().unwrap().iter().find(|d| d["name"] == "dup").unwrap_or_else(|| panic!("{}", r["duplication"]));
    assert_eq!(dup["state"], "separate-sources", "{}", dup);
    let version = |v: &str| dup["versions"].as_array().unwrap().iter().find(|x| x["version"] == v).unwrap().clone();
    let (old, new) = (version("1.1.0"), version("1.2.0"));
    assert!(old["source"].is_null() && new["source"].is_null(), "path packages have no source: {}", dup);
    assert_eq!(old["required_by"], json!(["app@0.1.0", "dup@1.1.0 [dev]"]), "{}", old);
    assert_eq!(new["required_by"], json!(["app@0.1.0", "mid@0.1.0", "dup@1.2.0"]), "{}", new);
    // `cargo build` compiled the one `mid` uses; the dev-dependency was not built.
    assert!(new["observed_compile_ns"].as_u64().is_some_and(|ns| ns > 0), "{}", new);
    assert!(old["observed_compile_ns"].is_null(), "{}", old);

    let rec = r["recommendations"].as_array().unwrap().iter().find(|x| x["rule"] == "duplicate_dependency_versions").unwrap_or_else(|| panic!("{}", r["recommendations"]));
    assert_eq!(rec["title"], "`dup` is built from 2 different sources");
    assert_eq!(rec["impact"]["kind"], "unknown", "the saving needs both versions observed");
    assert!(rec["verification"].as_str().unwrap().contains("[patch]"), "{}", rec);
}

#[test]
fn analyze_filters_sorts_and_limits_a_sections_records() {
    let sb = Sandbox::new("filters");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).arg("build"));
    let json_units = |args: &[&str]| -> Vec<Value> {
        let out = ok(sb.qrust(&p).args(["analyze", "timing", "--format", "json"]).args(args));
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["units"].as_array().unwrap().clone()
    };
    let all = json_units(&[]);
    assert!(all.len() >= 2, "{:?}", all);
    assert!(all.iter().all(|u| !u["artifact_cache"].is_null()), "every compiled unit says what the artifact cache did: {:?}", all);
    let bins = json_units(&["--filter", "kind=bin"]);
    assert!(!bins.is_empty() && bins.iter().all(|u| u["kind"] == "bin"), "{:?}", bins);
    let slowest = json_units(&["--sort", "duration_ns", "--limit", "1"]);
    let max = all.iter().filter_map(|u| u["duration_ns"].as_u64()).max().unwrap();
    assert_eq!((slowest.len(), slowest[0]["duration_ns"].as_u64()), (1, Some(max)));

    // Text shows exactly the records asked for, including the ones its default view leaves out.
    let text = stdout(&ok(sb.qrust(&p).args(["analyze", "rebuilds", "--filter", "reason=new", "--limit", "1"])));
    assert!(text.contains("new") && text.contains("first time this unit is observed"), "{}", text);
    // A filter nothing matches says so, not that the run compiled nothing.
    let text = stdout(&ok(sb.qrust(&p).args(["analyze", "timing", "--filter", "source=registry"])));
    assert!(text.starts_with("Timing\n  None of the ") && text.contains(" records matches source=registry."), "{}", text);
    // A filter that matches only units Cargo found fresh (known in a terminal build) says that none
    // of those was compiled, not that the run compiled nothing.
    sb.file("p/src/main.rs", "fn main() {\n    println!(\"{}\", p::add(2, 3));\n}\n");
    if let Some(mut build) = sb.cargo_in_terminal(&p, "build") {
        assert!(run(&mut build).status.success());
        let text = stdout(&ok(sb.qrust(&p).args(["analyze", "timing", "--filter", "kind=lib"])));
        assert_eq!(text, "Timing\n  No unit the filter matches was compiled in this run.\n");
        assert!(stdout(&ok(sb.qrust(&p).args(["analyze", "timing", "--filter", "kind=bin"]))).contains("units (1 of 1):"));
    }
    // Mistakes are named, never silently ignored.
    let e = run(sb.qrust(&p).args(["analyze", "timing", "--filter", "profile=release"]));
    assert!(!e.status.success() && stderr(&e).contains("no field `profile`"), "{}", stderr(&e));
    let e = run(sb.qrust(&p).args(["analyze", "--limit", "3"]));
    assert!(!e.status.success() && stderr(&e).contains("one section"), "{}", stderr(&e));
    let e = run(sb.qrust(&p).args(["analyze", "cache", "--sort", "x"]));
    assert!(!e.status.success() && stderr(&e).contains("no list of records"), "{}", stderr(&e));
}

#[test]
#[cfg(unix)]
fn a_command_that_prints_ends_quietly_when_its_reader_goes() {
    use std::os::unix::process::ExitStatusExt;
    let sb = Sandbox::new("pipe");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).arg("build"));
    for args in [&["runs"][..], &["help"], &["analyze"]] {
        let mut child = sb.qrust(&p).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        // The reader is gone before the command writes (`qrust runs | head -0`).
        drop(child.stdout.take());
        let out = child.wait_with_output().unwrap();
        assert!(!stderr(&out).contains("panicked") && !stderr(&out).contains("Broken pipe"), "{:?}: {}", args, stderr(&out));
        assert!(out.status.success() || out.status.signal() == Some(libc::SIGPIPE), "{:?}: {:?}", args, out.status);
    }
}

#[test]
fn qrust_says_up_to_date_only_when_cargo_would_build_nothing() {
    // The gate for QuickRust judging freshness itself: after every kind of change, a build QuickRust calls
    // up to date must be one Cargo, asked directly, finds entirely fresh.
    let sb = Sandbox::new("oracle");
    let ws = sb.dir.join("ws");
    sb.file("ws/Cargo.toml", "[workspace]\nmembers = [\"app\", \"data\", \"assets\"]\nresolver = \"2\"\n");
    // A build script that declares nothing: Cargo reruns it when any file of its package changes.
    sb.file("ws/data/Cargo.toml", "[package]\nname = \"data\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    sb.file("ws/data/build.rs", "fn main() {\n    let text = std::fs::read_to_string(\"data.txt\").unwrap();\n    std::fs::write(std::path::Path::new(&std::env::var(\"OUT_DIR\").unwrap()).join(\"data.rs\"), format!(\"pub const DATA: &str = {:?};\", text.trim())).unwrap();\n}\n");
    sb.file("ws/data/data.txt", "one\n");
    sb.file("ws/data/notes.md", "notes\n");
    sb.file("ws/data/src/lib.rs", "include!(concat!(env!(\"OUT_DIR\"), \"/data.rs\"));\n");
    // One that declares a directory.
    sb.file("ws/assets/Cargo.toml", "[package]\nname = \"assets\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    sb.file("ws/assets/build.rs", "fn main() {\n    println!(\"cargo::rerun-if-changed=files\");\n    let n = std::fs::read_dir(\"files\").unwrap().count();\n    println!(\"cargo::rustc-env=ASSET_COUNT={}\", n);\n}\n");
    sb.file("ws/assets/files/a.txt", "a\n");
    sb.file("ws/assets/README.md", "readme\n");
    sb.file("ws/assets/src/lib.rs", "pub const COUNT: &str = env!(\"ASSET_COUNT\");\n");
    sb.file("ws/app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ndata = { path = \"../data\" }\nassets = { path = \"../assets\" }\n");
    sb.file("ws/app/src/main.rs", "fn main() {\n    println!(\"{} {} {}\", data::DATA, assets::COUNT, option_env!(\"GREETING\").unwrap_or(\"-\"));\n}\n");

    let Some(_) = sb.cargo_in_terminal(&ws, "build") else {
        eprintln!("skipped: no util-linux `script` for a pseudo-terminal");
        return;
    };
    let pause = || std::thread::sleep(std::time::Duration::from_millis(20));
    let edit = |rel: &str, text: &str| {
        pause();
        sb.file(&format!("ws/{}", rel), text);
    };
    type Change<'a> = (&'a str, Box<dyn Fn() + 'a>, Option<(&'a str, &'a str)>);
    let changes: Vec<Change> = vec![
        ("nothing", Box::new(|| {}), None),
        ("edit a source", Box::new(|| edit("app/src/main.rs", "fn main() {\n    println!(\"{}|{}|{}\", data::DATA, assets::COUNT, option_env!(\"GREETING\").unwrap_or(\"-\"));\n}\n")), None),
        ("nothing", Box::new(|| {}), None),
        ("touch a source", Box::new(|| {
            pause();
            fs::File::options().write(true).open(ws.join("data/src/lib.rs")).unwrap().set_modified(std::time::SystemTime::now()).unwrap();
        }), None),
        ("edit an undeclared build script input", Box::new(|| edit("data/data.txt", "two\n")), None),
        ("edit another file of that package", Box::new(|| edit("data/notes.md", "more notes\n")), None),
        ("add a file to a declared directory", Box::new(|| edit("assets/files/b.txt", "b\n")), None),
        ("edit a file the script did not declare", Box::new(|| edit("assets/README.md", "changed\n")), None),
        ("set a variable the program reads", Box::new(|| {}), Some(("GREETING", "hi"))),
        ("nothing, variable still set", Box::new(|| {}), Some(("GREETING", "hi"))),
        ("add Cargo configuration", Box::new(|| edit(".cargo/config.toml", "[build]\nrustflags = [\"--cfg\", \"oracle_test\"]\n")), None),
        ("remove it", Box::new(|| {
            pause();
            fs::remove_file(ws.join(".cargo/config.toml")).unwrap();
        }), None),
        ("change the manifest", Box::new(|| edit("app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ndata = { path = \"../data\" }\nassets = { path = \"../assets\" }\n\n[features]\nextra = []\n")), None),
        ("delete the program", Box::new(|| {
            fs::remove_file(sb.target_dir(&ws).join("debug/app")).unwrap();
        }), None),
        ("nothing", Box::new(|| {}), None),
    ];
    ok(sb.cargo(&ws).arg("build"));
    let mut skipped = Vec::new();
    for (what, change, var) in &changes {
        change();
        let mut qrust = sb.cargo_in_terminal(&ws, "build").unwrap();
        let mut cargo = sb.cargo(&ws);
        cargo.args(["build", "-v"]);
        if let Some((k, v)) = var {
            qrust.env(k, v);
            cargo.env(k, v);
        }
        let screen = String::from_utf8_lossy(&run(&mut qrust).stdout).into_owned();
        assert!(screen.contains("OK   Built") || screen.contains("OK   Up to date"), "{}: {}", what, screen);
        if screen.contains("Up to date") {
            skipped.push(*what);
            // Asked through a pipe, where QuickRust never judges, Cargo must find nothing to do.
            let verdict = ok(&mut cargo);
            let said = stderr(&verdict);
            let work: Vec<&str> = said.lines().map(str::trim_start).filter(|l| l.starts_with("Compiling") || l.starts_with("Running")).collect();
            assert!(work.is_empty(), "QuickRust said up to date after `{}`, but Cargo had work: {:?}", what, work);
        }
    }
    // And it does answer when nothing changed: the point of judging at all.
    assert!(skipped.iter().filter(|w| w.starts_with("nothing")).count() >= 3, "{:?}", skipped);
}
