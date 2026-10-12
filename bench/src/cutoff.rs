//! The early cut-off gate: restoring a crate's dependents instead of compiling them, after an
//! edit that leaves its interface hash unchanged, must give exactly what compiling them gives.
//!
//! For random edits inside the function bodies of a workload's own crates (a literal changed,
//! or a macro call added in front of a statement, on the same line): (A) the
//! workload is built, the edit applied and the workload built again with the same Haul state,
//! as in a developer's loop; (B) the edited source is built from nothing with empty Haul state,
//! at the same path. Every library, metadata file and executable must be byte-identical.
//!
//! An incremental loop runs alongside (its own target directory and Haul state, default settings):
//! after each edit it builds, then appends a line to every source file the workload compiles and
//! builds again (every workspace crate recompiles incrementally against the metadata of crates
//! early cut-off kept), then restores them and builds again. Every one of those builds must
//! succeed: rustc verifies, in an incremental session, what it reads from a crate whose hash
//! did not change.
//!
//! Both byte-compared sides build without incremental compilation, without rustc's parallel
//! frontend and without the wild linker: neither of those two produces the same bytes twice (under several
//! threads rustc numbers nested statics in a varying order; wild sets one section's entry size
//! in a varying way), which would make byte comparison meaningless, and neither changes what
//! the cut-off restores.

use crate::workload::{self, Workload};
use crate::{fail, Ctx};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One synthetic edit: on `line` of `file`, `from` becomes `to`.
#[derive(Debug, Clone)]
struct LiteralEdit {
    file: String,
    line: usize,
    from: String,
    to: String,
}

/// Deterministic choice of edits for a given seed (SplitMix64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
    }
}

/// The workload's own sources that its build compiles: the inputs rustc's dep-info lists for
/// its units, inside the checkout (registry sources are outside it), without tests, benches,
/// examples and build scripts.
fn compiled_sources(co: &Path, target: &Path) -> Vec<String> {
    let mut files = std::collections::BTreeSet::new();
    for e in fs::read_dir(target.join("debug/deps")).into_iter().flatten().flatten() {
        if e.path().extension().map_or(true, |x| x != "d") {
            continue;
        }
        let text = fs::read_to_string(e.path()).unwrap_or_default();
        let Some(rule) = text.lines().find(|l| !l.starts_with('#') && l.contains(": ")) else { continue };
        let deps = rule.split_once(": ").map(|(_, d)| d).unwrap_or("");
        for f in deps.replace("\\ ", "\u{0}").split_whitespace().map(|f| f.replace('\u{0}', " ")) {
            let path = co.join(&f);
            let Ok(rel) = fs::canonicalize(&path).map_err(|_| ()).and_then(|p| p.strip_prefix(fs::canonicalize(co).map_err(|_| ())?).map(Path::to_path_buf).map_err(|_| ())) else { continue };
            let rel = rel.to_string_lossy().into_owned();
            let excluded = ["tests/", "benches/", "examples/"].iter().any(|d| rel.starts_with(d) || rel.contains(&format!("/{}", d)));
            if rel.ends_with(".rs") && !excluded && !rel.ends_with("build.rs") {
                files.insert(rel);
            }
        }
    }
    files.into_iter().collect()
}

/// Edits inside function bodies of `file`: a string literal made longer, an integer made one
/// larger, or `debug_assert!(true);` put in front of a statement, on lines indented as
/// statements are, outside test modules, constants and statics, attributes and comments. Edits
/// that turn out not to compile are skipped later.
fn literal_edits(co: &Path, file: &str) -> Vec<LiteralEdit> {
    let text = fs::read_to_string(co.join(file)).unwrap_or_default();
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let s = line.trim_start();
        if s.starts_with("#[cfg(test)]") {
            break;
        }
        let skipped = ["//", "#", "const ", "static ", "pub const ", "pub static ", "pub(crate) const ", "pub(crate) static "];
        if !line.starts_with("        ") || skipped.iter().any(|p| s.starts_with(p)) {
            continue;
        }
        // A macro call in front of a statement.
        if s.ends_with(';') && !s.starts_with('}') && !s.starts_with("let") && !s.starts_with("return") && !s.starts_with("fn ") && !s.starts_with("use ") && !s.starts_with("type ") {
            out.push(LiteralEdit { file: file.to_string(), line: i + 1, from: s.to_string(), to: format!("debug_assert!(true); {}", s) });
        }
        // String literals of words and spaces.
        let bytes = line.as_bytes();
        let mut j = 0;
        while let Some(start) = line[j..].find('"').map(|k| k + j) {
            let Some(end) = line[start + 1..].find('"').map(|k| k + start + 1) else { break };
            let inner = &line[start + 1..end];
            let words = inner.len() >= 3 && inner.len() <= 30 && inner.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && inner.chars().all(|c| c.is_ascii_alphabetic() || c == ' ');
            let escaped = start > 0 && bytes[start - 1] == b'\\';
            if words && !escaped {
                out.push(LiteralEdit { file: file.to_string(), line: i + 1, from: format!("\"{}\"", inner), to: format!("\"{} haul\"", inner) });
            }
            j = end + 1;
        }
        // Integer literals, where they are values (not array lengths, patterns or ranges).
        if !line.contains('[') && !line.contains("=>") && !line.contains("..") && !line.contains("match") {
            for (k, tok) in tokens(line) {
                let isolated = |at: Option<char>| at.map_or(true, |c| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'));
                let (before, after) = (line[..k].chars().last(), line[k + tok.len()..].chars().next());
                if isolated(before) && isolated(after) && tok.len() <= 4 && !tok.starts_with('0') {
                    if let Ok(n) = tok.parse::<u32>() {
                        out.push(LiteralEdit { file: file.to_string(), line: i + 1, from: tok.to_string(), to: (n + 1).to_string() });
                    }
                }
            }
        }
    }
    out
}

