//! Pinned workloads: definition, one-time fetch, checkouts and synthetic edits.

use crate::sealed::Sealed;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct Edit {
    pub file: String,
    pub find: String,
    pub replace: String,
}

impl Edit {
    /// Rewrites `file` under `root` from its original content, with edit number `n`.
    pub fn apply(&self, root: &Path, original: &str, n: u64) -> Result<(), String> {
        let edited = original.replacen(&self.find, &self.replace.replace("@N@", &n.to_string()), 1);
        if edited == original {
            return Err(format!("edit of {} did not change anything", self.file));
        }
        fs::write(root.join(&self.file), edited).map_err(|e| format!("cannot edit {}: {}", self.file, e))
    }
}

#[derive(Debug, Clone)]
pub enum Source {
    /// This repository.
    SelfRepo,
    Git(String),
}

#[derive(Debug, Clone)]
pub struct Workload {
    pub name: String,
    pub description: String,
    pub source: Source,
    pub rev: String,
    /// Package selection and the like, appended to every Cargo command.
    pub args: Vec<String>,
    /// What `haul-bench verify` passes to `cargo test` in place of `args`: the build may
    /// select a package that has no tests of its own.
    pub test_args: Vec<String>,
    pub quick: bool,
    pub leaf: Edit,
    pub core: Edit,
}

fn strings(t: &toml::Table, key: &str, name: &str) -> Result<Option<Vec<String>>, String> {
    let Some(v) = t.get(key) else { return Ok(None) };
    let bad = || format!("workload {}: `{}` must be a list of strings", name, key);
    v.as_array().ok_or_else(bad)?.iter().map(|x| x.as_str().map(str::to_string).ok_or_else(bad)).collect::<Result<_, _>>().map(Some)
}

fn edit(t: &toml::Table, key: &str, name: &str) -> Result<Edit, String> {
    let e = t.get(key).and_then(|v| v.as_table()).ok_or_else(|| format!("workload {}: `{}` edit is missing", name, key))?;
    let s = |k: &str| e.get(k).and_then(|v| v.as_str()).map(str::to_string).ok_or_else(|| format!("workload {}: `{}.{}` is missing", name, key, k));
    let edit = Edit { file: s("file")?, find: s("find")?, replace: s("replace")? };
    if !edit.replace.contains("@N@") {
        return Err(format!("workload {}: `{}.replace` must contain @N@ so that every edit is new code", name, key));
    }
    Ok(edit)
}

