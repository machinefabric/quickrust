//! Remediation previews (FR-170).
//!
//! A remediation is a minimal, reversible edit to one file, bound to the exact
//! content it was generated against. Only changes to Haul's own configuration
//! are classified `safe` and can be applied; everything that alters how the
//! project is compiled is `review_required` and is only ever shown as a diff.

use crate::config::Config;
use crate::store::WsStore;
use crate::util;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// Line-based unified diff with three lines of context.
pub fn unified_diff(old: &str, new: &str, path: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // Longest common subsequence table; remediation targets are small configuration files.
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    // (tag, old index, new index, text)
    let mut ops: Vec<(char, usize, usize, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            ops.push((' ', i, j, a[i]));
            i += 1;
            j += 1;
        } else if i < a.len() && (j == b.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
            // Removals precede additions within a change, as `diff -u` prints them.
            ops.push(('-', i, j, a[i]));
            i += 1;
        } else {
            ops.push(('+', i, j, b[j]));
            j += 1;
        }
    }
    if ops.iter().all(|o| o.0 == ' ') {
        return String::new();
    }
    let mut out = format!("--- a/{}\n+++ b/{}\n", path, path);
    let changed: Vec<usize> = ops.iter().enumerate().filter(|(_, o)| o.0 != ' ').map(|(k, _)| k).collect();
    let mut k = 0;
    while k < changed.len() {
        let start = changed[k].saturating_sub(3);
        let mut end = (changed[k] + 4).min(ops.len());
        while k + 1 < changed.len() && changed[k + 1] <= end + 3 {
            k += 1;
            end = (changed[k] + 4).min(ops.len());
        }
        let hunk = &ops[start..end];
        let old_count = hunk.iter().filter(|o| o.0 != '+').count();
        let new_count = hunk.iter().filter(|o| o.0 != '-').count();
        let old_start = if old_count == 0 { hunk[0].1 } else { hunk[0].1 + 1 };
        let new_start = if new_count == 0 { hunk[0].2 } else { hunk[0].2 + 1 };
        out.push_str(&format!("@@ -{},{} +{},{} @@\n", old_start, old_count, new_start, new_count));
        for (tag, _, _, text) in hunk {
            out.push(*tag);
            out.push_str(text);
            out.push('\n');
        }
        k += 1;
    }
    out
}

/// Sets `key = value` inside `[section]`, preserving every other byte of the document.
/// Fails rather than guesses when the result does not parse to exactly the intended value.
pub fn toml_set(text: &str, section: &str, key: &str, value: &str) -> Result<String, String> {
    let header = format!("[{}]", section);
    let lines: Vec<&str> = text.lines().collect();
    let line = format!("{} = {}", key, value);
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    match lines.iter().position(|l| l.trim() == header) {
        Some(h) => {
            let end = lines.iter().enumerate().skip(h + 1).find(|(_, l)| l.trim_start().starts_with('[')).map(|(i, _)| i).unwrap_or(lines.len());
            let existing = (h + 1..end).find(|&i| lines[i].split_once('=').map(|(k, _)| k.trim() == key).unwrap_or(false));
            match existing {
                Some(i) => out[i] = line,
                None => {
                    let last = (h..end).rev().find(|&i| !lines[i].trim().is_empty()).unwrap_or(h);
                    out.insert(last + 1, line);
                }
            }
        }
        None => {
            if out.last().map(|l| !l.trim().is_empty()).unwrap_or(false) {
                out.push(String::new());
            }
            out.push(header);
            out.push(line);
        }
    }
    let mut result = out.join("\n");
    result.push('\n');
    let doc: toml::Table = result.parse().map_err(|e| format!("the edited document would not be valid TOML ({}); it defines `{}` in a form Haul does not edit", e, section))?;
    let want: toml::Table = format!("v = {}", value).parse().map_err(|e| format!("invalid value {:?}: {}", value, e))?;
    // Walk the dotted section path; quoted segments (`package."*"`) are single keys.
    let mut cur = &doc;
    let mut rest = section;
    while !rest.is_empty() {
        let (seg, tail) = if let Some(stripped) = rest.strip_prefix('"') {
            let close = stripped.find('"').ok_or("unterminated quoted section name")?;
            (&stripped[..close], stripped[close + 1..].trim_start_matches('.'))
        } else {
            rest.split_once('.').unwrap_or((rest, ""))
        };
        cur = cur.get(seg).and_then(|v| v.as_table()).ok_or_else(|| format!("`{}` is not a table after the edit", section))?;
        rest = tail;
    }
    if cur.get(key) != want.get("v") {
        return Err(format!("`{}.{}` does not have the intended value after the edit", section, key));
    }
    Ok(result)
}

