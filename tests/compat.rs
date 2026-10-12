//! Compatibility corpus (QA-001 … QA-030): the `cargo` shim against upstream
//! Cargo as the oracle. Each test runs real builds in an isolated sandbox.

#![cfg(unix)]

mod common;

use common::*;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Artifact names with Cargo's own hashes: equal sets mean Cargo selected and built the same units.
fn artifacts(target: &Path) -> Vec<String> {
    tree(target).into_iter().filter(|f| f.contains("/deps/") && (f.ends_with(".rlib") || f.ends_with(".rmeta") || !f.rsplit('/').next().unwrap().contains('.'))).collect()
}

#[test]
fn qa001_plain_build_matches_upstream() {
    let sb = Sandbox::new("plain");
    let wrapped = sb.package("wrapped/p", "p");
    let native = sb.package("native/p", "p");
    let a = ok(sb.cargo(&wrapped).arg("build"));
    let b = ok(sb.upstream(&native).arg("build"));
    assert_eq!(stdout(&a), stdout(&b));
    assert_eq!(fs::read(wrapped.join("Cargo.lock")).unwrap(), fs::read(native.join("Cargo.lock")).unwrap(), "lockfiles must be identical");
    // Hashes differ between the two directories (the path is part of a path package's identity),
    // so compare the shape; identical hashes are asserted in-place by `no_rebuild_when_alternating`.
    let shape = |v: Vec<String>| v.into_iter().map(|f| f.rsplit_once('-').map(|(n, _)| n.to_string()).unwrap_or(f)).collect::<Vec<_>>();
    assert_eq!(shape(artifacts(&wrapped.join("target"))), shape(artifacts(&native.join("target"))));
    assert!(!tree(&wrapped).iter().any(|f| f.contains(".haul")), "the shim must not create files in the repository");
    assert_eq!(stdout(&ok(sb.cargo(&wrapped).args(["run", "-q"]))), "3\n");
}

#[test]
fn no_rebuild_when_alternating_with_upstream() {
    // compat mode: the wrapper must be invisible to Cargo's fingerprints, same target directory,
    // zero recompilation.
    let sb = Sandbox::new("alternate");
    let ws = sb.workspace("ws");
    ok(sb.cargo(&ws).arg("build").env("HAUL_MODE", "compat"));
    let before = artifacts(&ws.join("target"));
    let up = ok(sb.upstream(&ws).arg("build"));
    assert!(!stderr(&up).contains("Compiling"), "upstream rebuilt after a shim build:\n{}", stderr(&up));
    let again = ok(sb.cargo(&ws).arg("build").env("HAUL_MODE", "compat"));
    assert!(!stderr(&again).contains("Compiling"), "shim rebuilt after an upstream build:\n{}", stderr(&again));
    assert_eq!(artifacts(&ws.join("target")), before);
}

#[test]
fn fast_mode_and_upstream_keep_separate_artifacts_instead_of_rebuilding_each_other() {
    // Fast mode builds with other profile settings, so Cargo names its artifacts differently. After
    // each side has built once, alternating between them compiles nothing (docs/migration.md).
    let sb = Sandbox::new("alternate-fast");
    let ws = sb.workspace("ws");
    ok(sb.cargo(&ws).arg("build"));
    ok(sb.upstream(&ws).arg("build"));
    for _ in 0..2 {
        let shim = ok(sb.cargo(&ws).arg("build"));
        assert!(!stderr(&shim).contains("Compiling"), "fast mode rebuilt after an upstream build:\n{}", stderr(&shim));
        let up = ok(sb.upstream(&ws).arg("build"));
        assert!(!stderr(&up).contains("Compiling"), "upstream rebuilt after a fast-mode build:\n{}", stderr(&up));
    }
    assert_eq!(stdout(&ok(sb.cargo(&ws).args(["run", "-q", "-p", "app"]))), stdout(&ok(sb.upstream(&ws).args(["run", "-q", "-p", "app"]))));
}

