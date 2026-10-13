//! Compatibility corpus (QA-001 … QA-030): the `cargo` shim against upstream
//! Cargo as the oracle. Each test runs real builds in an isolated sandbox.

#![cfg(unix)]

mod common;

use common::*;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
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
    assert_eq!(shape(artifacts(&sb.target_dir(&wrapped))), shape(artifacts(&sb.target_dir(&native))));
    assert!(!tree(&wrapped).iter().any(|f| f.contains(".qrust")), "the shim must not create files in the repository");
    assert_eq!(stdout(&ok(sb.cargo(&wrapped).args(["run", "-q"]))), "3\n");
}

#[test]
fn no_rebuild_when_alternating_with_upstream() {
    // compat mode: the wrapper must be invisible to Cargo's fingerprints, same target directory,
    // zero recompilation.
    let sb = Sandbox::new("alternate");
    let ws = sb.workspace("ws");
    ok(sb.cargo(&ws).arg("build").env("QRUST_MODE", "compat"));
    let before = artifacts(&sb.target_dir(&ws));
    let up = ok(sb.upstream(&ws).arg("build"));
    assert!(!stderr(&up).contains("Compiling"), "upstream rebuilt after a shim build:\n{}", stderr(&up));
    let again = ok(sb.cargo(&ws).arg("build").env("QRUST_MODE", "compat"));
    assert!(!stderr(&again).contains("Compiling"), "shim rebuilt after an upstream build:\n{}", stderr(&again));
    assert_eq!(artifacts(&sb.target_dir(&ws)), before);
}