/// Runs of ASCII digits in `line`, with their byte offsets.
fn tokens(line: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            out.push((start, &line[start..i]));
        } else {
            i += 1;
        }
    }
    out
}

impl LiteralEdit {
    /// The file's text with this edit applied, or `None` when the line no longer holds `from`.
    fn apply(&self, original: &str) -> Option<String> {
        let mut lines: Vec<&str> = original.split('\n').collect();
        let line = lines.get(self.line - 1)?;
        if !line.contains(&self.from) {
            return None;
        }
        let edited = line.replacen(&self.from, &self.to, 1);
        lines[self.line - 1] = &edited;
        Some(lines.join("\n"))
    }
}

/// The outputs to compare: libraries, metadata and executables, by path below the target
/// directory (build scripts' own directories, incremental state and fingerprints aside).
fn outputs(target: &Path) -> BTreeMap<String, PathBuf> {
    let mut out = BTreeMap::new();
    let mut stack = vec![target.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(md) = fs::symlink_metadata(&p) else { continue };
            if md.is_dir() {
                if !matches!(name.as_str(), "build" | "incremental" | ".fingerprint") {
                    stack.push(p);
                }
                continue;
            }
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                md.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = name.ends_with(".exe");
            if md.is_file() && (name.ends_with(".rlib") || name.ends_with(".rmeta") || executable) {
                out.insert(p.strip_prefix(target).unwrap_or(&p).to_string_lossy().into_owned(), p);
            }
        }
    }
    out
}

/// What the latest build in a Haul home did per unit: (crate name, cache result, or `relinked`
/// for a unit linked again from its kept objects).
fn unit_results(home: &Path) -> Vec<(String, String)> {
    let runs: Vec<PathBuf> = fs::read_dir(home.join("workspaces")).into_iter().flatten().flatten().flat_map(|ws| fs::read_dir(ws.path().join("events")).into_iter().flatten().flatten().map(|e| e.path())).collect();
    let Some(latest) = runs.into_iter().max_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok()) else { return Vec::new() };
    let mut out: Vec<(String, String)> = fs::read_dir(latest)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| serde_json::from_slice::<Value>(&fs::read(e.path()).ok()?).ok())
        .filter(|ev| ev["kind"] == "compile")
        .map(|ev| {
            let args: Vec<&str> = ev["args"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let name = args.iter().position(|a| *a == "--crate-name").and_then(|i| args.get(i + 1)).unwrap_or(&"?").to_string();
            let result = if ev["link"] == "relinked" { "relinked" } else { ev["cache"]["result"].as_str().unwrap_or("") };
            (name, result.to_string())
        })
        .collect();
    out.sort();
    out
}