#[test]
fn qa002_virtual_workspace_members_are_all_discovered() {
    let sb = Sandbox::new("virtual");
    let ws = sb.workspace("ws");
    ok(sb.cargo(&ws).args(["build", "--workspace"]));
    let inspect: Value = serde_json::from_slice(&ok(sb.haul(&ws).args(["inspect", "--format", "json"])).stdout).unwrap();
    assert_eq!(inspect["workspace"]["members"], 4);
    assert_eq!(inspect["workspace"]["virtual"], true);
    let ids = inspect["workspace"]["member_ids"].to_string();
    for m in ["util", "gen", "mac", "app"] {
        assert!(ids.contains(&format!("/{}#", m)), "member {} missing from {}", m, ids);
    }
    assert!(inspect["workspace"]["lockfile_digest"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(inspect["workspace"]["root"], "$WORKSPACE", "exported paths are tokenized");
}

/// Links the toolchain `p` uses under another name, so that tests can tell selections apart
/// without installing a second release.
fn link_alias(sb: &Sandbox, p: &std::path::Path, name: &str) -> std::path::PathBuf {
    let dir = sb.toolchain_dir(p);
    ok(sb.haul(p).args(["toolchain", "link", name]).arg(&dir));
    dir
}

fn toolchain_of_last_run(sb: &Sandbox) -> String {
    sb.sessions().last().unwrap()["upstream"]["toolchain"].as_str().unwrap().to_string()
}

#[test]
fn qa003_toolchain_selection_plus_then_environment_then_file_then_default() {
    let sb = Sandbox::new("toolchain");
    let p = sb.package("p", "p");
    let stable = sb.toolchain_dir(&p);
    let stable_id = stable.file_name().unwrap().to_string_lossy().into_owned();
    link_alias(&sb, &p, "alt");
    ok(sb.cargo(&p).arg("check"));
    assert_eq!(toolchain_of_last_run(&sb), stable_id, "the default toolchain");
    sb.file("p/rust-toolchain.toml", "[toolchain]\nchannel = \"alt\"\n");
    ok(sb.cargo(&p).arg("check"));
    assert_eq!(toolchain_of_last_run(&sb), "alt", "the toolchain file outranks the default");
    ok(sb.cargo(&p).args(["+stable", "check"]));
    assert_eq!(toolchain_of_last_run(&sb), stable_id, "`+toolchain` outranks the file");
    ok(sb.cargo(&p).arg("check").env("RUSTUP_TOOLCHAIN", "stable"));
    assert_eq!(toolchain_of_last_run(&sb), stable_id, "RUSTUP_TOOLCHAIN outranks the file");
    ok(sb.cargo(&p).arg("check").env("RUSTUP_TOOLCHAIN", &stable));
    assert_eq!(toolchain_of_last_run(&sb), stable.display().to_string(), "RUSTUP_TOOLCHAIN may name a toolchain directory, as editors do");
    // `+toolchain` never reaches Cargo, and the compiler is the toolchain's.
    let report = sb.report(&p, "latest");
    let expected = stdout(&ok(std::process::Command::new(stable.join("bin/rustc")).arg("-V")));
    assert_eq!(report["toolchain"]["rustc_version"].as_str().unwrap(), expected.trim());
    // A toolchain that does not exist fails before anything runs.
    let a = run(sb.cargo(&p).args(["+no-such-toolchain-xyz", "check"]));
    assert_eq!(a.status.code(), Some(2));
    assert!(stderr(&a).contains("no toolchain is named `no-such-toolchain-xyz`"), "{}", stderr(&a));
    let b = run(sb.cargo(&p).args(["+1.98.1.4", "check"]));
    assert_eq!(b.status.code(), Some(2));
    assert!(stderr(&b).contains("is not a toolchain"), "{}", stderr(&b));
}

#[test]
fn builds_and_their_build_scripts_get_hauls_toolchain_and_cargo_home() {
    let sb = Sandbox::new("engine-env");
    let p = sb.package("p", "p");
    sb.file("p/build.rs", "fn main() {\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    let get = |k: &str| std::env::var(k).unwrap_or_default();\n    let nested = std::process::Command::new(get(\"CARGO\")).arg(\"-V\").output().unwrap();\n    std::fs::write(format!(\"{out}/env.txt\"), format!(\"{}\\n{}\\n{}\\n{}\", get(\"RUSTUP_TOOLCHAIN\"), get(\"CARGO_HOME\"), get(\"RUSTC\"), String::from_utf8_lossy(&nested.stdout).trim())).unwrap();\n}\n");
    let stable = sb.toolchain_dir(&p);
    ok(sb.cargo(&p).arg("build").env("CARGO_HOME", sb.dir.join("theirs")));
    let env_file = fs::read_dir(p.join("target/debug/build")).unwrap().flatten().map(|e| e.path().join("out/env.txt")).find(|f| f.is_file()).expect("the build script ran");
    let lines: Vec<String> = fs::read_to_string(env_file).unwrap().lines().map(str::to_string).collect();
    assert_eq!(lines[0], stable.file_name().unwrap().to_string_lossy(), "nested toolchain programs are pinned to the build's toolchain");
    assert_eq!(lines[1], sb.cargo_home().display().to_string(), "Haul's Cargo home, whatever the environment says");
    assert_eq!(lines[2], stable.join("bin/rustc").display().to_string());
    assert_eq!(lines[3], stdout(&ok(std::process::Command::new(stable.join("bin/cargo")).arg("-V"))).trim());
    assert!(!sb.dir.join("theirs").exists());
}

#[test]
fn toolchain_programs_run_through_hauls_proxies() {
    let sb = Sandbox::new("proxies");
    let p = sb.package("p", "p");
    let stable = sb.toolchain_dir(&p);
    link_alias(&sb, &p, "alt");
    let bin = sb.dir.join("proxies");
    fs::create_dir_all(&bin).unwrap();
    for tool in ["rustc", "rustfmt"] {
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_haul"), bin.join(tool)).unwrap();
    }
    let own = stdout(&ok(std::process::Command::new(stable.join("bin/rustc")).arg("-vV")));
    assert_eq!(stdout(&ok(sb.with_shim(&bin.join("rustc").display().to_string(), &p).arg("-vV"))), own);
    assert_eq!(stdout(&ok(sb.with_shim(&bin.join("rustc").display().to_string(), &p).args(["+alt", "-vV"]))), own);
    sb.file("p/src/lib.rs", "pub fn add(x:u32,y:u32)->u32{x+y}\n");
    ok(sb.with_shim(&bin.join("rustfmt").display().to_string(), &p).arg("src/lib.rs"));
    assert_eq!(fs::read_to_string(p.join("src/lib.rs")).unwrap(), "pub fn add(x: u32, y: u32) -> u32 {\n    x + y\n}\n", "the toolchain's rustfmt ran");
    let missing = run(sb.with_shim(&bin.join("rustc").display().to_string(), &p).args(["+no-such-toolchain-xyz", "-V"]));
    assert_eq!(missing.status.code(), Some(2));
}

#[test]
fn qa004_manifest_path_from_another_directory() {
    let sb = Sandbox::new("manifest");
    let p = sb.package("deep/p", "p");
    let manifest = p.join("Cargo.toml");
    ok(sb.cargo(&sb.dir).args(["build", "--manifest-path"]).arg(&manifest));
    assert!(p.join("target/debug/p").exists());
    let sessions = sb.sessions();
    assert_eq!(sessions.last().unwrap()["workspace_root"].as_str().unwrap(), p.to_str().unwrap(), "the workspace is found through --manifest-path, not the cwd");
}

#[test]
fn qa005_features_reach_rustc_unchanged() {
    // FR-001 acceptance: `cargo build --features a,b` selects the same build as upstream (compat
    // mode builds exactly upstream's; fast mode would differ in profile settings by design).
    let sb = Sandbox::new("features");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).args(["build", "--features", "a,b"]).env("HAUL_MODE", "compat"));
    let report = sb.report(&p, "latest");
    let lib = report["units"].as_array().unwrap().iter().find(|u| u["kind"] == "lib").unwrap();
    assert_eq!(lib["features"], serde_json::json!(["a", "b"]));
    let before = artifacts(&p.join("target"));
    let up = ok(sb.upstream(&p).args(["build", "--features", "a,b"]));
    assert!(!stderr(&up).contains("Compiling"), "upstream considers the shim's build stale:\n{}", stderr(&up));
    assert_eq!(artifacts(&p.join("target")), before);
    ok(sb.cargo(&p).args(["build", "--all-features"]));
    ok(sb.cargo(&p).args(["build", "--no-default-features"]));
}

#[test]
fn qa006_cross_target_check() {
    let sb = Sandbox::new("cross");
    let p = sb.package("p", "p");
    let target = "wasm32-unknown-unknown";
    // The target's standard library is installed on first use.
    ok(sb.cargo(&p).args(["check", "--lib", "--target", target]));
    assert!(sb.toolchain_dir(&p).join("lib/rustlib").join(target).join("lib").is_dir());
    assert!(p.join("target").join(target).join("debug").is_dir());
    let report = sb.report(&p, "latest");
    let lib = report["units"].as_array().unwrap().iter().find(|u| u["kind"] == "lib").unwrap();
    assert_eq!((lib["target"].as_str(), lib["mode"].as_str(), lib["role"].as_str()), (Some(target), Some("check"), Some("target")));
    assert_eq!(report["run"]["command"]["targets"], serde_json::json!([target]));
}