#[test]
fn fast_mode_and_upstream_keep_separate_artifacts_instead_of_rebuilding_each_other() {
    // Fast mode builds with other profile settings, so Cargo names its artifacts differently. After
    // each side has built once, alternating between them compiles nothing (docs/how-to/adopt-gradually.md).
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
    let inspect: Value = serde_json::from_slice(&ok(sb.qrust(&ws).args(["inspect", "--format", "json"])).stdout).unwrap();
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
    ok(sb.qrust(p).args(["toolchain", "link", name]).arg(&dir));
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
fn builds_and_their_build_scripts_get_quickrusts_toolchain_and_cargo_home() {
    let sb = Sandbox::new("engine-env");
    let p = sb.package("p", "p");
    sb.file("p/build.rs", "fn main() {\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    let get = |k: &str| std::env::var(k).unwrap_or_default();\n    let nested = std::process::Command::new(get(\"CARGO\")).arg(\"-V\").output().unwrap();\n    std::fs::write(format!(\"{out}/env.txt\"), format!(\"{}\\n{}\\n{}\\n{}\\n{}\", get(\"RUSTUP_TOOLCHAIN\"), get(\"CARGO_HOME\"), get(\"RUSTC\"), String::from_utf8_lossy(&nested.stdout).trim(), get(\"CARGO\"))).unwrap();\n}\n");
    let stable = sb.toolchain_dir(&p);
    ok(sb.cargo(&p).arg("build").env("CARGO_HOME", sb.dir.join("theirs")));
    let env_file = fs::read_dir(sb.target_dir(&p).join("debug/build")).unwrap().flatten().map(|e| e.path().join("out/env.txt")).find(|f| f.is_file()).expect("the build script ran");
    let lines: Vec<String> = fs::read_to_string(env_file).unwrap().lines().map(str::to_string).collect();
    assert_eq!(lines[0], stable.file_name().unwrap().to_string_lossy(), "nested toolchain programs are pinned to the build's toolchain");
    assert_eq!(lines[1], sb.cargo_home().display().to_string(), "QuickRust's Cargo home, whatever the environment says");
    assert_eq!(lines[2], stable.join("bin/rustc").display().to_string());
    assert_eq!(lines[3], stdout(&ok(std::process::Command::new(stable.join("bin/cargo")).arg("-V"))).trim());
    assert_eq!(lines[4], sb.home.join("bin/cargo").display().to_string(), "`CARGO` is QuickRust's: what calls Cargo back reaches QuickRust");
    assert!(!sb.dir.join("theirs").exists());
}

#[test]
fn toolchain_programs_run_through_quickrusts_proxies() {
    let sb = Sandbox::new("proxies");
    let p = sb.package("p", "p");
    let stable = sb.toolchain_dir(&p);
    link_alias(&sb, &p, "alt");
    let bin = sb.dir.join("proxies");
    fs::create_dir_all(&bin).unwrap();
    for tool in ["rustc", "rustfmt"] {
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_qrust"), bin.join(tool)).unwrap();
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
fn builds_go_to_quickrusts_cache_and_never_into_the_project() {
    let sb = Sandbox::new("builddir");
    let ws = sb.workspace("ws");
    let target = sb.target_dir(&ws);
    assert!(target.starts_with(sb.home.join("cache/builds")), "{}", target.display());
    // An external subcommand that builds through `$CARGO`, as tools do.
    let sub = sb.file("bin/cargo-tool", "#!/bin/sh\n\"$CARGO\" build -q -p util || exit 9\n");
    make_executable(&sub);
    let path = std::env::join_paths(std::iter::once(sb.dir.join("bin")).chain(std::env::split_paths(&sb.path_with_shim()))).unwrap();
    for args in [&["build"][..], &["check"], &["test", "--no-run"], &["doc", "--no-deps"], &["clippy"], &["package", "--allow-dirty", "--no-verify", "--offline", "-p", "util"], &["tool"]] {
        ok(sb.cargo(&ws).args(args).env("PATH", &path));
        assert!(!ws.join("target").exists(), "`cargo {}` wrote into the project", args.join(" "));
    }
    assert!(target.join("debug/app").is_file() && target.join("doc/util/index.html").is_file() && target.join("package/util-0.1.0.crate").is_file());
    let meta: Value = serde_json::from_slice(&ok(sb.cargo(&ws).args(["metadata", "--format-version", "1", "--no-deps"])).stdout).unwrap();
    assert_eq!(meta["target_directory"].as_str(), Some(target.to_str().unwrap()), "tools asking Cargo get QuickRust's directory");
    assert_eq!(stdout(&ok(sb.qrust(&ws).args(["path", "bin", "app"]))).trim(), target.join("debug/app").display().to_string());
    // A member directory belongs to its workspace's build directory.
    assert_eq!(sb.target_dir(&ws.join("util")), target);
    // Another checkout of the same code has a directory of its own.
    copy_dir(&ws, &sb.dir.join("ws-2"));
    assert_ne!(sb.target_dir(&sb.dir.join("ws-2")), target);
    // `cargo clean` empties it, as it would `target/`.
    ok(sb.cargo(&ws).arg("clean"));
    assert!(!target.exists());
}

/// One workspace built in two configurations — here a value baked in at build time, as a registry
/// or a channel is — keeps both builds when each names its variant: neither overwrites the other's
/// binary, and each is found by asking under its variant.
#[test]
fn build_variants_keep_each_configurations_output() {
    let sb = Sandbox::new("variants");
    let p = sb.package("p", "p");
    sb.file("p/build.rs", "fn main() {\n    println!(\"cargo:rerun-if-env-changed=FLAVOR\");\n    println!(\"cargo:rustc-env=BAKED_FLAVOR={}\", std::env::var(\"FLAVOR\").unwrap_or_default());\n}\n");
    sb.file("p/src/main.rs", "fn main() {\n    println!(\"{}\", env!(\"BAKED_FLAVOR\"));\n}\n");
    let plain = sb.target_dir(&p);
    let in_variant = |variant: &str, args: &[&str]| {
        let mut c = sb.qrust(&p);
        c.args(args).env("QRUST_BUILD_VARIANT", variant);
        c
    };
    let dir_of = |variant: &str| PathBuf::from(stdout(&ok(&mut in_variant(variant, &["path"]))).trim());
    let (staging, prod) = (dir_of("staging"), dir_of("prod"));
    assert_eq!(staging.file_name().unwrap().to_string_lossy(), format!("{}.staging", plain.file_name().unwrap().to_string_lossy()));
    assert!(staging != prod && staging.parent() == plain.parent(), "{} / {}", staging.display(), prod.display());

    for flavor in ["staging", "prod"] {
        ok(sb.cargo(&p).arg("build").env("QRUST_BUILD_VARIANT", flavor).env("FLAVOR", flavor));
    }
    // Both are there at once, each the configuration it was built in.
    let says = |binary: &Path| stdout(&ok(&mut Command::new(binary)));
    assert_eq!(says(&staging.join("debug/p")), "staging\n");
    assert_eq!(says(&prod.join("debug/p")), "prod\n");
    assert_eq!(stdout(&ok(&mut in_variant("prod", &["path", "bin", "p"]))).trim(), prod.join("debug/p").display().to_string());
    assert!(!plain.exists() && !p.join("target").exists(), "nothing was built outside the two variants");

    // Each is a build directory QuickRust's cache knows as this workspace's, and collects (once it
    // is not what was used a moment ago).
    for dir in [&staging, &prod] {
        let record = dir.with_file_name(format!("{}.json", dir.file_name().unwrap().to_string_lossy()));
        let mut v: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        assert_eq!(v["workspace_root"].as_str().map(Path::new), Some(fs::canonicalize(&p).unwrap().as_path()), "{}", v);
        v["last_used_ns"] = serde_json::json!(v["last_used_ns"].as_u64().unwrap() - 3 * 3_600_000_000_000);
        fs::write(&record, v.to_string()).unwrap();
    }
    let removed: Value = serde_json::from_slice(&ok(sb.qrust(&p).args(["gc", "--to", "1", "--format", "json"])).stdout).unwrap();
    let removed: Vec<&str> = removed["removed"].as_array().unwrap().iter().filter(|r| r["kind"] == "build directories").filter_map(|r| r["path"].as_str()).collect();
    for dir in [&staging, &prod] {
        assert!(removed.contains(&dir.to_str().unwrap()) && !dir.exists(), "{} of {:?}", dir.display(), removed);
    }

    // A variant is a word, since it names a directory.
    let bad = run(&mut in_variant("../elsewhere", &["path"]));
    assert_eq!(bad.status.code(), Some(2), "{}", stderr(&bad));
    assert!(stderr(&bad).contains("invalid build variant \"../elsewhere\""), "{}", stderr(&bad));
}

#[test]
fn a_target_directory_the_user_chose_is_used_and_cargo_configuration_files_are_not() {
    let sb = Sandbox::new("usertarget");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).args(["build", "--target-dir", "out"]));
    assert!(p.join("out/debug/p").is_file());
    let mine = sb.dir.join("mine");
    ok(sb.cargo(&p).arg("build").env("CARGO_TARGET_DIR", &mine));
    assert!(mine.join("debug/p").is_file());
    assert_eq!(stdout(&ok(sb.qrust(&p).arg("path").env("CARGO_TARGET_DIR", &mine))).trim(), mine.display().to_string());
    ok(sb.cargo(&p).args(["--config", "build.target-dir=\"cli-out\"", "build"]));
    assert!(p.join("cli-out/debug/p").is_file());
    // A Cargo configuration file's target directory (a global one, say) gives way to QuickRust's.
    sb.file("p/.cargo/config.toml", "[build]\ntarget-dir = \"configured\"\n");
    ok(sb.cargo(&p).arg("build"));
    assert!(!p.join("configured").exists());
    assert!(sb.target_dir(&p).join("debug/p").is_file());
    assert!(!sb.home.join("cache/builds").read_dir().unwrap().any(|e| e.unwrap().path() == mine), "a directory the user chose is never QuickRust's");
}

#[test]
fn a_build_script_building_another_workspace_builds_it_in_that_workspaces_directory() {
    let sb = Sandbox::new("nestedws");
    let other = sb.package("other", "other");
    let p = sb.package("p", "p");
    // Like a build script that builds a helper project (a wasm module, a plugin) with `cargo`.
    sb.file("p/build.rs", &format!("fn main() {{\n    let s = std::process::Command::new(\"cargo\").args([\"build\", \"-q\", \"--manifest-path\", {:?}]).status().unwrap();\n    assert!(s.success());\n}}\n", other.join("Cargo.toml").display().to_string()));
    ok(sb.cargo(&p).arg("build"));
    assert!(sb.target_dir(&other).join("debug/other").is_file(), "the other workspace's own directory");
    assert!(!sb.target_dir(&p).join("debug/other").exists());
    assert!(!p.join("target").exists() && !other.join("target").exists());
}

#[test]
fn qrust_installs_programs_into_its_own_bin() {
    use common::registry::{Crate, Registry};
    let sb = Sandbox::new("install");
    let reg = Registry::start();
    reg.publish(&Crate::bin("hello-tool", "1.0.0", "hello 1.0"));
    reg.publish(&Crate::bin("hello-tool", "1.1.0", "hello 1.1"));
    reg.publish(&Crate { name: "clash", vers: "1.0.0", files: vec![("src/bin/rustfmt.rs", "fn main() {}\n".into())], deps: vec![] });
    reg.publish(&Crate { name: "other-tool", vers: "1.0.0", files: vec![("src/bin/hello-tool.rs", "fn main() { println!(\"other\"); }\n".into())], deps: vec![] });
    reg.serve_crates_io_for(&sb.home);
    let bin = sb.home.join("bin");
    let run_tool = |name: &str| stdout(&ok(&mut std::process::Command::new(bin.join(name))));
    // `cargo install` is QuickRust's: the newest release, built and put into QuickRust's bin.
    let started = std::time::Instant::now();
    let out = ok(sb.cargo(&sb.dir).args(["install", "hello-tool"]));
    let took = started.elapsed();
    let said = stderr(&out);
    let result = said.lines().find(|l| l.contains("Installed hello-tool 1.1.0")).unwrap_or_else(|| panic!("{}", said));
    // The time it states is the install's, resolution and build included.
    let stated = result.rsplit_once(" [").and_then(|(_, d)| d.strip_suffix(']')).and_then(|d| d.strip_suffix("ms").map(|m| m.parse::<f64>().ok().map(|m| m / 1e3)).unwrap_or_else(|| d.strip_suffix('s').and_then(|s| s.parse::<f64>().ok()))).unwrap_or_else(|| panic!("{}", result));
    assert!(stated >= took.as_secs_f64() / 2.0, "{} for an install of {:?}", result, took);
    assert_eq!(run_tool("hello-tool"), "hello 1.1\n");
    assert!(!sb.dir.join(".cargo").exists() && !sb.dir.join("target").exists());
    let again = ok(sb.qrust(&sb.dir).args(["install", "hello-tool"]));
    assert!(stderr(&again).contains("installed already"), "{}", stderr(&again));
    // Another release replaces it.
    ok(sb.qrust(&sb.dir).args(["install", "hello-tool@1.0.0"]));
    assert_eq!(run_tool("hello-tool"), "hello 1.0\n");
    assert_eq!(stdout(&ok(sb.qrust(&sb.dir).args(["install", "--list"]))), "hello-tool v1.0.0 (crates.io):\n    hello-tool\n");
    // Never over QuickRust's own names, nor over another package's binary without --force.
    let clash = run(sb.qrust(&sb.dir).args(["install", "clash"]));
    assert_eq!(clash.status.code(), Some(2));
    assert!(stderr(&clash).contains("would replace QuickRust's own `rustfmt`"), "{}", stderr(&clash));
    let taken = run(sb.qrust(&sb.dir).args(["install", "other-tool"]));
    assert!(stderr(&taken).contains("is hello-tool's; --force replaces it"), "{}", stderr(&taken));
    assert_eq!(run_tool("hello-tool"), "hello 1.0\n");
    ok(sb.qrust(&sb.dir).args(["install", "other-tool", "--force"]));
    assert_eq!(run_tool("hello-tool"), "other\n");
    // A file nobody installed is not replaced either.
    fs::write(bin.join("stray"), "x").unwrap();
    reg.publish(&Crate { name: "stray-maker", vers: "1.0.0", files: vec![("src/bin/stray.rs", "fn main() {}\n".into())], deps: vec![] });
    assert!(stderr(&run(sb.qrust(&sb.dir).args(["install", "stray-maker"]))).contains("was not installed by QuickRust"));
    // From a path, and into another root.
    let p = sb.package("local", "local-tool");
    ok(sb.qrust(&p).args(["install", "--path", "."]));
    assert_eq!(run_tool("local-tool"), "3\n");
    let elsewhere = sb.dir.join("elsewhere");
    ok(sb.qrust(&sb.dir).args(["install", "hello-tool", "--root"]).arg(&elsewhere));
    assert_eq!(stdout(&ok(&mut std::process::Command::new(elsewhere.join("bin/hello-tool")))), "hello 1.1\n");
    // Dependencies resolved afresh, as by `cargo install`; with --locked, as the lockfile
    // published with the crate says.
    let dep_10 = reg.publish(&Crate::lib("dep", "1.0.0", "pub const V: &str = \"dep 1.0.0\";\n"));
    reg.publish(&Crate::lib("dep", "1.1.0", "pub const V: &str = \"dep 1.1.0\";\n"));
    let published_lock = format!("version = 4\n\n[[package]]\nname = \"dep\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{}\"\n\n[[package]]\nname = \"locky\"\nversion = \"1.0.0\"\ndependencies = [\n \"dep\",\n]\n", dep_10);
    reg.publish(&Crate { name: "locky", vers: "1.0.0", files: vec![("src/main.rs", "fn main() {\n    println!(\"{}\", dep::V);\n}\n".into()), ("Cargo.lock", published_lock)], deps: vec![("dep", "1")] });
    ok(sb.qrust(&sb.dir).args(["install", "locky"]));
    assert_eq!(run_tool("locky"), "dep 1.1.0\n");
    ok(sb.qrust(&sb.dir).args(["install", "locky", "--locked", "--force"]));
    assert_eq!(run_tool("locky"), "dep 1.0.0\n");
    ok(sb.qrust(&sb.dir).args(["install", "locky", "--force"]));
    assert_eq!(run_tool("locky"), "dep 1.1.0\n", "the published lockfile stayed in force");
    // Uninstalled: binaries and record.
    ok(sb.cargo(&sb.dir).args(["uninstall", "local-tool"]));
    assert!(!bin.join("local-tool").exists());
    assert!(!stdout(&ok(sb.qrust(&sb.dir).args(["install", "--list"]))).contains("local-tool"));
    assert_eq!(run(sb.qrust(&sb.dir).args(["uninstall", "local-tool"])).status.code(), Some(2));
}

#[test]
#[cfg(target_os = "linux")]
fn toolchain_programs_load_their_own_libraries_whatever_the_library_path_says() {
    // rustup puts its toolchain's `lib` on LD_LIBRARY_PATH for everything it runs (`cargo test`
    // included): with the same release as QuickRust's, its librustc_driver has the same name, and a
    // rustc loading it takes rustup's toolchain for its sysroot. Here a decoy of that name, which
    // fails to load at all, stands for it.
    let sb = Sandbox::new("libpath");
    let p = sb.package("p", "p");
    let tc = sb.toolchain_dir(&p);
    let driver_lib = fs::read_dir(tc.join("lib")).unwrap().flatten().map(|e| e.file_name()).find(|n| n.to_string_lossy().starts_with("librustc_driver-")).expect("the toolchain's librustc_driver");
    let decoy = sb.dir.join("decoy");
    fs::create_dir_all(&decoy).unwrap();
    fs::write(decoy.join(&driver_lib), "not a library").unwrap();
    let with_decoy = |c: &mut Command| {
        c.env("LD_LIBRARY_PATH", &decoy);
    };
    let mut status = sb.qrust(&p);
    with_decoy(status.args(["driver", "status"]));
    let out = ok(&mut status);
    assert!(stdout(&out).contains("1."), "{}", stdout(&out));
    let mut build = sb.cargo(&p);
    with_decoy(build.arg("build"));
    ok(&mut build);
    let mut doctor = sb.qrust(&p);
    with_decoy(doctor.arg("doctor"));
    assert!(stdout(&run(&mut doctor)).contains("  rustc              rustc 1."));
    build_driver(&sb, &p);
    let mut driver = sb.qrust(&p);
    with_decoy(driver.args(["driver", "build"]));
    ok(&mut driver);
}

#[test]
fn qa004_manifest_path_from_another_directory() {
    let sb = Sandbox::new("manifest");
    let p = sb.package("deep/p", "p");
    let manifest = p.join("Cargo.toml");
    ok(sb.cargo(&sb.dir).args(["build", "--manifest-path"]).arg(&manifest));
    assert!(sb.target_dir(&p).join("debug/p").exists());
    let sessions = sb.sessions();
    assert_eq!(sessions.last().unwrap()["workspace_root"].as_str().unwrap(), p.to_str().unwrap(), "the workspace is found through --manifest-path, not the cwd");
}

#[test]
fn qa005_features_reach_rustc_unchanged() {
    // FR-001 acceptance: `cargo build --features a,b` selects the same build as upstream (compat
    // mode builds exactly upstream's; fast mode would differ in profile settings by design).
    let sb = Sandbox::new("features");
    let p = sb.package("p", "p");
    ok(sb.cargo(&p).args(["build", "--features", "a,b"]).env("QRUST_MODE", "compat"));
    let report = sb.report(&p, "latest");
    let lib = report["units"].as_array().unwrap().iter().find(|u| u["kind"] == "lib").unwrap();
    assert_eq!(lib["features"], serde_json::json!(["a", "b"]));
    let before = artifacts(&sb.target_dir(&p));
    let up = ok(sb.upstream(&p).args(["build", "--features", "a,b"]));
    assert!(!stderr(&up).contains("Compiling"), "upstream considers the shim's build stale:\n{}", stderr(&up));
    assert_eq!(artifacts(&sb.target_dir(&p)), before);
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
    assert!(sb.target_dir(&p).join(target).join("debug").is_dir());
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
    assert!(sb.report(&p, "latest")["metrics"]["units_compiled"].as_u64().unwrap() >= 2, "QuickRust still observed the units");

    // QA-011: RUSTC_WORKSPACE_WRAPPER alone.
    fs::remove_dir_all(sb.target_dir(&p)).unwrap();
    ok(sb.cargo(&p).arg("build").env("RUSTC_WORKSPACE_WRAPPER", &ws_wrapper));
    assert!(fs::read_to_string(&ws_log).unwrap().lines().count() >= 2);

    // QA-012: both. Cargo's documented order is $RUSTC_WRAPPER $RUSTC_WORKSPACE_WRAPPER $RUSTC.
    fs::remove_dir_all(sb.target_dir(&p)).unwrap();
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
    assert!(sb.target_dir(&p).join("doc/p/index.html").exists());
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
    let before = tree(&sb.target_dir(&ws));
    // A collection never removes a build directory in use (used within the last minutes).
    ok(sb.qrust(&ws).arg("gc").env("QRUST_CACHE_MAX_SIZE", "1"));
    assert_eq!(tree(&sb.target_dir(&ws)), before, "gc removed a build directory just used");
    // Selective clean removes the package's artifacts and nothing else.
    let a = ok(sb.cargo(&ws).args(["clean", "-p", "util"]));
    assert!(stderr(&a).contains("Removed"), "{}", stderr(&a));
    let after = tree(&sb.target_dir(&ws));
    assert!(!after.iter().any(|f| f.contains("/deps/libutil-")));
    assert!(after.iter().any(|f| f.contains("/deps/libgen-")));
    ok(sb.cargo(&ws).arg("clean"));
    assert!(!sb.target_dir(&ws).exists());
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
    // what the delegated Cargo prints, describing the settings QuickRust applied.
    let sb = Sandbox::new("json");
    let ws = sb.workspace("ws");
    let args = ["build", "-j1", "--message-format=json"];
    let wrapped = ok(sb.cargo(&ws).args(args).env("QRUST_MODE", "compat"));
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
    let again = ok(sb.cargo(&ws).args(args).env("QRUST_MODE", "compat"));
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

    // Clippy, nextest and most other subcommands call Cargo back through `$CARGO` (COMP-030).
    let back = sb.file("bin/cargo-back", "#!/bin/sh\n\"$CARGO\" build --quiet || exit 9\nexit 6\n");
    make_executable(&back);
    let out = run(sb.cargo(&p).arg("back").env("PATH", &path));
    assert_eq!(out.status.code(), Some(6), "{}", stderr(&out));
    let sessions = sb.sessions();
    assert_eq!(sessions.len(), 4);
    let (outer, nested) = (&sessions[2], &sessions[3]);
    assert_eq!((&outer["command"]["family"], &nested["command"]["family"]), (&"external".into(), &"build".into()));
    assert_eq!(nested["parent_session_id"], outer["id"], "a build through `$CARGO` is a nested QuickRust session");
    // `qrust upstream` is the toolchain's Cargo as it is: it hands on its own path.
    let out = run(sb.qrust(&p).args(["upstream", "back"]).env("PATH", &path));
    assert_eq!(out.status.code(), Some(6), "{}", stderr(&out));
    assert_eq!(sb.sessions().len(), 4, "nothing under `qrust upstream` reaches QuickRust");

    // COMP-032: projects can opt nested invocations out; they then go straight to upstream.
    fs::remove_dir_all(sb.target_dir(&p)).unwrap();
    let opt_out = run(sb.cargo(&p).args(["hello"]).env("PATH", &path).env("QRUST_NESTED", "upstream"));
    assert_eq!(opt_out.status.code(), Some(5));
    assert!(sb.target_dir(&p).join("debug/p").exists());
    assert_eq!(sb.sessions().len(), 5, "only the outer invocation is recorded when nested ones are opted out");
}

#[test]
fn qa020_aliases_are_resolved_by_cargo_and_classified_for_observation() {
    let sb = Sandbox::new("alias");
    let p = sb.package("p", "p");
    sb.file("p/.cargo/config.toml", "[alias]\nxb = \"build --features a\"\n");
    ok(sb.cargo(&p).arg("xb"));
    assert!(sb.target_dir(&p).join("debug/p").exists());
    let s = sb.sessions().pop().unwrap();
    assert_eq!((s["command"]["family"].as_str(), s["command"]["alias_of"].as_str(), s["instrumented"].as_bool()), (Some("build"), Some("build"), Some(true)));
    assert_eq!(sb.report(&p, "latest")["units"].as_array().unwrap().iter().find(|u| u["kind"] == "lib").unwrap()["features"], serde_json::json!(["a"]));
}

#[test]
fn qa021_package_and_login_pass_through_and_secrets_never_reach_qrust_state() {
    let sb = Sandbox::new("registry");
    let p = sb.package("p", "p");
    let a = ok(sb.cargo(&p).args(["package", "--allow-dirty", "--no-verify", "--offline"]));
    assert!(sb.target_dir(&p).join("package/p-0.1.0.crate").exists(), "{}", stderr(&a));
    // `cargo login` against a mock registry: the token lands in QuickRust's Cargo home.
    let cargo_home = sb.cargo_home();
    let index = sb.dir.join("mock-index");
    sb.file("mock-index/config.json", "{\"dl\":\"file:///nonexistent/dl\",\"api\":\"http://127.0.0.1:9\"}\n");
    git(&index, &["init", "-q"]);
    git(&index, &["add", "."]);
    git(&index, &["commit", "-q", "-m", "index"]);
    sb.file("qrust-home/cargo/config.toml", &format!("[registries.mock]\nindex = \"file://{}\"\n", index.display()));
    let secret = "s3cr3t-token-0123456789abcdef";
    let login = |c: &mut std::process::Command| {
        c.args(["login", "--registry", "mock", secret]);
    };
    let mut c = sb.cargo(&p);
    login(&mut c);
    let out = run(&mut c);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(fs::read_to_string(cargo_home.join("credentials.toml")).unwrap().contains(secret), "Cargo stored the token: the command was delegated intact");
    // AC-019: the credential must not appear anywhere in QuickRust's records (Cargo's home aside).
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
    // --locked with a missing lockfile fails identically, and QuickRust must not have created one.
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

    let open = run(sb.cargo(&p).arg("build").env("PATH", &path).env("QRUST_CACHE_PROVIDER", "sccache"));
    assert!(open.status.success(), "fail-open must compile locally: {}", stderr(&open));
    assert!(stderr(&open).contains("WARN sccache is unavailable") && stderr(&open).contains("compiling without it"), "{}", stderr(&open));
    assert!(sb.target_dir(&p).join("debug/p").exists());
    let s = sb.sessions().pop().unwrap();
    assert_eq!(s["policy"]["cache"]["state"], "degraded");
    assert!(s["wrappers"]["inner"].is_null(), "the unavailable cache is not in the wrapper chain");
    // AC-018: nothing in the report claims a cache hit.
    let report = sb.report(&p, "latest");
    assert!(report["cache"]["hits"].is_null());
    assert!(report["units"].as_array().unwrap().iter().all(|u| u["cacheability"] == "n/a"));

    fs::remove_dir_all(sb.target_dir(&p)).unwrap();
    let closed = run(sb.cargo(&p).arg("build").env("PATH", &path).env("QRUST_CACHE_PROVIDER", "sccache").env("QRUST_CACHE_FAIL", "closed"));
    assert_eq!(closed.status.code(), Some(2), "{}", stderr(&closed));
    assert!(stderr(&closed).contains("cache.fail = closed"));
    assert!(!sb.target_dir(&p).exists(), "fail-closed stops before any compilation");
}

#[test]
fn qa027_unwritable_report_or_state_never_fails_a_successful_build() {
    let sb = Sandbox::new("diskfull");
    let p = sb.package("p", "p");
    let blocker = sb.file("blocker", "a file where a directory is needed");
    // API-004: report generation failure leaves the Cargo result alone …
    let out = run(sb.cargo(&p).arg("build").env("QRUST_REPORT", blocker.join("report.json")));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("WARN could not write report"), "{}", stderr(&out));
    // … unless strict reporting was requested.
    let strict = run(sb.cargo(&p).arg("build").env("QRUST_REPORT", blocker.join("report.json")).env("QRUST_MEASUREMENT_STRICT", "true"));
    assert_eq!(strict.status.code(), Some(2));
    // NFR-020: QuickRust's records cannot be written: the build still runs, with a warning. Here the
    // records' directory is a regular file (a read-only directory would not stop root); the
    // toolchains QuickRust installed are still there to build with.
    let unusable = sb.dir.join("records-blocked");
    fs::create_dir_all(&unusable).unwrap();
    std::os::unix::fs::symlink(toolchain_store(), unusable.join("toolchains")).unwrap();
    fs::write(unusable.join("workspaces"), "").unwrap();
    fs::remove_dir_all(sb.target_dir(&p)).unwrap();
    let no_state = run(sb.cargo(&p).arg("build").env("QRUST_HOME", &unusable));
    assert!(no_state.status.success(), "{}", stderr(&no_state));
    assert!(stderr(&no_state).contains("this run was not recorded"), "{}", stderr(&no_state));
    let target = stdout(&ok(sb.qrust(&p).arg("path").env("QRUST_HOME", &unusable))).trim().to_string();
    assert!(std::path::Path::new(&target).join("debug/p").exists());
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
    // In a terminal, where QuickRust renders builds: help is Cargo's own, never shown as a build.
    for args in ["build --help", "check -h", "clippy --help"] {
        let Some(mut ours) = sb.cargo_in_terminal(&sb.dir, args) else { return };
        let theirs = run(&mut sb.upstream_in_terminal(&sb.dir, args).unwrap());
        assert_eq!(String::from_utf8_lossy(&run(&mut ours).stdout), String::from_utf8_lossy(&theirs.stdout), "{}", args);
    }
}