fn file_remediation(rec: &Value, root: &Path, rel: &str, class: &str, new_content: impl FnOnce(&str) -> Result<String, String>, summary: &str) -> Value {
    let path = root.join(rel);
    let pre = match fs::read_to_string(&path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return guidance(rec, &format!("{} could not be read: {}", path.display(), e)),
    };
    let post = match new_content(pre.as_deref().unwrap_or("")) {
        Ok(p) => p,
        Err(e) => return guidance(rec, &format!("no automatic patch: {}", e)),
    };
    let diff = unified_diff(pre.as_deref().unwrap_or(""), &post, rel);
    if diff.is_empty() {
        return json!({
            "id": Value::Null, "recommendation": rec["id"], "class": class, "state": "not-needed", "file": rel,
            "summary": format!("{}: the file already has this setting", summary),
        });
    }
    let diff_sha = util::sha256_hex(diff.as_bytes());
    json!({
        "id": format!("rem-{}", &diff_sha[..12]),
        "recommendation": rec["id"],
        "class": class,
        "state": "previewed",
        "file": rel,
        "summary": summary,
        "preimage_sha256": pre.as_ref().map(|p| util::sha256_hex(p.as_bytes())),
        "preimage_content": pre,
        "postimage_sha256": util::sha256_hex(post.as_bytes()),
        "postimage_content": post,
        "diff": diff,
        "diff_sha256": diff_sha,
        "rollback": { "method": "restore the recorded preimage (or remove the file if it did not exist)", "command": Value::Null },
        "verification": rec["verification"],
        "applicable": class == "safe",
        "apply_note": if class == "safe" { "can be applied with `haul fix --apply <id>`" } else { "review required: apply the diff yourself (for example `haul fix --dry-run --patch | git apply`) after reviewing it" },
    })
}

fn guidance(rec: &Value, text: &str) -> Value {
    json!({
        "id": Value::Null, "recommendation": rec["id"], "class": rec["remediation"], "state": "guidance-only",
        "summary": text, "verification": rec["verification"], "compatibility": rec["compatibility"], "applicable": false,
    })
}

