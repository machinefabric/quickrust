//! Haul's global artifact cache (fast mode).
//!
//! The rustc wrapper looks a compilation up before running rustc. On a hit it restores the
//! outputs into Cargo's output directory and replays rustc's stderr, so Cargo sees exactly
//! what a real compilation would have produced, minus the time. This first version caches
//! only units whose sources cannot change under a name (registry and git dependencies) and
//! that read nothing a build script produced outside `OUT_DIR`:
//!
//! - **key, level 1**: cache format version; rustc identity; every argument, with output
//!   and search paths removed and each `--extern` replaced by the content hash of the
//!   artifact it names; Cargo's `CARGO_*` variables; the content of `OUT_DIR`;
//! - **key, level 2**: level 1 plus the values of the environment variables rustc reported
//!   reading (`# env-dep:` in its dep-info), which are only known after one compilation;
//! - **hit**: outputs are copied (never linked: a later build may touch them) and so get a
//!   current modification time, which Cargo's staleness check expects; the dep-info and
//!   the replayed stderr have the output and `OUT_DIR` paths of the cached build replaced
//!   by this build's, because Cargo derives its own fingerprints and its pipelining
//!   (`artifact` notices) from them, and stores the diagnostics it replays as warnings.
//!
//! Any cache problem means "compile normally": the cache can only ever save time.
//!
//! Known difference: a crate that `include!`s files its build script generated is keyed by
//! the content of `OUT_DIR`, not its path, so a unit restored into another target directory
//! names the first one in panic locations and debug information for that generated code.

use crate::util;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub const ENV_CACHE_DIR: &str = "HAUL_CACHE_DIR";
const FORMAT: &str = "haul-artifact-cache-1";

/// One cacheable compilation, prepared before rustc runs.
pub struct Job {
    root: PathBuf,
    key1: String,
    out_dir: PathBuf,
    stem: String,
    out_dir_env: Option<String>,
}

pub enum Lookup {
    /// Outputs restored and stderr to replay.
    Hit { stderr: Vec<u8>, entry: String },
    Miss,
}

fn value_of<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Option<&'a str> {
    let a = &args[*i];
    if a == flag {
        *i += 1;
        return args.get(*i).map(String::as_str);
    }
    a.strip_prefix(flag).map(|r| r.strip_prefix('=').unwrap_or(r))
}

fn file_identity(p: &Path) -> Option<String> {
    let md = fs::metadata(p).ok()?;
    Some(format!("{}:{}:{}", util::path_str(&util::canon(p)), md.len(), util::mtime_ns(&md)))
}

/// Content digest of a directory tree: relative names and file contents, sorted.
fn tree_digest(root: &Path) -> Option<String> {
    let mut parts = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).ok()?.flatten() {
            let md = fs::symlink_metadata(e.path()).ok()?;
            let rel = util::path_str(e.path().strip_prefix(root).ok()?);
            if md.is_dir() {
                stack.push(e.path());
            } else {
                parts.push(format!("{}={}", rel, util::sha256_file(&e.path()).ok()?));
            }
        }
    }
    parts.sort();
    Some(util::digest_parts(parts))
}