#[test]
fn recursion_is_refused_with_a_diagnostic() {
    let sb = Sandbox::new("recursion");
    let p = sb.package("p", "p");
    // A linked "toolchain" whose cargo is QuickRust: refused when linked if it is the same file.
    let fake = sb.dir.join("fake-toolchain/bin");
    fs::create_dir_all(&fake).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_qrust"), fake.join("cargo")).unwrap();
    std::os::unix::fs::symlink(sb.toolchain_dir(&p).join("bin/rustc"), fake.join("rustc")).unwrap();
    let a = run(sb.qrust(&p).args(["toolchain", "link", "loop"]).arg(sb.dir.join("fake-toolchain")));
    assert_eq!(a.status.code(), Some(2));
    assert!(stderr(&a).contains("is QuickRust itself"), "{}", stderr(&a));
    // A byte-for-byte copy has a different inode and path: only the session guard can catch it (COMP-022).
    fs::remove_file(fake.join("cargo")).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_qrust"), fake.join("cargo")).unwrap();
    ok(sb.qrust(&p).args(["toolchain", "link", "loop"]).arg(sb.dir.join("fake-toolchain")));
    let b = run(sb.cargo(&p).args(["+loop", "build"]));
    assert_eq!(b.status.code(), Some(2), "{}", stderr(&b));
    assert!(stderr(&b).contains("refusing to run Cargo"), "{}", stderr(&b));
    assert!(!sb.target_dir(&p).exists());
}