pub fn load(path: &Path) -> Result<Vec<Workload>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let doc: toml::Table = text.parse().map_err(|e| format!("invalid TOML in {}: {}", path.display(), e))?;
    let mut out = Vec::new();
    for w in doc.get("workload").and_then(|v| v.as_array()).ok_or_else(|| format!("{} defines no [[workload]]", path.display()))? {
        let t = w.as_table().ok_or("each [[workload]] must be a table")?;
        let s = |k: &str| t.get(k).and_then(|v| v.as_str()).map(str::to_string);
        let name = s("name").ok_or("a workload has no name")?;
        let source = match (s("source").as_deref(), s("git")) {
            (Some("self"), None) => Source::SelfRepo,
            (None, Some(url)) => Source::Git(url),
            _ => return Err(format!("workload {}: set exactly one of `source = \"self\"` or `git = URL`", name)),
        };
        out.push(Workload {
            rev: s("rev").ok_or_else(|| format!("workload {}: `rev` is missing", name))?,
            description: s("description").unwrap_or_default(),
            args: strings(t, "args", &name)?.unwrap_or_default(),
            test_args: match strings(t, "test-args", &name)? {
                Some(a) => a,
                None => strings(t, "args", &name)?.unwrap_or_default(),
            },
            quick: t.get("quick").and_then(|v| v.as_bool()).unwrap_or(false),
            leaf: edit(t, "leaf", &name)?,
            core: edit(t, "core", &name)?,
            source,
            name,
        });
    }
    Ok(out)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).stdin(Stdio::null()).output().map_err(|e| format!("cannot run git: {}", e))?;
    if !out.status.success() {
        return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The git repository holding a workload's pinned commit.
pub fn source_repo(work: &Path, w: &Workload) -> PathBuf {
    work.join("sources").join(&w.name)
}

/// Extracts the pinned commit into `into` (which must not exist yet).
pub fn checkout(work: &Path, w: &Workload, into: &Path) -> Result<(), String> {
    fs::create_dir_all(into).map_err(|e| format!("cannot create {}: {}", into.display(), e))?;
    let mut archive = Command::new("git").arg("-C").arg(source_repo(work, w)).args(["archive", &w.rev]).stdout(Stdio::piped()).spawn().map_err(|e| format!("git archive: {}", e))?;
    let stream = archive.stdout.take().ok_or("git archive produced no output")?;
    let untar = Command::new("tar").arg("-x").arg("-C").arg(into).stdin(stream).status().map_err(|e| format!("tar: {}", e))?;
    let archived = archive.wait().map_err(|e| format!("git archive: {}", e))?;
    if !archived.success() || !untar.success() {
        return Err(format!("could not extract {}@{} (run `haul-bench fetch` first)", w.name, w.rev));
    }
    Ok(())
}

fn marker(work: &Path, w: &Workload) -> PathBuf {
    work.join("sources").join(format!("{}.fetched", w.name))
}

pub fn is_fetched(work: &Path, w: &Workload) -> bool {
    fs::read_to_string(marker(work, w)).map(|r| r.trim() == w.rev).unwrap_or(false)
}

/// Clones the pinned commit, checks the edit anchors, and downloads every dependency in the
/// lockfile (dev-dependencies included) into the sealed Cargo home. The only step that uses the network.
pub fn fetch(work: &Path, repo: &Path, w: &Workload, sealed: &Sealed) -> Result<(), String> {
    let src = source_repo(work, w);
    if !src.join(".git").exists() && !src.join("HEAD").exists() {
        fs::create_dir_all(src.parent().unwrap_or(work)).map_err(|e| e.to_string())?;
        match &w.source {
            Source::SelfRepo => {
                let status = Command::new("git").args(["clone", "--quiet", "--no-checkout"]).arg(repo).arg(&src).status().map_err(|e| format!("git clone: {}", e))?;
                if !status.success() {
                    return Err(format!("could not clone {}", repo.display()));
                }
            }
            Source::Git(url) => {
                fs::create_dir_all(&src).map_err(|e| e.to_string())?;
                git(&src, &["init", "--quiet"])?;
                eprintln!("haul-bench: fetching {} {}", url, w.rev);
                git(&src, &["fetch", "--quiet", "--depth", "1", url, &w.rev])?;
            }
        }
    }
    git(&src, &["cat-file", "-e", &format!("{}^{{commit}}", w.rev)]).map_err(|_| format!("{}: commit {} is not available in {}", w.name, w.rev, src.display()))?;
    let tree = work.join("sources").join(format!("{}.tree", w.name));
    let _ = fs::remove_dir_all(&tree);
    checkout(work, w, &tree)?;
    for e in [&w.leaf, &w.core] {
        let text = fs::read_to_string(tree.join(&e.file)).map_err(|err| format!("{}: cannot read {}: {}", w.name, e.file, err))?;
        let n = text.matches(&e.find).count();
        if n != 1 {
            return Err(format!("{}: the edit anchor occurs {} times in {} (must be exactly once): {:?}", w.name, n, e.file, e.find));
        }
    }
    eprintln!("haul-bench: downloading dependencies of {}", w.name);
    let mut cmd = sealed.command(&sealed.cargo, &tree, &tree.join("target"), true);
    cmd.args(["fetch", "--locked"]).stdout(Stdio::null());
    let status = cmd.status().map_err(|e| format!("cargo fetch: {}", e))?;
    if !status.success() {
        return Err(format!("{}: cargo fetch --locked failed", w.name));
    }
    let _ = fs::remove_dir_all(&tree);
    fs::write(marker(work, w), &w.rev).map_err(|e| e.to_string())
}