/// Builds remediation previews for the recommendations of a report. Nothing is written to the repository.
pub fn preview(report: &Value, root: &Path, only: Option<&str>, option: Option<&str>) -> Result<Vec<Value>, String> {
    let recs: Vec<&Value> = report["recommendations"].as_array().into_iter().flatten().filter(|r| only.map(|o| r["id"] == json!(o)).unwrap_or(true)).collect();
    if let (Some(o), true) = (only, recs.is_empty()) {
        return Err(format!("no recommendation with id `{}` in run {}", o, report["run"]["id"]));
    }
    let mut out = Vec::new();
    for rec in recs {
        let rem = match rec["rule"].as_str().unwrap_or("") {
            "cache_strategy_choice" => {
                let alts = rec["alternatives"].as_array().map(Vec::as_slice).unwrap_or(&[]);
                let chosen = match option {
                    Some(o) => alts.iter().find(|a| a["id"] == json!(o)).ok_or_else(|| format!("recommendation {} has no alternative `{}` (available: {})", rec["id"], o, alts.iter().filter_map(|a| a["id"].as_str()).collect::<Vec<_>>().join(", ")))?,
                    None => alts.first().ok_or("the recommendation lists no alternatives")?,
                };
                let workload = report["policy"]["workload"].as_str().unwrap_or("local-dev");
                let value = if chosen["id"] == json!("disable-incremental") { "\"off\"" } else { "\"cargo-default\"" };
                let mut r = file_remediation(rec, root, ".haul/config.toml", "safe", |t| toml_set(t, "incremental", workload, value), &format!("{} ({})", util::vstr(chosen, "action"), util::vstr(chosen, "id")));
                r["alternative"] = chosen["id"].clone();
                r
            }
            "dev_dependency_debuginfo" => file_remediation(
                rec,
                root,
                "Cargo.toml",
                "review_required",
                |t| toml_set(t, "profile.dev", "debug", "\"line-tables-only\"").and_then(|t| toml_set(&t, "profile.dev.package.\"*\"", "debug", "false")),
                "line tables only for workspace crates, no debug info for dependencies, in the dev profile",
            ),
            "build_script_broad_invalidation" => guidance(rec, "declare the script's inputs with `println!(\"cargo::rerun-if-changed=<path>\")` / `cargo::rerun-if-env-changed=<VAR>` for every file, directory and variable it reads. Haul does not generate these lines: it cannot prove what the script reads"),
            "duplicate_dependency_versions" => guidance(rec, "unify with `cargo update -p <name>` or by aligning the version requirements shown in the evidence; review the resulting Cargo.lock diff"),
            "feature_variant_duplication" => guidance(rec, "make the dependents listed in the evidence request the same feature set, for example via `[workspace.dependencies]`"),
            "stale_artifacts" => guidance(rec, "inspect the plan with `haul gc --dry-run`; this release does not delete build output"),
            _ => guidance(rec, "no file change is associated with this recommendation; follow its verification procedure"),
        };
        out.push(rem);
    }
    Ok(out)
}

fn record_path(store: &WsStore, id: &str) -> PathBuf {
    store.remediations_dir().join(format!("{}.json", id))
}

/// Persists previews so that a later `--apply` acts on exactly what was shown.
pub fn save_previews(store: &WsStore, root: &Path, previews: &[Value]) -> Result<(), String> {
    for p in previews.iter().filter(|p| p["id"].is_string()) {
        let mut rec = p.clone();
        rec["workspace_root"] = json!(util::path_str(root));
        rec["previewed_at"] = json!(util::iso8601(util::now_wall_ns()));
        util::write_json(&record_path(store, p["id"].as_str().unwrap_or("")), &rec).map_err(|e| format!("could not store remediation preview: {}", e))?;
    }
    Ok(())
}