#[test]
fn an_unreachable_dist_server_is_a_clear_error() {
    let sb = Sandbox::new("nocargo");
    let p = sb.package("p", "p");
    // A QuickRust home without the suite's toolchains.
    let empty = sb.dir.join("empty-home");
    let out = run(sb.cargo(&p).arg("build").env("QRUST_HOME", &empty).env("QRUST_DIST_SERVER", "http://127.0.0.1:9"));
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("toolchain stable (from default toolchain): could not download http://127.0.0.1:9/dist/channel-rust-stable.toml"), "{}", stderr(&out));
    assert!(!sb.target_dir(&p).exists());
}

#[test]
fn explicit_qrust_entry_and_upstream_escape_hatch() {
    let sb = Sandbox::new("explicit");
    let p = sb.package("p", "p");
    // Migration stage "explicit wrapper": `qrust build` is Cargo syntax through QuickRust.
    ok(sb.qrust(&p).arg("build"));
    assert_eq!(sb.sessions().pop().unwrap()["command"]["entry"], "qrust");
    // AC-020 / COMP-013: the escape hatch runs upstream with no session and no wrapper.
    let count = sb.sessions().len();
    fs::remove_dir_all(sb.target_dir(&p)).unwrap();
    let (wrapper, log) = logging_wrapper(&sb, "probe");
    let out = ok(sb.qrust(&p).args(["upstream", "build"]).env("RUSTC_WRAPPER", &wrapper));
    assert!(stderr(&out).contains("Compiling p"));
    assert_eq!(sb.sessions().len(), count, "`qrust upstream` records nothing");
    assert!(fs::read_to_string(&log).unwrap().lines().all(|l| l.ends_with("rustc")), "the user's wrapper received rustc directly: no QuickRust wrapper in the chain");
}