/// Runs the gate: `edits` edits per workload, chosen with `seed`. Returns the report and
/// whether every edit passed.
pub fn gate(ctx: &Ctx, workloads: &[Workload], edits: usize, seed: u64) -> (Value, bool) {
    let drivers = ctx.drivers.as_ref().unwrap_or_else(|e| fail(&format!("the cut-off gate needs Haul's compiler driver, which could not be built: {}", e)));
    let mut all_pass = true;
    let mut report = Vec::new();
    for w in workloads {
        let base = ctx.work.join("cutoff").join(&w.name);
        let _ = fs::remove_dir_all(&base);
        let (co, target, target_a) = (base.join("co"), base.join("target"), base.join("target-a"));
        workload::checkout(&ctx.work, w, &co).unwrap_or_else(|e| fail(&e));
        let fresh_home = |name: &str| -> PathBuf {
            let home = base.join(name);
            let _ = fs::remove_dir_all(&home);
            crate::copy_tree(drivers, &home.join("drivers")).unwrap_or_else(|e| fail(&format!("cannot copy Haul's compiler driver: {}", e)));
            if name != "home-incremental" {
                fs::write(home.join("config.toml"), "[fast]\nfrontend-threads = \"off\"\nlinker = \"off\"\n").unwrap_or_else(|e| fail(&e.to_string()));
            }
            home
        };
        let inc_target = base.join("target-incremental");
        // A build of the incremental loop: its stderr's error lines (and any panic) when it fails.
        let build_incremental = |home: &Path| -> Result<(), String> {
            let out = ctx.sealed.command(&ctx.haul, &co, &inc_target, false).env("HAUL_HOME", home).arg("build").args(&w.args).output().map_err(|e| e.to_string())?;
            let err = String::from_utf8_lossy(&out.stderr);
            if out.status.success() && !err.contains("panicked") && !err.contains("driver failed") {
                Ok(())
            } else {
                Err(err.lines().filter(|l| l.starts_with("error") || l.contains("panicked") || l.contains("driver failed")).take(3).collect::<Vec<_>>().join(" | "))
            }
        };
        let build = |home: &Path| -> Result<(), String> {
            let out = ctx.sealed.command(&ctx.haul, &co, &target, false).env("HAUL_HOME", home).env("CARGO_INCREMENTAL", "0").arg("build").args(&w.args).output().map_err(|e| e.to_string())?;
            if out.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&out.stderr).lines().filter(|l| l.starts_with("error")).take(3).collect::<Vec<_>>().join(" | "))
            }
        };
        println!("\n{} @ {}", w.name, &w.rev[..w.rev.len().min(10)]);
        let home_a = fresh_home("home-a");
        build(&home_a).unwrap_or_else(|e| fail(&format!("[{}] the workload does not build: {}", w.name, e)));
        let home_inc = fresh_home("home-incremental");
        build_incremental(&home_inc).unwrap_or_else(|e| fail(&format!("[{}] the workload does not build incrementally: {}", w.name, e)));
        let sources = compiled_sources(&co, &target);
        let mut candidates: Vec<LiteralEdit> = sources.iter().flat_map(|f| literal_edits(&co, f)).collect();
        if candidates.is_empty() {
            fail(&format!("[{}] no literal to edit in the workload's own sources", w.name));
        }
        Rng(seed ^ w.name.bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64))).shuffle(&mut candidates);
        let mut rows = Vec::new();
        let (mut done, mut skipped) = (0usize, 0usize);
        for edit in candidates {
            if done == edits {
                break;
            }
            let path = co.join(&edit.file);
            let original = fs::read_to_string(&path).unwrap_or_else(|e| fail(&format!("cannot read {}: {}", path.display(), e)));
            let Some(edited) = edit.apply(&original) else { continue };
            fs::write(&path, &edited).unwrap_or_else(|e| fail(&e.to_string()));
            let label = format!("{}:{} {} → {}", edit.file, edit.line, edit.from, edit.to);
            // A: the developer's loop, with the state the previous builds left.
            if build(&home_a).is_err() {
                skipped += 1;
                fs::write(&path, &original).unwrap_or_else(|e| fail(&e.to_string()));
                continue;
            }
            // The incremental loop: the edit, then every compiled source touched and restored.
            let mut differ: Vec<String> = Vec::new();
            let touched = || -> Result<(), String> {
                build_incremental(&home_inc).map_err(|e| format!("incremental build after the edit: {}", e))?;
                let originals: Vec<(PathBuf, String)> = sources.iter().map(|f| co.join(f)).filter_map(|p| fs::read_to_string(&p).ok().map(|t| (p, t))).collect();
                for (p, t) in &originals {
                    fs::write(p, format!("{}\n// haul-bench\n", t)).map_err(|e| e.to_string())?;
                }
                let touched = build_incremental(&home_inc).map_err(|e| format!("incremental build of every crate: {}", e));
                for (p, t) in &originals {
                    fs::write(p, t).map_err(|e| e.to_string())?;
                }
                touched?;
                build_incremental(&home_inc).map_err(|e| format!("incremental build after restoring them: {}", e))
            };
            if let Err(e) = touched() {
                differ.push(e);
            }
            let units = unit_results(&home_a);
            let restored: Vec<&str> = units.iter().filter(|(_, r)| r == "hit").map(|(n, _)| n.as_str()).collect();
            let relinked: Vec<&str> = units.iter().filter(|(_, r)| r == "relinked").map(|(n, _)| n.as_str()).collect();
            let compiled: Vec<&str> = units.iter().filter(|(_, r)| r != "hit" && r != "relinked").map(|(n, _)| n.as_str()).collect();
            // B: the same source from nothing, at the same path.
            fs::rename(&target, &target_a).unwrap_or_else(|e| fail(&e.to_string()));
            let home_b = fresh_home("home-b");
            let b = build(&home_b);
            let (a_out, b_out) = (outputs(&target_a), outputs(&target));
            match &b {
                Err(e) => differ.push(format!("the edited source does not build from nothing: {}", e)),
                Ok(()) => {
                    for (rel, pb) in &b_out {
                        match a_out.get(rel) {
                            Some(pa) if fs::read(pa).ok() == fs::read(pb).ok() => {}
                            Some(_) => differ.push(rel.clone()),
                            None => differ.push(format!("{} (only built from nothing)", rel)),
                        }
                    }
                    differ.extend(a_out.keys().filter(|rel| !b_out.contains_key(*rel)).map(|rel| format!("{} (only in the loop)", rel)));
                }
            }
            let _ = fs::remove_dir_all(&target);
            let _ = fs::remove_dir_all(&home_b);
            fs::rename(&target_a, &target).unwrap_or_else(|e| fail(&e.to_string()));
            fs::write(&path, &original).unwrap_or_else(|e| fail(&e.to_string()));
            let pass = differ.is_empty();
            all_pass &= pass;
            done += 1;
            println!("  {} {}", if pass { "SAME" } else { "DIFFERENT" }, label);
            let listed = |v: &[&str]| if v.is_empty() { "none".to_string() } else { v.join(", ") };
            println!("      restored {} · relinked {} · compiled {} · {} outputs compared", listed(&restored), listed(&relinked), compiled.join(", "), b_out.len());
            for d in differ.iter().take(10) {
                println!("      differs: {}", d);
            }
            rows.push(json!({ "file": edit.file, "line": edit.line, "from": edit.from, "to": edit.to, "restored": restored, "relinked": relinked, "compiled": compiled, "outputs": b_out.len(), "differ": differ }));
        }
        if done < edits {
            fail(&format!("[{}] only {} of {} edits compiled ({} did not)", w.name, done, edits, skipped));
        }
        let cut = rows.iter().filter(|r| r["restored"].as_array().is_some_and(|a| !a.is_empty())).count();
        println!("  {} edits ({} cut off, {} that did not compile skipped)", done, cut, skipped);
        report.push(json!({ "workload": w.name, "rev": w.rev, "edits": rows, "skipped": skipped }));
        let _ = fs::remove_dir_all(&base);
    }
    (json!(report), all_pass)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_stay_inside_bodies_and_change_one_literal() {
        let dir = std::env::temp_dir().join(format!("haul-bench-cutoff-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let src = "const LIMIT: u32 = 10;\n\nfn f(x: u32) -> String {\n        let a = [0u8; 16];\n        let n = x + 42;\n        match n { 7 => {} _ => {} }\n        // \"not this\"\n        log(n);\n        format!(\"value is\", n)\n}\n\n#[cfg(test)]\nmod tests {\n        let t = \"in tests\";\n}\n";
        fs::write(dir.join("lib.rs"), src).unwrap();
        let edits = literal_edits(&dir, "lib.rs");
        let found: Vec<(usize, &str, &str)> = edits.iter().map(|e| (e.line, e.from.as_str(), e.to.as_str())).collect();
        assert_eq!(found, vec![(5, "42", "43"), (8, "log(n);", "debug_assert!(true); log(n);"), (9, "\"value is\"", "\"value is haul\"")]);
        let applied = edits[0].apply(src).unwrap();
        assert_eq!(applied.lines().nth(4), Some("        let n = x + 43;"));
        assert_eq!(applied.lines().count(), src.lines().count());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_same_seed_chooses_the_same_edits() {
        let mut a: Vec<u32> = (0..50).collect();
        let mut b = a.clone();
        Rng(7).shuffle(&mut a);
        Rng(7).shuffle(&mut b);
        assert_eq!(a, b);
        let mut c: Vec<u32> = (0..50).collect();
        Rng(8).shuffle(&mut c);
        assert_ne!(a, c);
    }
}