#[test]
fn qa007_qa008_qa009_build_scripts_links_and_proc_macros() {
    let sb = Sandbox::new("scripts");
    let ws = sb.workspace("ws");
    // A native `links` build script that passes metadata to its dependents.
    sb.file("ws/gen/Cargo.toml", "[package]\nname = \"gen\"\nversion = \"0.1.0\"\nedition = \"2021\"\nbuild = \"build.rs\"\nlinks = \"gennative\"\n");
    sb.file(
        "ws/gen/build.rs",
        "fn main() {\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    std::fs::write(format!(\"{out}/gen.rs\"), \"pub const GENERATED: u32 = 7;\").unwrap();\n    println!(\"cargo::rerun-if-changed=build.rs\");\n    println!(\"cargo::metadata=root={out}\");\n}\n",
    );
    let out = ok(sb.cargo(&ws).args(["run", "-q", "-p", "app"]));
    assert_eq!(stdout(&out), "56\n");
    let report = sb.report(&ws, "latest");
    let units = report["units"].as_array().unwrap();
    let kind = |k: &str| units.iter().filter(|u| u["kind"] == k).count();
    assert_eq!((kind("build-script"), kind("build-script-run"), kind("proc-macro"), kind("bin")), (1, 1, 1, 1));
    let script = &report["build_scripts"][0];
    assert_eq!(script["package"], "gen");
    assert_eq!(script["ran_in_session"], true);
    assert_eq!(script["has_rerun_directives"], true);
    assert!(script["duration_ns"].as_u64().unwrap() > 0, "the script's run time is measured: {}", script);
    assert!(script["generated_output_bytes"].as_u64().unwrap() > 0);
    let mac = &report["proc_macros"]["units"][0];
    assert_eq!((mac["package"].as_str(), mac["direct_dependents"].as_u64()), (Some("mac"), Some(1)));
    assert!(mac["execution_time_ns"].is_null());
    // The build script ran before the library that includes its output: edge present and inferred.
    let edges = report["edges"].as_array().unwrap();
    assert!(edges.iter().any(|e| e["from"].as_str().unwrap().starts_with("unit:run:gen-") && e["to"].as_str().unwrap().starts_with("unit:gen-") && e["confidence"] == "inferred"));
    assert!(edges.iter().any(|e| e["from"].as_str().unwrap().starts_with("unit:mac-") && e["to"].as_str().unwrap().starts_with("unit:app-") && e["confidence"] == "observed"));
    // With a rerun directive an unrelated edit must not rerun the script, wrapped or not.
    fs::write(ws.join("gen/src/lib.rs"), "include!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n// edit\n").unwrap();
    ok(sb.cargo(&ws).arg("build"));
    assert_eq!(sb.report(&ws, "latest")["build_scripts"][0]["ran_in_session"], false);
}

fn logging_wrapper(sb: &Sandbox, name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let log = sb.dir.join(format!("{}.log", name));
    let script = sb.file(&format!("bin/{}", name), &format!("#!/bin/sh\necho \"$1\" >> '{}'\nexec \"$@\"\n", log.display()));
    make_executable(&script);
    (script, log)
}

#[test]
fn qa010_qa011_qa012_existing_wrappers_are_preserved_in_cargo_order() {
    let sb = Sandbox::new("wrappers");
    let p = sb.package("p", "p");
    let (outer, outer_log) = logging_wrapper(&sb, "outer-wrapper");
    let (ws_wrapper, ws_log) = logging_wrapper(&sb, "workspace-wrapper");

    // QA-010: RUSTC_WRAPPER alone.
    ok(sb.cargo(&p).arg("build").env("RUSTC_WRAPPER", &outer));
    let lines = fs::read_to_string(&outer_log).unwrap();
    assert!(lines.lines().count() >= 2, "the user's wrapper ran for each rustc invocation");
    assert!(lines.lines().all(|l| l.ends_with("rustc")), "without a workspace wrapper the user's wrapper receives rustc: {}", lines);
    let session = sb.sessions().pop().unwrap();
    assert_eq!(session["wrappers"]["inner"].as_str().unwrap(), outer.to_str().unwrap());
    assert_eq!(session["wrappers"]["inner_source"], "env RUSTC_WRAPPER");
    assert!(sb.report(&p, "latest")["metrics"]["units_compiled"].as_u64().unwrap() >= 2, "Haul still observed the units");

    // QA-011: RUSTC_WORKSPACE_WRAPPER alone.
    fs::remove_dir_all(p.join("target")).unwrap();
    ok(sb.cargo(&p).arg("build").env("RUSTC_WORKSPACE_WRAPPER", &ws_wrapper));
    assert!(fs::read_to_string(&ws_log).unwrap().lines().count() >= 2);

    // QA-012: both. Cargo's documented order is $RUSTC_WRAPPER $RUSTC_WORKSPACE_WRAPPER $RUSTC.
    fs::remove_dir_all(p.join("target")).unwrap();
    fs::remove_file(&outer_log).unwrap();
    fs::remove_file(&ws_log).unwrap();
    ok(sb.cargo(&p).arg("build").env("RUSTC_WRAPPER", &outer).env("RUSTC_WORKSPACE_WRAPPER", &ws_wrapper));
    let outer_seen = fs::read_to_string(&outer_log).unwrap();
    assert!(outer_seen.lines().any(|l| l == ws_wrapper.to_str().unwrap()), "the outer wrapper must receive the workspace wrapper as its first argument:\n{}", outer_seen);
    assert!(fs::read_to_string(&ws_log).unwrap().lines().all(|l| l.ends_with("rustc")));
}

#[test]
fn wrapper_configured_in_cargo_config_is_not_masked() {
    // FR-100 acceptance: installing instrumentation must not drop an existing wrapper,
    // including one that only exists in `.cargo/config.toml` (which RUSTC_WRAPPER would override).
    let sb = Sandbox::new("cfgwrapper");
    let p = sb.package("p", "p");
    let (wrapper, log) = logging_wrapper(&sb, "configured-wrapper");
    sb.file("p/.cargo/config.toml", &format!("[build]\nrustc-wrapper = \"{}\"\n", wrapper.display()));
    ok(sb.cargo(&p).arg("build"));
    assert!(fs::read_to_string(&log).unwrap().lines().count() >= 2, "the configured wrapper was bypassed");
    assert!(sb.sessions().pop().unwrap()["wrappers"]["inner_source"].as_str().unwrap().starts_with("cargo config"));
}

#[test]
fn a_broken_user_wrapper_fails_like_upstream() {
    let sb = Sandbox::new("brokenwrapper");
    let p = sb.package("p", "p");
    let a = run(sb.cargo(&p).arg("build").env("RUSTC_WRAPPER", "/nonexistent/wrapper"));
    let b = run(sb.upstream(&p).arg("build").env("RUSTC_WRAPPER", "/nonexistent/wrapper"));
    assert!(!b.status.success());
    assert_eq!(a.status.code(), b.status.code(), "a wrapper the user configured must not be silently skipped\n{}", stderr(&a));
}