/// Prepares a lookup, or says why this compilation is not cached.
pub fn prepare(rustc: &str, args: &[String]) -> Result<Job, String> {
    let root = std::env::var_os(ENV_CACHE_DIR).filter(|v| !v.is_empty()).map(PathBuf::from).ok_or("the artifact cache is off")?;
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| "not a Cargo unit")?;
    if !crate::wrapper::is_external_source(Path::new(&manifest_dir)) {
        return Err("workspace sources can change under the same name".into());
    }
    let mut key = vec![FORMAT.to_string(), file_identity(Path::new(rustc)).ok_or("rustc cannot be identified")?];
    let (mut crate_name, mut out_dir, mut extra, mut types) = (None, None, String::new(), Vec::new());
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a.starts_with('@') {
            return Err("arguments come from an argument file".into());
        }
        if let Some(v) = value_of(args, &mut i, "--out-dir") {
            out_dir = Some(PathBuf::from(v));
        } else if let Some(v) = value_of(args, &mut i, "--crate-name") {
            crate_name = Some(v.to_string());
            key.push(format!("--crate-name={}", v));
        } else if let Some(v) = value_of(args, &mut i, "--crate-type") {
            types.extend(v.split(',').map(str::to_string));
            key.push(format!("--crate-type={}", v));
        } else if let Some(v) = value_of(args, &mut i, "--extern") {
            // The artifact a dependency resolves to, by content: identical inputs, identical output.
            match v.split_once('=') {
                Some((name, path)) => key.push(format!("--extern={}=sha:{}", name, util::sha256_file(Path::new(path)).map_err(|e| format!("dependency {} is unreadable: {}", path, e))?)),
                None => key.push(format!("--extern={}", v)),
            }
        } else if let Some(v) = value_of(args, &mut i, "-L") {
            // Where to look for dependencies is not what is compiled; native search paths are.
            if !v.starts_with("dependency=") {
                return Err("links against native libraries".into());
            }
        } else if value_of(args, &mut i, "-l").is_some() || a.starts_with("--print") {
            return Err("links native libraries or prints compiler information".into());
        } else if let Some(v) = value_of(args, &mut i, "-C") {
            if v.starts_with("incremental") {
                return Err("incremental compilation".into());
            }
            if let Some(e) = v.strip_prefix("extra-filename=") {
                extra = e.to_string();
            }
            key.push(format!("-C{}", v));
        } else if value_of(args, &mut i, "--diagnostic-width").is_some() {
            // Terminal width of the build that ran first; never part of the identity.
        } else {
            key.push(args[i].clone());
        }
        i += 1;
    }
    let crate_name = crate_name.ok_or("no crate name")?;
    let out_dir = out_dir.ok_or("no output directory")?;
    if let Some(t) = types.iter().find(|t| !matches!(t.as_str(), "lib" | "rlib" | "proc-macro" | "bin")) {
        return Err(format!("crate type {} is not cached", t));
    }
    if types.iter().any(|t| t == "bin") && !crate_name.starts_with("build_script_") {
        return Err("binaries other than build scripts are not cached".into());
    }
    // What Cargo sets for the unit itself. Variables describing where this particular build
    // lives (target directory, Cargo home, current directory) are left out: when rustc or a
    // macro reads one of them through `env!`, dep-info reports it and level 2 keys on it.
    const UNIT_ENV: &[&str] = &["CARGO_PKG_", "CARGO_CRATE_NAME", "CARGO_MANIFEST_DIR", "CARGO_MANIFEST_PATH", "CARGO_MANIFEST_LINKS", "CARGO_PRIMARY_PACKAGE", "CARGO_BIN_NAME", "CARGO_CFG_"];
    let mut env: Vec<String> = std::env::vars().filter(|(k, _)| UNIT_ENV.iter().any(|p| k.starts_with(p))).map(|(k, v)| format!("{}={}", k, v)).collect();
    env.sort();
    key.extend(env);
    let out_dir_env = std::env::var("OUT_DIR").ok();
    if let Some(o) = &out_dir_env {
        key.push(format!("OUT_DIR=sha:{}", tree_digest(Path::new(o)).ok_or("OUT_DIR is unreadable")?));
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    Ok(Job { root, key1: util::digest_parts(key), out_dir: cwd.join(out_dir), stem: format!("{}{}", crate_name, extra), out_dir_env })
}

fn env_digest(key1: &str, names: &[String]) -> String {
    util::digest_parts(std::iter::once(key1.to_string()).chain(names.iter().map(|n| {
        // `include!(concat!(env!("OUT_DIR"), …))` reads OUT_DIR: what matters is the directory's
        // content, already in level 1; its path differs in every target directory.
        if n == "OUT_DIR" {
            return "OUT_DIR=<content>".to_string();
        }
        format!("{}={}", n, std::env::var(n).map(|v| format!("set:{}", v)).unwrap_or_else(|_| "unset".into()))
    })))
}

/// Replaces the cached build's paths with this build's (longest first).
fn rewrite(text: &[u8], pairs: &[(String, String)]) -> Vec<u8> {
    let mut s = String::from_utf8_lossy(text).into_owned();
    for (from, to) in pairs {
        if !from.is_empty() && from != to {
            s = s.replace(from.as_str(), to);
        }
    }
    s.into_bytes()
}

impl Job {
    fn manifest(&self) -> PathBuf {
        self.root.join("m").join(format!("{}.json", self.key1))
    }

    fn entry_dir(&self, key2: &str) -> PathBuf {
        self.root.join("e").join(key2)
    }