#[test]
fn environment_reaches_build_scripts_unchanged() {
    // Compatibility oracle: build-script-visible relevant environment.
    let sb = Sandbox::new("env");
    let script = "fn main() {\n    let mut vars: Vec<String> = std::env::vars().filter(|(k, _)| !k.starts_with(\"QRUST_\") && k != \"RUSTC_WRAPPER\" && k != \"CARGO\" && k != \"OUT_DIR\" && k != \"CARGO_MANIFEST_DIR\" && k != \"CARGO_MANIFEST_PATH\" && k != \"PATH\" && k != \"CARGO_TARGET_DIR\" && k != \"CARGO_BUILD_BUILD_DIR\" && k != \"DYLD_FALLBACK_LIBRARY_PATH\" && k != \"LD_LIBRARY_PATH\" && k != \"PWD\" && k != \"OLDPWD\" && k != \"DUMP\").map(|(k, v)| format!(\"{k}={v}\")).collect();\n    vars.sort();\n    std::fs::write(std::env::var(\"DUMP\").unwrap(), vars.join(\"\\n\")).unwrap();\n}\n";
    let mut dumps = Vec::new();
    for (name, wrapped) in [("wrapped", true), ("native", false)] {
        let p = sb.package(&format!("{}/p", name), "p");
        sb.file(&format!("{}/p/build.rs", name), script);
        let dump = sb.dir.join(format!("{}.env", name));
        let mut c = if wrapped { sb.cargo(&p) } else { sb.upstream(&p) };
        ok(c.arg("build").env("DUMP", &dump).env("RUSTFLAGS", "--cfg some_flag").env("MY_PROXY_SETTING", "http://proxy.invalid:1"));
        dumps.push(fs::read_to_string(dump).unwrap());
        // Each project's own build directory, as for OUT_DIR: QuickRust's, the oracle given the same.
        assert!(!p.join("target").exists());
        assert!(sb.target_dir(&p).join("debug/p").is_file());
    }
    let (w, n): (Vec<&str>, Vec<&str>) = (dumps[0].lines().collect(), dumps[1].lines().collect());
    let only_wrapped: Vec<&&str> = w.iter().filter(|l| !n.contains(l)).collect();
    let only_native: Vec<&&str> = n.iter().filter(|l| !w.contains(l)).collect();
    assert!(only_wrapped.is_empty() && only_native.is_empty(), "build scripts must see the same environment apart from QuickRust's own variables\nonly under the shim: {:#?}\nonly under upstream: {:#?}", only_wrapped, only_native);
    assert!(dumps[0].contains("CARGO_ENCODED_RUSTFLAGS=--cfg\u{1f}some_flag"));
    assert!(dumps[0].contains("MY_PROXY_SETTING=http://proxy.invalid:1"));
}