#[test]
fn qa014_check_test_run_doc_bench_behave_like_upstream() {
    let sb = Sandbox::new("commands");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).arg("check"));
    let t = ok(sb.cargo(&p).arg("test"));
    assert!(stdout(&t).contains("test tests::adds ... ok"), "{}", stdout(&t));
    ok(sb.cargo(&p).args(["doc", "--no-deps"]));
    assert!(p.join("target/doc/p/index.html").exists());
    ok(sb.cargo(&p).args(["bench", "--no-run"]));
    let families: Vec<String> = sb.sessions().iter().map(|s| s["command"]["family"].as_str().unwrap().to_string()).collect();
    assert_eq!(families, ["check", "test", "doc", "bench"]);
    // A failing test keeps Cargo's exit status and output.
    sb.file("p/src/lib.rs", "#[test]\nfn fails() {\n    assert_eq!(1, 2);\n}\n");
    sb.file("p/src/main.rs", "fn main() {}\n");
    let a = run(sb.cargo(&p).arg("test"));
    let b = run(sb.upstream(&p).arg("test"));
    assert_eq!(a.status.code(), Some(101));
    assert_eq!(a.status.code(), b.status.code());
    assert_eq!(sb.sessions().pop().unwrap()["state"], "failed");
}

#[test]
fn stdin_and_exit_codes_pass_through_cargo_run() {
    let sb = Sandbox::new("stdio");
    let p = sb.package("p", "p");
    sb.file("p/src/main.rs", "use std::io::Read;\nfn main() {\n    let mut s = String::new();\n    std::io::stdin().read_to_string(&mut s).unwrap();\n    print!(\"{}\", s.to_uppercase());\n    eprint!(\"err:{}\", s.len());\n    std::process::exit(7);\n}\n");
    let mut child = sb.cargo(&p).args(["run", "-q"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"piped input\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(stdout(&out), "PIPED INPUT\n");
    assert!(stderr(&out).ends_with("err:12"), "{}", stderr(&out));
    assert_eq!(out.status.code(), Some(7), "the program's exit status is propagated exactly");
}

#[test]
fn qa015_cargo_rustc_extra_arguments() {
    let sb = Sandbox::new("rustc");
    let p = sb.package("p", "p");
    let a = ok(sb.cargo(&p).args(["rustc", "--lib", "--", "--print", "crate-name"]));
    let b = ok(sb.upstream(&p).args(["rustc", "--lib", "--", "--print", "crate-name"]).env("CARGO_TARGET_DIR", sb.dir.join("oracle")));
    assert_eq!(stdout(&a), stdout(&b));
    assert_eq!(stdout(&a).trim(), "p");
}

#[test]
fn qa016_cargo_clean_keeps_exact_upstream_semantics() {
    let sb = Sandbox::new("clean");
    let ws = sb.workspace("ws");
    ok(sb.cargo(&ws).arg("build"));
    let before = tree(&ws.join("target"));
    // Smart lifecycle management never deletes through any path in this release.
    ok(sb.haul(&ws).args(["gc", "--dry-run"]));
    assert_eq!(tree(&ws.join("target")), before, "gc --dry-run must not remove anything");
    // Selective clean removes the package's artifacts and nothing else.
    let a = ok(sb.cargo(&ws).args(["clean", "-p", "util"]));
    assert!(stderr(&a).contains("Removed"), "{}", stderr(&a));
    let after = tree(&ws.join("target"));
    assert!(!after.iter().any(|f| f.contains("/deps/libutil-")));
    assert!(after.iter().any(|f| f.contains("/deps/libgen-")));
    ok(sb.cargo(&ws).arg("clean"));
    assert!(!ws.join("target").exists());
    assert_eq!(sb.sessions().pop().unwrap()["command"]["family"], "clean");
    // After a clean, an identical rebuild is classified as a cache miss, not as an unknown rebuild.
    ok(sb.cargo(&ws).arg("build"));
    let report = sb.report(&ws, "latest");
    let reasons = &report["rebuilds"]["summary"]["by_reason"];
    assert!(reasons["cache-miss"]["units"].as_u64().unwrap() >= 4, "{}", reasons);
    assert!(reasons.get("unknown").is_none(), "{}", reasons);
}

#[test]
fn qa017_metadata_stdout_is_byte_identical() {
    let sb = Sandbox::new("metadata");
    let ws = sb.workspace("ws");
    let a = ok(sb.cargo(&ws).args(["metadata", "--format-version", "1"]));
    let b = ok(sb.upstream(&ws).args(["metadata", "--format-version", "1"]));
    assert_eq!(a.stdout, b.stdout);
    assert!(sb.sessions().is_empty(), "read-only queries are delegated without leaving a session behind");
}

#[test]
fn qa018_json_message_stream_is_byte_identical_and_observed() {
    // AC-002 / FR-001.2: the machine-readable stream must be exactly Cargo's. In compat mode the
    // build is upstream's own, so the stream is byte-identical; in fast mode it is still exactly
    // what the delegated Cargo prints, describing the settings Haul applied.
    let sb = Sandbox::new("json");
    let ws = sb.workspace("ws");
    let args = ["build", "-j1", "--message-format=json"];
    let wrapped = ok(sb.cargo(&ws).args(args).env("HAUL_MODE", "compat"));
    ok(sb.upstream(&ws).arg("clean"));
    let native = ok(sb.upstream(&ws).args(args));
    // Upstream Cargo itself emits independent units in a different order from run to run (even
    // with -j1), so the oracle is the multiset of lines: every message byte-identical, none
    // added, none missing, and the stream ending with Cargo's own `build-finished`.
    let sorted = |o: &std::process::Output| {
        let mut lines: Vec<String> = stdout(o).lines().map(str::to_string).collect();
        lines.sort();
        lines
    };
    assert_eq!(sorted(&wrapped), sorted(&native), "JSON messages differ from upstream");
    assert_eq!(wrapped.stdout.len(), native.stdout.len(), "framing (line terminators) must match");
    assert_eq!(stdout(&wrapped).lines().last(), stdout(&native).lines().last());
    for line in stdout(&wrapped).lines() {
        let v: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("non-JSON line on stdout: {:?} ({})", line, e));
        assert!(v["reason"].is_string(), "foreign object on Cargo's stdout: {}", line);
    }
    // The stream was also ingested: fresh units are known for this run, and none were fresh.
    let report = sb.report(&ws, "latest");
    assert_eq!(report["metrics"]["units_fresh"], 0);
    assert!(report["metrics"]["units_observed"].as_u64().unwrap() >= 5);
    // A second, fully fresh build reports its units as fresh rather than as zero work unknown.
    let again = ok(sb.cargo(&ws).args(args).env("HAUL_MODE", "compat"));
    let fresh = sb.report(&ws, "latest");
    assert!(fresh["metrics"]["units_fresh"].as_u64().unwrap() >= 5, "{}", fresh["metrics"]);
    assert_eq!(fresh["metrics"]["units_compiled"], 0);
    assert!(stdout(&again).lines().all(|l| serde_json::from_str::<Value>(l).is_ok()));
}

#[test]
fn qa019_external_subcommands_and_nested_cargo() {
    let sb = Sandbox::new("external");
    let p = sb.package("p", "p");
    // The subcommand calls `cargo` the way most tools do: by name, through PATH.
    let sub = sb.file("bin/cargo-hello", "#!/bin/sh\necho \"hello-args:$*\"\ncargo build --quiet || exit 9\nexit 5\n");
    make_executable(&sub);
    let mut path = vec![sb.dir.join("bin")];
    path.extend(std::env::split_paths(&sb.path_with_shim()));
    let path = std::env::join_paths(path).unwrap();
    let out = run(sb.cargo(&p).args(["hello", "--flag", "value"]).env("PATH", &path));
    assert_eq!(out.status.code(), Some(5), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("hello-args:hello --flag value"), "Cargo resolved the external subcommand itself: {}", text);
    let sessions = sb.sessions();
    assert_eq!(sessions.len(), 2);
    let (outer, nested) = (&sessions[0], &sessions[1]);
    assert_eq!(outer["command"]["family"], "external");
    assert_eq!(nested["command"]["family"], "build");
    assert_eq!(nested["parent_session_id"], outer["id"], "COMP-031: the nested invocation is linked to its parent");
    assert_eq!(nested["instrumented"], true);

    // COMP-032: projects can opt nested invocations out; they then go straight to upstream.
    fs::remove_dir_all(p.join("target")).unwrap();
    let opt_out = run(sb.cargo(&p).args(["hello"]).env("PATH", &path).env("HAUL_NESTED", "upstream"));
    assert_eq!(opt_out.status.code(), Some(5));
    assert!(p.join("target/debug/p").exists());
    assert_eq!(sb.sessions().len(), 3, "only the outer invocation is recorded when nested ones are opted out");
}

#[test]
fn qa020_aliases_are_resolved_by_cargo_and_classified_for_observation() {
    let sb = Sandbox::new("alias");
    let p = sb.package("p", "p");
    sb.file("p/.cargo/config.toml", "[alias]\nxb = \"build --features a\"\n");
    ok(sb.cargo(&p).arg("xb"));
    assert!(p.join("target/debug/p").exists());
    let s = sb.sessions().pop().unwrap();
    assert_eq!((s["command"]["family"].as_str(), s["command"]["alias_of"].as_str(), s["instrumented"].as_bool()), (Some("build"), Some("build"), Some(true)));
    assert_eq!(sb.report(&p, "latest")["units"].as_array().unwrap().iter().find(|u| u["kind"] == "lib").unwrap()["features"], serde_json::json!(["a"]));
}

#[test]
fn qa021_package_and_login_pass_through_and_secrets_never_reach_haul_state() {
    let sb = Sandbox::new("registry");
    let p = sb.package("p", "p");
    let a = ok(sb.cargo(&p).args(["package", "--allow-dirty", "--no-verify", "--offline"]));
    assert!(p.join("target/package/p-0.1.0.crate").exists(), "{}", stderr(&a));
    // `cargo login` against a mock registry: the token lands in Haul's Cargo home.
    let cargo_home = sb.cargo_home();
    let index = sb.dir.join("mock-index");
    sb.file("mock-index/config.json", "{\"dl\":\"file:///nonexistent/dl\",\"api\":\"http://127.0.0.1:9\"}\n");
    git(&index, &["init", "-q"]);
    git(&index, &["add", "."]);
    git(&index, &["commit", "-q", "-m", "index"]);
    sb.file("haul-home/cargo/config.toml", &format!("[registries.mock]\nindex = \"file://{}\"\n", index.display()));
    let secret = "s3cr3t-token-0123456789abcdef";
    let login = |c: &mut std::process::Command| {
        c.args(["login", "--registry", "mock", secret]);
    };
    let mut c = sb.cargo(&p);
    login(&mut c);
    let out = run(&mut c);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(fs::read_to_string(cargo_home.join("credentials.toml")).unwrap().contains(secret), "Cargo stored the token: the command was delegated intact");
    // AC-019: the credential must not appear anywhere in Haul's records (Cargo's home aside).
    for f in tree(&sb.home).into_iter().filter(|f| !f.starts_with("cargo/")) {
        let content = fs::read(sb.home.join(&f)).unwrap();
        assert!(!String::from_utf8_lossy(&content).contains(secret), "token leaked into {}", f);
    }
    let s = sb.sessions().pop().unwrap();
    assert_eq!(s["command"]["family"], "passthrough");
    assert_eq!(s["instrumented"], false);
    assert!(s["command"]["argv_redacted"].to_string().contains("<redacted>"));
}

#[test]
fn qa022_signals_are_forwarded_and_the_exit_mirrors_the_signal() {
    let sb = Sandbox::new("signal");
    let p = sb.package("p", "p");
    let marker = sb.dir.join("script-started");
    sb.file("p/build.rs", &format!("fn main() {{\n    std::fs::write({:?}, \"x\").unwrap();\n    std::thread::sleep(std::time::Duration::from_secs(20));\n}}\n", marker));
    let started = std::time::Instant::now();
    let mut child = sb.cargo(&p).arg("build").stderr(Stdio::null()).spawn().unwrap();
    while !marker.exists() {
        assert!(started.elapsed().as_secs() < 60, "the build script never started");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // Signal only the shim: it must pass the signal on to Cargo.
    assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) }, 0);
    let status = child.wait().unwrap();
    assert_eq!(status.signal(), Some(2), "the shim must terminate by the same signal as Cargo did: {:?}", status);
    assert!(started.elapsed().as_secs() < 15, "the interruption was not forwarded promptly");
    let s = sb.sessions().pop().unwrap();
    assert_eq!((s["state"].as_str(), s["result"]["signal"].as_i64()), (Some("cancelled"), Some(2)));
}