    pub fn lookup(&self) -> Lookup {
        let Some(m) = util::read_json(&self.manifest()) else { return Lookup::Miss };
        let names: Vec<String> = m["env_deps"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
        let key2 = env_digest(&self.key1, &names);
        let dir = self.entry_dir(&key2);
        let Some(entry) = util::read_json(&dir.join("entry.json")) else { return Lookup::Miss };
        let pairs = vec![
            (util::vstr(&entry, "out_dir"), util::path_str(&self.out_dir)),
            (util::vstr(&entry, "out_dir_env"), self.out_dir_env.clone().unwrap_or_default()),
        ];
        let restore = || -> std::io::Result<Vec<u8>> {
            for f in entry["files"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                let bytes = fs::read(dir.join(f))?;
                let bytes = if f.ends_with(".d") { rewrite(&bytes, &pairs) } else { bytes };
                // A fresh file, so Cargo's mtime-based staleness check sees a new output.
                util::write_atomic_fast(&self.out_dir.join(f), &bytes)?;
                #[cfg(unix)]
                if entry["executable"].as_array().into_iter().flatten().any(|x| x.as_str() == Some(f)) {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(self.out_dir.join(f), fs::Permissions::from_mode(0o755))?;
                }
            }
            Ok(rewrite(&fs::read(dir.join("stderr")).unwrap_or_default(), &pairs))
        };
        match restore() {
            Ok(stderr) => Lookup::Hit { stderr, entry: key2 },
            // A damaged entry is a miss; the compilation that follows overwrites the outputs.
            Err(_) => Lookup::Miss,
        }
    }

    /// Stores the outputs of a successful compilation that started at `start_wall_ns`.
    pub fn store(&self, stderr: &[u8], start_wall_ns: u64) -> Result<(), String> {
        let dep_info = self.out_dir.join(format!("{}.d", self.stem));
        let text = fs::read_to_string(&dep_info).map_err(|e| format!("no dep-info: {}", e))?;
        let mut names: Vec<String> = text.lines().filter_map(|l| l.strip_prefix("# env-dep:")).map(|r| r.split_once('=').map(|(n, _)| n).unwrap_or(r).to_string()).collect();
        names.sort();
        names.dedup();
        let key2 = env_digest(&self.key1, &names);
        let dir = self.entry_dir(&key2);
        if dir.join("entry.json").exists() {
            return Ok(());
        }
        let candidates = [format!("lib{}.rlib", self.stem), format!("lib{}.rmeta", self.stem), format!("lib{}.so", self.stem), format!("lib{}.dylib", self.stem), format!("{}.dll", self.stem), self.stem.clone(), format!("{}.exe", self.stem), format!("{}.d", self.stem)];
        let tmp = self.root.join("tmp").join(format!("{}-{}", key2, std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        util::create_private_dir(&tmp).map_err(|e| e.to_string())?;
        let (mut files, mut executable) = (Vec::new(), Vec::new());
        for name in candidates {
            let p = self.out_dir.join(&name);
            let Ok(md) = fs::metadata(&p) else { continue };
            // Only what this compilation wrote; older files of the same name are not its output.
            if !md.is_file() || util::mtime_ns(&md) + 1_000_000_000 < start_wall_ns {
                continue;
            }
            fs::copy(&p, tmp.join(&name)).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if md.permissions().mode() & 0o111 != 0 {
                    executable.push(name.clone());
                }
            }
            files.push(name);
        }
        if !files.iter().any(|f| !f.ends_with(".d")) {
            let _ = fs::remove_dir_all(&tmp);
            return Err("the compilation left no output to cache".into());
        }
        fs::write(tmp.join("stderr"), stderr).map_err(|e| e.to_string())?;
        let entry = json!({ "files": files, "executable": executable, "out_dir": util::path_str(&self.out_dir), "out_dir_env": self.out_dir_env, "env_deps": names, "stored_at": util::iso8601(util::now_wall_ns()) });
        util::write_json(&tmp.join("entry.json"), &entry).map_err(|e| e.to_string())?;
        fs::create_dir_all(self.root.join("e")).map_err(|e| e.to_string())?;
        // Another build may have stored the same entry meanwhile; either copy is the same.
        if fs::rename(&tmp, &dir).is_err() {
            let _ = fs::remove_dir_all(&tmp);
        }
        util::write_json(&self.manifest(), &json!({ "env_deps": names })).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_replaces_the_cached_builds_paths() {
        let pairs = vec![("/a/target/debug/deps".to_string(), "/b/t/debug/deps".to_string()), (String::new(), "/ignored".to_string())];
        let out = rewrite(b"{\"artifact\":\"/a/target/debug/deps/libx.rmeta\"}\n", &pairs);
        assert_eq!(String::from_utf8(out).unwrap(), "{\"artifact\":\"/b/t/debug/deps/libx.rmeta\"}\n");
    }
}
