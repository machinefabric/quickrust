//! What Haul shows in a terminal, screen by screen: each command's screen (after progress lines
//! have been redrawn and cleared, as a terminal shows it) is compared whole with its snapshot in
//! `tests/snapshots/`. Only what varies between runs and hosts is replaced: durations
//! (`<time>`), sizes (`<size>`), counts of files (`<count> files`), the sandbox's paths
//! (`<sandbox>`, `<home>`) and build-directory ids (`<id>`).
//! A missing or different snapshot fails the test and shows the screen; `HAUL_UPDATE_SNAPSHOTS=1`
//! writes the screens as the new snapshots, for review in the diff.
//!
//! The terminal is util-linux `script`'s (as for the other suites' terminal checks): Linux only.

#![cfg(target_os = "linux")]

mod common;

use common::registry::{Crate, Registry};
use common::*;
use std::fs;
use std::path::PathBuf;

/// The toolchain the screens are taken with: compiler messages and Cargo's help differ between
/// releases.
const TOOLCHAIN: &str = "1.99.0";

struct Screens<'a> {
    sb: &'a Sandbox,
    dir: PathBuf,
    update: bool,
    mismatches: Vec<String>,
}

impl<'a> Screens<'a> {
    fn new(sb: &'a Sandbox) -> Screens<'a> {
        Screens { sb, dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots"), update: std::env::var_os("HAUL_UPDATE_SNAPSHOTS").is_some_and(|v| v == "1"), mismatches: Vec::new() }
    }

    /// `cargo <args>` in a terminal in `cwd`, compared with snapshot `name`.
    fn check(&mut self, name: &str, cwd: &std::path::Path, args: &str) {
        let mut c = self.sb.cargo_in_terminal(cwd, args).expect("the screens need util-linux `script`");
        let shown = normalize(&screen(&run(&mut c).stdout), self.sb);
        let path = self.dir.join(format!("{}.screen", name));
        if self.update {
            fs::create_dir_all(&self.dir).unwrap();
            fs::write(&path, format!("{}\n", shown)).unwrap();
            return;
        }
        match fs::read_to_string(&path) {
            Ok(want) if want.trim_end_matches('\n') == shown => {}
            Ok(want) => self.mismatches.push(format!("`cargo {}` ({}):\n--- snapshot\n{}\n--- screen\n{}\n", args, path.display(), want.trim_end_matches('\n'), shown)),
            Err(_) => self.mismatches.push(format!("`cargo {}`: no snapshot {} (HAUL_UPDATE_SNAPSHOTS=1 writes it):\n{}\n", args, path.display(), shown)),
        }
    }

    fn finish(self) {
        assert!(self.mismatches.is_empty(), "{} screen(s) differ from their snapshots:\n\n{}", self.mismatches.len(), self.mismatches.join("\n"));
    }
}

/// `text` with what varies between runs replaced (see the module notes).
fn normalize(text: &str, sb: &Sandbox) -> String {
    let text = text.replace(&sb.home.display().to_string(), "<home>").replace(&sb.dir.display().to_string(), "<sandbox>");
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let boundary = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '.' || chars[i - 1] == '_');
        if boundary && chars[i].is_ascii_digit() {
            if let Some((len, placeholder)) = quantity(&chars[i..]) {
                out.push_str(placeholder);
                i += len;
                continue;
            }
        }
        // A build directory or unit id: `-` and sixteen hex digits.
        if chars[i] == '-' && chars.len() >= i + 17 && chars[i + 1..i + 17].iter().all(|c| c.is_ascii_hexdigit()) && chars.get(i + 17).map_or(true, |c| !c.is_ascii_alphanumeric()) {
            out.push_str("-<id>");
            i += 17;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// A duration (`45ms`, `1.84s`, `2m03.5s`) or a size (`12 B`, `5.6 MiB`) at the start of `s`:
/// its length and placeholder.
fn quantity(s: &[char]) -> Option<(usize, &'static str)> {
    let number = |from: usize| -> usize {
        let mut j = from;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
        }
        if j + 1 < s.len() && s[j] == '.' && s[j + 1].is_ascii_digit() {
            j += 1;
            while j < s.len() && s[j].is_ascii_digit() {
                j += 1;
            }
        }
        j
    };
    let ends = |j: usize| s.get(j).map_or(true, |c| !c.is_alphanumeric());
    let has = |j: usize, unit: &str| unit.chars().enumerate().all(|(k, u)| s.get(j + k) == Some(&u)) && ends(j + unit.len());
    let j = number(0);
    // Minutes and seconds.
    if s.get(j) == Some(&'m') && s.get(j + 1).is_some_and(|c| c.is_ascii_digit()) {
        let k = number(j + 1);
        if has(k, "s") {
            return Some((k + 1, "<time>"));
        }
    }
    for unit in ["ms", "s"] {
        if has(j, unit) {
            return Some((j + unit.len(), "<time>"));
        }
    }
    if s.get(j) == Some(&' ') {
        // What a toolchain's own files add to a count of files removed differs between hosts.
        if has(j + 1, "files") {
            return Some((j + 6, "<count> files"));
        }
        for unit in ["KiB", "MiB", "GiB", "TiB", "B"] {
            if has(j + 1, unit) {
                return Some((j + 1 + unit.len(), "<size>"));
            }
        }
    }
    None
}

#[test]
fn placeholders_replace_only_what_varies() {
    let sb = Sandbox::new("normalize");
    let line = format!("✓ Built 1.84s · 3 compiled · 45ms · 2m03.5s · 5.6 MiB · 12 B · {}/cache/builds/app-0123456789abcdef/debug/app · v1.2.3 · x86_64 · 10 warnings · 174 files", sb.home.display());
    assert_eq!(normalize(&line, &sb), "✓ Built <time> · 3 compiled · <time> · <time> · <size> · <size> · <home>/cache/builds/app-<id>/debug/app · v1.2.3 · x86_64 · 10 warnings · <count> files");
}

#[test]
fn every_screen_matches_its_snapshot() {
    let sb = Sandbox::new("screens");
    let reg = Registry::start();
    reg.publish(&Crate::lib("greet", "1.0.0", "pub fn greet() -> &'static str {\n    \"hello\"\n}\n"));
    reg.publish(&Crate::bin("hello", "0.3.0", "hello from an installed program"));
    reg.serve_crates_io_for(&sb.home);
    let p = sb.dir.join("app");
    sb.file("app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ngreet = \"1\"\n");
    sb.file("app/rust-toolchain.toml", &format!("[toolchain]\nchannel = \"{}\"\n", TOOLCHAIN));
    let main = |body: &str| sb.file("app/src/main.rs", &format!("fn main() {{\n{}}}\n\n#[test]\nfn greets() {{\n    assert_eq!(greet::greet(), \"hello\");\n}}\n", body));
    main("    println!(\"{}\", greet::greet());\n");
    let mut s = Screens::new(&sb);
    s.check("help", &p, "build --help");
    s.check("build", &p, "build");
    s.check("build-nothing-to-do", &p, "build");
    main("    let unused = 1;\n    println!(\"{}\", greet::greet());\n");
    s.check("build-warning", &p, "build");
    main("    println!(\"{}\", greet::greeting());\n");
    s.check("build-error", &p, "build");
    main("    println!(\"{}\", greet::greet());\n");
    s.check("check", &p, "check");
    s.check("run", &p, "run");
    s.check("test", &p, "test");
    s.check("doc", &p, "doc --no-deps");
    main("    let v = vec![1];\n    if v.len() == 0 {\n        println!(\"{}\", greet::greet());\n    }\n");
    s.check("clippy-warning", &p, "clippy");
    s.check("clean", &p, "clean");
    s.check("install", &p, "install hello");
    s.check("uninstall", &p, "uninstall hello");
    s.finish();
}

#[test]
fn every_screen_of_commands_that_do_not_build_matches_its_snapshot() {
    let sb = Sandbox::new("screens-manifest");
    let reg = Registry::start();
    reg.publish(&Crate::lib("greet", "1.0.0", "pub fn greet() -> &'static str {\n    \"hello\"\n}\n"));
    reg.publish(&Crate::lib("greet", "1.1.0", "pub fn greet() -> &'static str {\n    \"hello again\"\n}\n"));
    reg.serve_crates_io_for(&sb.home);
    sb.file("rust-toolchain.toml", &format!("[toolchain]\nchannel = \"{}\"\n", TOOLCHAIN));
    let mut s = Screens::new(&sb);
    s.check("new", &sb.dir, "new --vcs none tool");
    let p = sb.dir.join("tool");
    s.check("add", &p, "add greet@=1.0.0");
    s.check("generate-lockfile", &p, "generate-lockfile");
    s.check("fetch", &p, "fetch");
    sb.file("tool/Cargo.toml", &fs::read_to_string(p.join("Cargo.toml")).unwrap().replace("\"=1.0.0\"", "\"1\""));
    s.check("update", &p, "update");
    s.check("update-nothing-to-do", &p, "update");
    s.check("tree", &p, "tree");
    s.check("remove", &p, "remove greet");
    s.check("package", &p, "package --allow-dirty --no-verify");
    fs::create_dir_all(sb.dir.join("lib-here")).unwrap();
    s.check("init", &sb.dir.join("lib-here"), "init --lib --vcs none");
    s.finish();
}