#[test]
fn qa023_non_utf8_arguments_are_forwarded_byte_for_byte() {
    use std::os::unix::ffi::OsStrExt;
    let sb = Sandbox::new("bytes");
    let p = sb.package("p", "p");
    sb.file("p/src/main.rs", "use std::os::unix::ffi::OsStrExt;\nfn main() {\n    for a in std::env::args_os().skip(1) {\n        println!(\"{:?}\", a.as_bytes());\n    }\n}\n");
    let raw = std::ffi::OsStr::from_bytes(b"caf\xe9-\xff\xfe");
    let a = ok(sb.cargo(&p).args(["run", "-q", "--"]).arg(raw).arg("plain"));
    assert_eq!(stdout(&a), "[99, 97, 102, 233, 45, 255, 254]\n[112, 108, 97, 105, 110]\n");
    let s = sb.sessions().pop().unwrap();
    assert!(s["notes"].to_string().contains("non-UTF-8"), "the lossy rendering in the record is called out: {}", s["notes"]);
}

#[test]
fn qa025_offline_and_locked_flags_pass_through() {
    let sb = Sandbox::new("offline");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).args(["build", "--offline"]));
    ok(sb.cargo(&p).args(["build", "--locked", "--offline"]));
    // --locked with a missing lockfile fails identically, and Haul must not have created one.
    fs::remove_file(p.join("Cargo.lock")).unwrap();
    let a = run(sb.cargo(&p).args(["build", "--locked"]));
    assert!(!p.join("Cargo.lock").exists(), "discovery must never write the lockfile");
    let b = run(sb.upstream(&p).args(["build", "--locked"]));
    assert_eq!(a.status.code(), b.status.code());
    assert_eq!(stderr(&a), stderr(&b));
}