#[test]
fn invalid_qrust_configuration_names_the_field_and_its_source() {
    let sb = Sandbox::new("badconfig");
    let p = sb.package("p", "p");
    sb.file("p/.qrust/config.toml", "[cache]\nprovidr = \"sccache\"\n");
    let out = run(sb.cargo(&p).arg("build"));
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.contains("cache.providr") && err.contains(".qrust/config.toml"), "{}", err);
    assert!(!p.join("target").exists() && !sb.home.join("cache/builds").exists(), "an invalid configuration stops before the build instead of guessing");
    // Configuration errors are reported consistently for every command, queries included.
    assert_eq!(run(sb.cargo(&p).arg("--version")).status.code(), Some(2));
    // The escape hatch does not depend on QuickRust's configuration.
    ok(sb.qrust(&p).args(["upstream", "--version"]));
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
    // COMP-007: Cargo's standard streams are a terminal under QuickRust as under upstream Cargo: it
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
    let ours = run(ours.env("QRUST_MODE", "compat").env_remove("CARGO_TERM_COLOR"));
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
    // QuickRust's own way, building first and then running the command (the default in a terminal).
    let (ours, took) = interrupted(sb.cargo_in_terminal(&p, "run").unwrap());
    assert_eq!(ours, theirs, "the same exit status as upstream Cargo after Ctrl-C");
    assert!(took < std::time::Duration::from_secs(10), "Ctrl-C ended the run after {:?}", took);
}

