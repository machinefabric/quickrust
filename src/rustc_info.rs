//! What a compiler says about itself (`rustc -vV`), asked once per compiler binary.
//!
//! The artifact cache keys entries by it, and the wrapper's fast-mode levers need its release
//! channel: on a stable or beta compiler they unlock `-Z` flags with `RUSTC_BOOTSTRAP` and must
//! then keep stable's rule that `#![feature]` is rejected.

use crate::util;
use std::fs;
use std::path::{Path, PathBuf};

/// `rustc -vV` of `rustc` (a path, or a bare name looked up on `PATH` as Cargo may pass it),
/// remembered in `memo_dir` per binary identity. QuickRust's own `rustc` proxy picks a toolchain on
/// every call, so its answers are not remembered.
pub fn version_verbose(memo_dir: &Path, rustc: &str) -> Result<String, String> {
    let text = ask(memo_dir, rustc, "-vV")?;
    if !text.starts_with("rustc ") {
        return Err(format!("{} -vV did not identify a Rust compiler", rustc));
    }
    Ok(text)
}

/// `rustc --print sysroot`: the toolchain the compiler is (two toolchains can share a release
/// and a commit), remembered like `version_verbose`.
pub fn sysroot(memo_dir: &Path, rustc: &str) -> Result<PathBuf, String> {
    let text = ask(memo_dir, rustc, "--print=sysroot")?;
    let path = PathBuf::from(text.trim());
    if !path.is_absolute() {
        return Err(format!("{} --print sysroot did not name a directory", rustc));
    }
    Ok(path)
}

/// What the compiler adds to a binary's name for `target` (the host's when `None`): `.exe`,
/// `.wasm`, or nothing. Asked as Cargo asks (`--print file-names`), so it holds for any target,
/// a JSON specification included.
pub fn binary_suffix(rustc: &Path, target: Option<&str>) -> Result<String, String> {
    let mut cmd = crate::engine::toolchain_program(rustc);
    cmd.args(["-", "--crate-name", "___", "--crate-type", "bin", "--print", "file-names"]);
    if let Some(t) = target {
        cmd.args(["--target", t]);
        // As Cargo passes a JSON specification: unstable, refused by a stable compiler.
        if t.ends_with(".json") {
            cmd.arg("-Zunstable-options");
        }
    }
    let out = cmd.stdin(std::process::Stdio::null()).output().map_err(|e| format!("cannot run {}: {}", rustc.display(), e))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Err(format!("{} cannot name a binary for {}: {}", rustc.display(), target.unwrap_or("the host"), String::from_utf8_lossy(&out.stderr).trim()));
    }
    text.lines().next().and_then(|l| l.strip_prefix("___")).map(str::to_string).ok_or_else(|| format!("{} named a binary `{}`, not after its crate", rustc.display(), text.trim()))
}

fn ask(memo_dir: &Path, rustc: &str, arg: &str) -> Result<String, String> {
    let path = if rustc.contains(std::path::MAIN_SEPARATOR) || rustc.contains('/') { PathBuf::from(rustc) } else { util::which(rustc).ok_or_else(|| format!("{} is not on PATH", rustc))? };
    let md = fs::metadata(&path).map_err(|e| format!("cannot inspect {}: {}", path.display(), e))?;
    let canonical = util::canon(&path);
    let proxy = canonical == crate::resolve::self_identity().0;
    let memo = memo_dir.join(util::digest_parts([util::path_str(&canonical), md.len().to_string(), util::mtime_ns(&md).to_string(), arg.to_string()]));
    if let Ok(text) = fs::read_to_string(&memo) {
        if !text.is_empty() && !proxy {
            return Ok(text);
        }
    }
    let out = crate::engine::toolchain_program(&path).arg(arg).stdin(std::process::Stdio::null()).output().map_err(|e| format!("cannot run {} {}: {}", path.display(), arg, e))?;
    let text = String::from_utf8(out.stdout).map_err(|_| format!("{} {} printed something other than UTF-8", path.display(), arg))?;
    if !out.status.success() || text.trim().is_empty() {
        return Err(format!("{} {} failed", path.display(), arg));
    }
    if !proxy {
        util::create_private_dir(memo_dir).and_then(|_| util::write_atomic_fast(&memo, text.as_bytes())).map_err(|e| e.to_string())?;
    }
    Ok(text)
}

/// Whether the compiler is a nightly or locally built one, on which unstable features are
/// available without `RUSTC_BOOTSTRAP`. `None` when its account has no `release:` line.
pub fn is_unstable_channel(version_verbose: &str) -> Option<bool> {
    let release = version_verbose.lines().find_map(|l| l.strip_prefix("release: "))?;
    Some(release.ends_with("-nightly") || release.ends_with("-dev"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // TEST0111: A compiler is described by its own account whatever it is called
    #[test]
    fn test0111_a_compiler_is_described_by_its_own_account_whatever_it_is_called() {
        let dir = std::env::temp_dir().join(format!("qrust-rustc-vv-{}", util::new_id()));
        // By bare name through PATH, as Cargo may pass it, and by absolute path: the same compiler.
        let by_name = version_verbose(&dir, "rustc").unwrap();
        let path = util::which("rustc").unwrap();
        assert_eq!(version_verbose(&dir, &util::path_str(&path)).unwrap(), by_name);
        let out = std::process::Command::new(&path).arg("-vV").output().unwrap();
        assert_eq!(by_name.as_bytes(), out.stdout.as_slice());
        assert!(version_verbose(&dir, "no-such-compiler-xyz").unwrap_err().contains("not on PATH"));
        fs::remove_dir_all(&dir).ok();
    }

    // TEST0112: The release channel comes from the release line
    #[test]
    fn test0112_the_release_channel_comes_from_the_release_line() {
        let vv = |release: &str| format!("rustc {release} (abc 2026-10-01)\nbinary: rustc\nrelease: {release}\nLLVM version: 21.1.0\n");
        assert_eq!(is_unstable_channel(&vv("1.98.1")), Some(false));
        assert_eq!(is_unstable_channel(&vv("1.99.0-beta.3")), Some(false));
        assert_eq!(is_unstable_channel(&vv("1.101.0-nightly")), Some(true));
        assert_eq!(is_unstable_channel(&vv("1.101.0-dev")), Some(true));
        assert_eq!(is_unstable_channel("rustc 1.98.1\n"), None);
    }
}