#[test]
fn qa026_cache_outage_fails_open_or_closed_by_policy() {
    let sb = Sandbox::new("outage");
    let p = sb.package("p", "p");
    // sccache "installed" but unable to serve: its statistics/preflight call fails.
    let fake = sb.file("fakebin/sccache", "#!/bin/sh\necho 'sccache: error: Connection refused' >&2\nexit 2\n");
    make_executable(&fake);
    let mut path = vec![sb.dir.join("fakebin")];
    path.extend(std::env::split_paths(&sb.path_with_shim()));
    let path = std::env::join_paths(path).unwrap();

    let open = run(sb.cargo(&p).arg("build").env("PATH", &path).env("HAUL_CACHE_PROVIDER", "sccache"));
    assert!(open.status.success(), "fail-open must compile locally: {}", stderr(&open));
    assert!(stderr(&open).contains("haul: warning") && stderr(&open).contains("compiling without it"), "{}", stderr(&open));
    assert!(p.join("target/debug/p").exists());
    let s = sb.sessions().pop().unwrap();
    assert_eq!(s["policy"]["cache"]["state"], "degraded");
    assert!(s["wrappers"]["inner"].is_null(), "the unavailable cache is not in the wrapper chain");
    // AC-018: nothing in the report claims a cache hit.
    let report = sb.report(&p, "latest");
    assert!(report["cache"]["hits"].is_null());
    assert!(report["units"].as_array().unwrap().iter().all(|u| u["cacheability"] == "n/a"));

    fs::remove_dir_all(p.join("target")).unwrap();
    let closed = run(sb.cargo(&p).arg("build").env("PATH", &path).env("HAUL_CACHE_PROVIDER", "sccache").env("HAUL_CACHE_FAIL", "closed"));
    assert_eq!(closed.status.code(), Some(2), "{}", stderr(&closed));
    assert!(stderr(&closed).contains("cache.fail = closed"));
    assert!(!p.join("target").exists(), "fail-closed stops before any compilation");
}

#[test]
fn qa027_unwritable_report_or_state_never_fails_a_successful_build() {
    let sb = Sandbox::new("diskfull");
    let p = sb.package("p", "p");
    let blocker = sb.file("blocker", "a file where a directory is needed");
    // API-004: report generation failure leaves the Cargo result alone …
    let out = run(sb.cargo(&p).arg("build").env("HAUL_REPORT", blocker.join("report.json")));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("haul: warning: could not write report"), "{}", stderr(&out));
    // … unless strict reporting was requested.
    let strict = run(sb.cargo(&p).arg("build").env("HAUL_REPORT", blocker.join("report.json")).env("HAUL_MEASUREMENT_STRICT", "true"));
    assert_eq!(strict.status.code(), Some(2));
    // NFR-020: Haul's records cannot be written: the build still runs, with a warning. Here the
    // records' directory is a regular file (a read-only directory would not stop root); the
    // toolchains Haul installed are still there to build with.
    let unusable = sb.dir.join("records-blocked");
    fs::create_dir_all(&unusable).unwrap();
    std::os::unix::fs::symlink(toolchain_store(), unusable.join("toolchains")).unwrap();
    fs::write(unusable.join("workspaces"), "").unwrap();
    fs::remove_dir_all(p.join("target")).unwrap();
    let no_state = run(sb.cargo(&p).arg("build").env("HAUL_HOME", &unusable));
    assert!(no_state.status.success(), "{}", stderr(&no_state));
    assert!(stderr(&no_state).contains("this run was not recorded"), "{}", stderr(&no_state));
    assert!(p.join("target/debug/p").exists());
}

#[test]
fn qa030_unknown_future_flags_and_commands_reach_upstream() {
    let sb = Sandbox::new("future");
    let p = sb.package("p", "p");
    for args in [&["build", "--frobnicate-from-the-future"][..], &["--future-global-flag", "build"][..], &["command-from-the-future", "--x"][..]] {
        let a = run(sb.cargo(&p).args(args));
        let b = run(sb.upstream(&p).args(args));
        assert_eq!(a.status.code(), b.status.code(), "{:?}", args);
        assert_eq!(stderr(&a), stderr(&b), "the rejection must be Cargo's own for {:?}", args);
        assert_eq!(a.stdout, b.stdout);
    }
}

#[test]
fn help_version_and_list_are_pure_passthrough() {
    let sb = Sandbox::new("help");
    for args in [&["--version"][..], &["-V"][..], &["--list"][..], &["help", "build"][..], &["build", "--help"][..], &[][..]] {
        let a = run(sb.cargo(&sb.dir).args(args));
        let b = run(sb.upstream(&sb.dir).args(args));
        assert_eq!(a.status.code(), b.status.code(), "{:?}", args);
        assert_eq!(a.stdout, b.stdout, "{:?}", args);
    }
}

#[test]
fn recursion_is_refused_with_a_diagnostic() {
    let sb = Sandbox::new("recursion");
    let p = sb.package("p", "p");
    // A linked "toolchain" whose cargo is Haul: refused when linked if it is the same file.
    let fake = sb.dir.join("fake-toolchain/bin");
    fs::create_dir_all(&fake).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_haul"), fake.join("cargo")).unwrap();
    std::os::unix::fs::symlink(sb.toolchain_dir(&p).join("bin/rustc"), fake.join("rustc")).unwrap();
    let a = run(sb.haul(&p).args(["toolchain", "link", "loop"]).arg(sb.dir.join("fake-toolchain")));
    assert_eq!(a.status.code(), Some(2));
    assert!(stderr(&a).contains("is Haul itself"), "{}", stderr(&a));
    // A byte-for-byte copy has a different inode and path: only the session guard can catch it (COMP-022).
    fs::remove_file(fake.join("cargo")).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_haul"), fake.join("cargo")).unwrap();
    ok(sb.haul(&p).args(["toolchain", "link", "loop"]).arg(sb.dir.join("fake-toolchain")));
    let b = run(sb.cargo(&p).args(["+loop", "build"]));
    assert_eq!(b.status.code(), Some(2), "{}", stderr(&b));
    assert!(stderr(&b).contains("refusing to run Cargo"), "{}", stderr(&b));
    assert!(!p.join("target").exists());
}

#[test]
fn an_unreachable_dist_server_is_a_clear_error() {
    let sb = Sandbox::new("nocargo");
    let p = sb.package("p", "p");
    // A Haul home without the suite's toolchains.
    let empty = sb.dir.join("empty-home");
    let out = run(sb.cargo(&p).arg("build").env("HAUL_HOME", &empty).env("HAUL_DIST_SERVER", "http://127.0.0.1:9"));
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("toolchain stable (from default toolchain): could not download http://127.0.0.1:9/dist/channel-rust-stable.toml"), "{}", stderr(&out));
    assert!(!p.join("target").exists());
}