#[test]
fn comp042_the_jobserver_reaches_rustc_through_the_wrapper_and_cargo_from_make() {
    // COMP-042 / FR-120.4: Cargo's jobserver reaches the compiler through QuickRust's rustc wrapper,
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

    // Fast mode: every compilation passes through QuickRust's wrapper.
    let built = ok(sb.cargo(&p).arg("build"));
    assert!(stderr(&built).contains("jobserver token acquired and released"), "{}", stderr(&built));
    assert!(!stderr(&built).contains(refused), "the compiler lost the jobserver: {}", stderr(&built));
    // The check can fail: a wrapper of the user's that closes the jobserver's descriptors, which
    // QuickRust runs inside its own, makes the compiler say so.
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
    let seen = fs::read_dir(sb.target_dir(&p).join("debug/build")).unwrap().filter_map(|e| fs::read_to_string(e.ok()?.path().join("out/jobserver-auth")).ok()).next().expect("the build script recorded its jobserver");
    assert_eq!(seen, outer_auth, "Cargo used its own jobserver instead of make's");
}

#[test]
fn quickrusts_compiler_driver_builds_byte_for_byte_what_rustc_builds() {
    // The driver is the toolchain's own compiler run through QuickRust's callbacks: with nothing
    // changed by them, every output must be rustc's, byte for byte.
    let sb = Sandbox::new("driver");
    let ws = sb.workspace("ws");
    let driver = build_driver(&sb, &ws);
    let driver = driver.to_string_lossy().into_owned();
    // The compiler the driver was built for: the toolchain QuickRust uses here.
    let rustc = sb.toolchain_dir(&ws).join("bin/rustc").display().to_string();
    let outputs = |compiler: &str| -> std::collections::BTreeMap<String, Vec<u8>> {
        let target = sb.target_dir(&ws);
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