fn current_sha(path: &Path) -> Result<Option<String>, String> {
    match fs::read(path) {
        Ok(b) => Ok(Some(util::sha256_hex(&b))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {}", path.display(), e)),
    }
}

/// Applies a previewed `safe` remediation after verifying the file is unchanged since the preview.
pub fn apply(store: &WsStore, id: &str) -> Result<Value, String> {
    let path = record_path(store, id);
    let mut rem = util::read_json(&path).ok_or_else(|| format!("no previewed remediation `{}`; run `haul fix --dry-run` first", id))?;
    if rem["class"] != json!("safe") {
        return Err(format!("remediation {} is `{}`: it changes how the project is built and is never applied automatically. Review and apply its diff yourself.", id, util::vstr(&rem, "class")));
    }
    if rem["state"] == json!("applied") {
        return Err(format!("remediation {} was already applied", id));
    }
    let root = PathBuf::from(util::vstr(&rem, "workspace_root"));
    let file = root.join(util::vstr(&rem, "file"));
    // Conflict safety: the target must be byte-identical to what the preview was computed from.
    if current_sha(&file)? != rem["preimage_sha256"].as_str().map(str::to_string) {
        return Err(format!("{} changed after the preview was generated; refusing to apply. Run `haul fix --dry-run` again.", file.display()));
    }
    util::write_atomic(&file, rem["postimage_content"].as_str().unwrap_or("").as_bytes()).map_err(|e| format!("could not write {}: {}", file.display(), e))?;
    // Verify the result; a remediation that leaves Haul's configuration invalid is rolled back (FR-170.4).
    let verified = Config::load(&root);
    if let Err(e) = &verified {
        restore_preimage(&file, &rem)?;
        rem["state"] = json!("reverted");
        rem["failure"] = json!(format!("verification failed and the change was rolled back: {}", e));
        util::write_json(&path, &rem).map_err(|e| e.to_string())?;
        return Err(util::vstr(&rem, "failure"));
    }
    rem["state"] = json!("applied");
    rem["applied_at"] = json!(util::iso8601(util::now_wall_ns()));
    rem["rollback"]["command"] = json!(format!("haul fix --rollback {}", id));
    util::write_json(&path, &rem).map_err(|e| format!("the change was applied but its audit record could not be written: {}", e))?;
    Ok(rem)
}

fn restore_preimage(file: &Path, rem: &Value) -> Result<(), String> {
    match rem["preimage_content"].as_str() {
        Some(pre) => util::write_atomic(file, pre.as_bytes()).map_err(|e| format!("could not restore {}: {}", file.display(), e)),
        None => fs::remove_file(file).map_err(|e| format!("could not remove {}: {}", file.display(), e)),
    }
}

/// Reverts an applied remediation, provided the file still holds exactly what was applied.
pub fn rollback(store: &WsStore, id: &str) -> Result<Value, String> {
    let path = record_path(store, id);
    let mut rem = util::read_json(&path).ok_or_else(|| format!("no remediation `{}`", id))?;
    if rem["state"] != json!("applied") {
        return Err(format!("remediation {} is in state `{}`, not `applied`", id, util::vstr(&rem, "state")));
    }
    let file = PathBuf::from(util::vstr(&rem, "workspace_root")).join(util::vstr(&rem, "file"));
    if current_sha(&file)? != rem["postimage_sha256"].as_str().map(str::to_string) {
        return Err(format!("{} changed after the remediation was applied; refusing to overwrite it. Undo the change manually using the recorded diff.", file.display()));
    }
    restore_preimage(&file, &rem)?;
    rem["state"] = json!("reverted");
    rem["reverted_at"] = json!(util::iso8601(util::now_wall_ns()));
    util::write_json(&path, &rem).map_err(|e| e.to_string())?;
    Ok(rem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_is_minimal_and_well_formed() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\nb\nc\nd\nE\nf\ng\nh\n";
        let d = unified_diff(old, new, "x.toml");
        assert_eq!(d, "--- a/x.toml\n+++ b/x.toml\n@@ -2,7 +2,7 @@\n b\n c\n d\n-e\n+E\n f\n g\n h\n");
        assert_eq!(unified_diff(old, old, "x"), "");
        assert_eq!(unified_diff("", "k = 1\n", "new.toml"), "--- a/new.toml\n+++ b/new.toml\n@@ -0,0 +1,1 @@\n+k = 1\n");
    }

    #[test]
    fn toml_set_preserves_everything_else() {
        let src = "# top comment\n[package]\nname = \"x\"   # keep me\n\n[profile.dev]\nopt-level = 1\n\n[dependencies]\nserde = \"1\"\n";
        let out = toml_set(src, "profile.dev", "debug", "\"line-tables-only\"").unwrap();
        assert_eq!(out, "# top comment\n[package]\nname = \"x\"   # keep me\n\n[profile.dev]\nopt-level = 1\ndebug = \"line-tables-only\"\n\n[dependencies]\nserde = \"1\"\n");
        let replaced = toml_set(&out, "profile.dev", "debug", "false").unwrap();
        assert!(replaced.contains("debug = false") && !replaced.contains("line-tables-only"));
        let appended = toml_set("[package]\nname = \"x\"\n", "profile.dev.package.\"*\"", "debug", "false").unwrap();
        assert!(appended.ends_with("\n[profile.dev.package.\"*\"]\ndebug = false\n"));
    }

    #[test]
    fn toml_set_refuses_forms_it_cannot_edit_exactly() {
        // The profile is defined through a dotted key; adding a section would redefine it.
        let err = toml_set("profile.dev.debug = true\n[package]\nname = \"x\"\n", "profile.dev", "debug", "false").unwrap_err();
        assert!(err.contains("would not be valid TOML"), "{}", err);
    }

    fn fixture() -> (PathBuf, WsStore, Value) {
        let base = std::env::temp_dir().join(format!("haul-fix-{}", util::new_id()));
        let root = base.join("ws");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\nversion = \"0.1.0\"\n").unwrap();
        let store = WsStore { id: "t".into(), dir: base.join("state"), root: Some(root.clone()) };
        let report = json!({
            "run": { "id": "R" }, "policy": { "workload": "ci" },
            "recommendations": [
                { "id": "rec:cache", "rule": "cache_strategy_choice", "remediation": "safe", "verification": "v",
                  "alternatives": [{ "id": "disable-incremental", "action": "off" }, { "id": "keep-incremental", "action": "keep" }] },
                { "id": "rec:debug", "rule": "dev_dependency_debuginfo", "remediation": "review_required", "verification": "v" },
                { "id": "rec:script", "rule": "build_script_broad_invalidation", "remediation": "review_required", "verification": "v" }
            ]
        });
        (root, store, report)
    }

    #[test]
    fn preview_touches_nothing_and_classifies_changes() {
        let (root, _store, report) = fixture();
        let before = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let p = preview(&report, &root, None, None).unwrap();
        assert_eq!(fs::read_to_string(root.join("Cargo.toml")).unwrap(), before, "a preview must not modify the manifest");
        assert!(!root.join(".haul").exists(), "a preview must not create configuration");
        assert_eq!((p[0]["class"].as_str(), p[0]["applicable"].as_bool()), (Some("safe"), Some(true)));
        assert!(p[0]["diff"].as_str().unwrap().contains("+ci = \"off\""));
        assert_eq!((p[1]["class"].as_str(), p[1]["applicable"].as_bool()), (Some("review_required"), Some(false)));
        assert!(p[1]["diff"].as_str().unwrap().contains("+[profile.dev.package.\"*\"]"));
        assert_eq!(p[2]["state"], "guidance-only");
        assert!(preview(&report, &root, Some("rec:nope"), None).is_err());
        fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[test]
    fn apply_checks_the_preimage_refuses_review_required_and_rolls_back() {
        let (root, store, report) = fixture();
        let p = preview(&report, &root, None, None).unwrap();
        save_previews(&store, &root, &p).unwrap();
        let (safe_id, review_id) = (p[0]["id"].as_str().unwrap(), p[1]["id"].as_str().unwrap());
        assert!(apply(&store, review_id).unwrap_err().contains("never applied automatically"));

        // FR-170 acceptance: the target changed after the preview ⇒ refuse.
        fs::create_dir_all(root.join(".haul")).unwrap();
        fs::write(root.join(".haul/config.toml"), "[cache]\nfail = \"open\"\n").unwrap();
        assert!(apply(&store, safe_id).unwrap_err().contains("changed after the preview"));
        assert_eq!(fs::read_to_string(root.join(".haul/config.toml")).unwrap(), "[cache]\nfail = \"open\"\n");

        fs::remove_file(root.join(".haul/config.toml")).unwrap();
        let applied = apply(&store, safe_id).unwrap();
        assert_eq!(applied["state"], "applied");
        assert_eq!(fs::read_to_string(root.join(".haul/config.toml")).unwrap(), "[incremental]\nci = \"off\"\n");
        assert!(apply(&store, safe_id).is_err(), "applying twice is an error");
        assert_eq!(rollback(&store, safe_id).unwrap()["state"], "reverted");
        assert!(!root.join(".haul/config.toml").exists(), "the file did not exist before, so rollback removes it");
        fs::remove_dir_all(root.parent().unwrap()).ok();
    }
}