#[test]
fn explicit_haul_entry_and_upstream_escape_hatch() {
    let sb = Sandbox::new("explicit");
    let p = sb.package("p", "p");
    // Migration stage "explicit wrapper": `haul build` is Cargo syntax through Haul.
    ok(sb.haul(&p).arg("build"));
    assert_eq!(sb.sessions().pop().unwrap()["command"]["entry"], "haul");
    // AC-020 / COMP-013: the escape hatch runs upstream with no session and no wrapper.
    let count = sb.sessions().len();
    fs::remove_dir_all(p.join("target")).unwrap();
    let (wrapper, log) = logging_wrapper(&sb, "probe");
    let out = ok(sb.haul(&p).args(["upstream", "build"]).env("RUSTC_WRAPPER", &wrapper));
    assert!(stderr(&out).contains("Compiling p"));
    assert_eq!(sb.sessions().len(), count, "`haul upstream` records nothing");
    assert!(fs::read_to_string(&log).unwrap().lines().all(|l| l.ends_with("rustc")), "the user's wrapper received rustc directly: no Haul wrapper in the chain");
}

#[test]
fn environment_reaches_build_scripts_unchanged() {
    // Compatibility oracle: build-script-visible relevant environment.
    let sb = Sandbox::new("env");
    let script = "fn main() {\n    let mut vars: Vec<String> = std::env::vars().filter(|(k, _)| !k.starts_with(\"HAUL_\") && k != \"RUSTC_WRAPPER\" && k != \"CARGO\" && k != \"OUT_DIR\" && k != \"CARGO_MANIFEST_DIR\" && k != \"CARGO_MANIFEST_PATH\" && k != \"PATH\" && k != \"DYLD_FALLBACK_LIBRARY_PATH\" && k != \"LD_LIBRARY_PATH\" && k != \"PWD\" && k != \"OLDPWD\" && k != \"DUMP\").map(|(k, v)| format!(\"{k}={v}\")).collect();\n    vars.sort();\n    std::fs::write(std::env::var(\"DUMP\").unwrap(), vars.join(\"\\n\")).unwrap();\n}\n";
    let mut dumps = Vec::new();
    for (name, wrapped) in [("wrapped", true), ("native", false)] {
        let p = sb.package(&format!("{}/p", name), "p");
        sb.file(&format!("{}/p/build.rs", name), script);
        let dump = sb.dir.join(format!("{}.env", name));
        let mut c = if wrapped { sb.cargo(&p) } else { sb.upstream(&p) };
        ok(c.arg("build").env("DUMP", &dump).env("RUSTFLAGS", "--cfg some_flag").env("MY_PROXY_SETTING", "http://proxy.invalid:1"));
        dumps.push(fs::read_to_string(dump).unwrap());
    }
    let (w, n): (Vec<&str>, Vec<&str>) = (dumps[0].lines().collect(), dumps[1].lines().collect());
    let only_wrapped: Vec<&&str> = w.iter().filter(|l| !n.contains(l)).collect();
    let only_native: Vec<&&str> = n.iter().filter(|l| !w.contains(l)).collect();
    assert!(only_wrapped.is_empty() && only_native.is_empty(), "build scripts must see the same environment apart from Haul's own variables\nonly under the shim: {:#?}\nonly under upstream: {:#?}", only_wrapped, only_native);
    assert!(dumps[0].contains("CARGO_ENCODED_RUSTFLAGS=--cfg\u{1f}some_flag"));
    assert!(dumps[0].contains("MY_PROXY_SETTING=http://proxy.invalid:1"));
}

#[test]
fn invalid_haul_configuration_names_the_field_and_its_source() {
    let sb = Sandbox::new("badconfig");
    let p = sb.package("p", "p");
    sb.file("p/.haul/config.toml", "[cache]\nprovidr = \"sccache\"\n");
    let out = run(sb.cargo(&p).arg("build"));
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.contains("cache.providr") && err.contains(".haul/config.toml"), "{}", err);
    assert!(!p.join("target").exists(), "an invalid configuration stops before the build instead of guessing");
    // Configuration errors are reported consistently for every command, queries included.
    assert_eq!(run(sb.cargo(&p).arg("--version")).status.code(), Some(2));
    // The escape hatch does not depend on Haul's configuration.
    ok(sb.haul(&p).args(["upstream", "--version"]));
}

#[test]
fn dispatch_overhead_stays_within_budget() {
    // NFR-PERF-001: passive compatibility dispatch adds ≤ 50 ms (p95 target; median asserted here
    // with the unoptimized test binary).
    let sb = Sandbox::new("overhead");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).arg("build"));
    let median = |mk: &dyn Fn() -> std::process::Command| {
        let mut samples: Vec<u128> = (0..15)
            .map(|_| {
                let t = std::time::Instant::now();
                let out = mk().arg("build").output().unwrap();
                assert!(out.status.success());
                t.elapsed().as_micros()
            })
            .collect();
        samples.sort();
        samples[samples.len() / 2]
    };
    let native = median(&|| sb.upstream(&p));
    let wrapped = median(&|| sb.cargo(&p));
    let overhead_ms = (wrapped as f64 - native as f64) / 1000.0;
    eprintln!("no-op build: upstream {:.1} ms, shim {:.1} ms, overhead {:.1} ms", native as f64 / 1000.0, wrapped as f64 / 1000.0, overhead_ms);
    assert!(overhead_ms < 100.0, "no-op overhead {:.1} ms exceeds the 100 ms budget (NFR-PERF-003)", overhead_ms);
}

#[test]
fn comp007_cargo_has_the_terminal_through_the_shim() {
    // COMP-007: Cargo's standard streams are a terminal under Haul as under upstream Cargo: it
    // detects colour, and a Ctrl-C typed there reaches every process of the build and ends it
    // with the same status.
    let sb = Sandbox::new("terminal");
    let p = sb.package("p", "p");
    let Some(mut ours) = sb.cargo_in_terminal(&p, "build") else {
        eprintln!("skipped: no util-linux `script` for a pseudo-terminal");
        return;
    };
    let compiling = |out: &std::process::Output| -> Vec<String> { String::from_utf8_lossy(&out.stdout).split(['\r', '\n']).filter(|l| l.contains("Compiling")).map(str::to_string).collect() };
    // Cargo's own colour detection (the sandbox otherwise turns colour off).
    let ours = run(ours.env("HAUL_MODE", "compat").env_remove("CARGO_TERM_COLOR"));
    ok(sb.upstream(&p).arg("clean"));
    let theirs = run(sb.upstream_in_terminal(&p, "build").unwrap().env_remove("CARGO_TERM_COLOR"));
    assert!(ours.status.success() && theirs.status.success());
    assert!(!compiling(&theirs).is_empty() && compiling(&theirs)[0].contains("\x1b["), "upstream Cargo colours its status on a terminal: {:?}", compiling(&theirs));
    assert_eq!(compiling(&ours), compiling(&theirs), "the same colour, through the shim");

    // A program that waits; Ctrl-C typed at the terminal.
    sb.file("p/src/main.rs", "fn main() {\n    println!(\"ready\");\n    std::thread::sleep(std::time::Duration::from_secs(60));\n}\n");
    let interrupted = |mut cmd: std::process::Command| -> (Option<i32>, std::time::Duration) {
        use std::io::{BufRead, Write};
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut lines = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        while !line.contains("ready") {
            line.clear();
            assert!(lines.read_line(&mut line).unwrap() > 0, "the program never started");
        }
        let typed = std::time::Instant::now();
        child.stdin.as_mut().unwrap().write_all(b"\x03").unwrap();
        let status = child.wait().unwrap();
        (status.code(), typed.elapsed())
    };
    let (theirs, _) = interrupted(sb.upstream_in_terminal(&p, "run").unwrap());
    // Haul's own way, building first and then running the command (the default in a terminal).
    let (ours, took) = interrupted(sb.cargo_in_terminal(&p, "run").unwrap());
    assert_eq!(ours, theirs, "the same exit status as upstream Cargo after Ctrl-C");
    assert!(took < std::time::Duration::from_secs(10), "Ctrl-C ended the run after {:?}", took);
}

#[test]
fn comp042_the_jobserver_reaches_rustc_through_the_wrapper_and_cargo_from_make() {
    // COMP-042 / FR-120.4: Cargo's jobserver reaches the compiler through Haul's rustc wrapper,
    // and an outer `make -jN` jobserver reaches Cargo through the shim.
    let sb = Sandbox::new("jobserver");
    let p = sb.package("p", "p");
    // A build script that takes a token from the jobserver it is given and gives it back.
    sb.file(
        "p/build.rs",
        r#"use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;

fn main() {
    let flags = std::env::var("CARGO_MAKEFLAGS").unwrap();
    // `--jobserver-auth` since GNU Make 4.2, `--jobserver-fds` before (macOS ships 3.81).
    let auth = flags.split_whitespace().find_map(|f| f.strip_prefix("--jobserver-auth=").or_else(|| f.strip_prefix("--jobserver-fds="))).unwrap().to_string();
    let mut token = [0u8; 1];
    if let Some(path) = auth.strip_prefix("fifo:") {
        let mut f = std::fs::OpenOptions::new().read(true).write(true).open(path).unwrap();
        f.read_exact(&mut token).unwrap();
        f.write_all(&token).unwrap();
    } else {
        let (r, w) = auth.split_once(',').unwrap();
        let mut read = unsafe { std::fs::File::from_raw_fd(r.parse().unwrap()) };
        let mut write = unsafe { std::fs::File::from_raw_fd(w.parse().unwrap()) };
        read.read_exact(&mut token).unwrap();
        write.write_all(&token).unwrap();
        std::mem::forget(read);
        std::mem::forget(write);
    }
    std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("jobserver-auth"), &auth).unwrap();
    println!("cargo::warning=jobserver token acquired and released");
}
"#,
    );
    let refused = "failed to connect to jobserver";

    // Fast mode: every compilation passes through Haul's wrapper.
    let built = ok(sb.cargo(&p).arg("build"));
    assert!(stderr(&built).contains("jobserver token acquired and released"), "{}", stderr(&built));
    assert!(!stderr(&built).contains(refused), "the compiler lost the jobserver: {}", stderr(&built));
    // The check can fail: a wrapper of the user's that closes the jobserver's descriptors, which
    // Haul runs inside its own, makes the compiler say so.
    let closing = sb.file("close-jobserver.sh", "#!/bin/sh\nexec 3>&- 4>&- 5>&-\nexec \"$@\"\n");
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&closing, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(p.join("src/lib.rs"), "pub fn add(x: u32, y: u32) -> u32 { y + x }\n").unwrap();
    let broken = ok(sb.cargo(&p).arg("build").env("RUSTC_WRAPPER", &closing));
    assert!(stderr(&broken).contains(refused), "the negative control did not trip: {}", stderr(&broken));

    // `make -j3` driving Cargo through the shim: Cargo uses make's jobserver.
    if Command::new("make").arg("--version").output().is_err() {
        eprintln!("skipped the make part: no make");
        return;
    }
    sb.file("p/Makefile", "all:\n\t@echo \"$$MAKEFLAGS\" > make-flags.txt\n\t+cargo build\n");
    ok(sb.cargo(&p).arg("clean"));
    // The suite itself may run under a jobserver: make starts its own.
    let out = run(sb.with_shim("make", &p).arg("-j3").env_remove("CARGO_MAKEFLAGS").env_remove("MAKEFLAGS").env_remove("MFLAGS"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let outer = fs::read_to_string(p.join("make-flags.txt")).unwrap();
    let outer_auth = outer.split_whitespace().find_map(|f| f.strip_prefix("--jobserver-auth=").or_else(|| f.strip_prefix("--jobserver-fds="))).unwrap_or_else(|| panic!("make set no jobserver: {}", outer)).to_string();
    let seen = fs::read_dir(p.join("target/debug/build")).unwrap().filter_map(|e| fs::read_to_string(e.ok()?.path().join("out/jobserver-auth")).ok()).next().expect("the build script recorded its jobserver");
    assert_eq!(seen, outer_auth, "Cargo used its own jobserver instead of make's");
}

#[test]
fn hauls_compiler_driver_builds_byte_for_byte_what_rustc_builds() {
    // The driver is the toolchain's own compiler run through Haul's callbacks: with nothing
    // changed by them, every output must be rustc's, byte for byte.
    let sb = Sandbox::new("driver");
    let ws = sb.workspace("ws");
    let Some(driver) = build_driver(&sb, &ws) else { return };
    let driver = driver.to_string_lossy().into_owned();
    // The compiler the driver was built for: the toolchain Haul uses here.
    let rustc = sb.toolchain_dir(&ws).join("bin/rustc").display().to_string();
    let outputs = |compiler: &str| -> std::collections::BTreeMap<String, Vec<u8>> {
        let target = ws.join("target");
        let _ = fs::remove_dir_all(&target);
        ok(sb.upstream(&ws).arg("build").env("RUSTC", compiler).env("CARGO_INCREMENTAL", "0"));
        let mut out = std::collections::BTreeMap::new();
        for e in fs::read_dir(target.join("debug/deps")).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".d") {
                out.insert(name, fs::read(e.path()).unwrap());
            }
        }
        out
    };
    let (stock, ours) = (outputs(&rustc), outputs(&driver));
    assert!(stock.len() >= 4, "{:?}", stock.keys());
    assert_eq!(stock.keys().collect::<Vec<_>>(), ours.keys().collect::<Vec<_>>());
    let differ: Vec<&String> = stock.iter().filter(|(k, v)| ours[*k] != **v).map(|(k, _)| k).collect();
    assert!(differ.is_empty(), "outputs that differ from rustc's: {:?}", differ);
}
