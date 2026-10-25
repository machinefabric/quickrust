//! How this QuickRust was installed: by a package manager, or by itself.
//!
//! A package (the Homebrew formula, the deb, the rpm) puts `MARKER` beside the `qrust` it installs.
//! It names the package manager, the path the package's `qrust` is reached by whatever version is
//! installed (Homebrew's `opt/quickrust`, not the versioned Cellar), and the manager's commands that
//! update and remove it. Such a QuickRust is the one binary on the machine: `qrust setup` links
//! `~/.qrust/bin` to it and copies nothing, and `qrust self update` and `qrust self uninstall`
//! leave it to its manager. A QuickRust without the marker (an archive, a source build) installs
//! itself: setup copies it into `~/.qrust/bin`, and that copy is the install.
//!
//! The marker is the package's statement about the binary beside it, so a marker that does not
//! describe that binary, or cannot be read, is an error, never taken for a standalone install.

use crate::util;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// The file a package puts beside the `qrust` it installs.
pub const MARKER: &str = "qrust-installed-by.json";
const FORMAT: u64 = 1;

/// A QuickRust a package manager installed, as its marker says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    /// The package manager, as people name it (`Homebrew`, `apt`, `dnf`).
    pub manager: String,
    /// The package's `qrust`, by the path that stays the same across its versions.
    pub program: PathBuf,
    /// The command that updates it.
    pub update: String,
    /// The command that removes it.
    pub uninstall: String,
}

impl Package {
    /// The package's rustc wrapper, beside its `qrust`.
    pub fn wrapper(&self) -> PathBuf {
        util::exe_candidates(self.program.parent().unwrap_or(Path::new(".")), "qrust-rustc-wrapper").remove(0)
    }
}

/// The package that installed the QuickRust binary `qrust`, if one did: the marker beside the file
/// `qrust` resolves to. An error when the marker is there but unreadable, malformed, or about
/// another binary.
pub fn of(qrust: &Path) -> Result<Option<Package>, String> {
    let resolved = util::canon(qrust);
    let marker = resolved.with_file_name(MARKER);
    let text = match fs::read_to_string(&marker) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}, which says how QuickRust was installed: {}", marker.display(), e)),
    };
    let bad = |why: &str| format!("{} does not say how QuickRust was installed: {}; reinstall the package", marker.display(), why);
    let v: Value = serde_json::from_str(&text).map_err(|e| bad(&format!("it is not JSON ({})", e)))?;
    if v["format"] != serde_json::json!(FORMAT) {
        return Err(bad(&format!("its format is {}, not {}", v["format"], FORMAT)));
    }
    let field = |name: &str| v[name].as_str().filter(|s| !s.trim().is_empty()).map(str::to_string).ok_or_else(|| bad(&format!("`{}` is missing", name)));
    let package = Package { manager: field("manager")?, program: PathBuf::from(field("program")?), update: field("update")?, uninstall: field("uninstall")? };
    if !package.program.is_absolute() {
        return Err(bad(&format!("`program` {} is not an absolute path", package.program.display())));
    }
    if util::canon(&package.program) != resolved {
        return Err(bad(&format!("it names {}, which is not the binary beside it ({})", package.program.display(), resolved.display())));
    }
    if cfg!(windows) {
        return Err(format!("{} says {} installed QuickRust, and no package manager installs it on Windows; install it from its archive", marker.display(), package.manager));
    }
    Ok(Some(package))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in installed program: `qrust` and its wrapper in `dir`.
    fn program(dir: &Path) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let qrust = dir.join("qrust");
        fs::write(&qrust, "qrust").unwrap();
        fs::write(dir.join("qrust-rustc-wrapper"), "wrapper").unwrap();
        qrust
    }

    // TEST0306: A marker is read only when it describes the binary beside it
    #[cfg(unix)]
    #[test]
    fn test0306_a_marker_is_read_only_when_it_describes_the_binary_beside_it() {
        let dir = std::env::temp_dir().join(format!("qrust-package-{}", util::new_id()));
        // Versioned where it is, reached by a path that stays (as Homebrew's Cellar and opt).
        let cellar = program(&dir.join("Cellar/quickrust/1.0/libexec"));
        fs::create_dir_all(dir.join("opt")).unwrap();
        std::os::unix::fs::symlink(dir.join("Cellar/quickrust/1.0"), dir.join("opt/quickrust")).unwrap();
        let stable = dir.join("opt/quickrust/libexec/qrust");
        assert_eq!(of(&cellar).unwrap(), None, "no marker: a standalone install");
        let marker = cellar.with_file_name(MARKER);
        let write = |v: serde_json::Value| fs::write(&marker, v.to_string()).unwrap();
        write(serde_json::json!({ "format": 1, "manager": "Homebrew", "program": util::path_str(&stable), "update": "brew upgrade quickrust", "uninstall": "brew uninstall quickrust" }));
        let p = of(&cellar).unwrap().expect("a package install");
        assert_eq!((p.manager.as_str(), p.program.as_path(), p.update.as_str()), ("Homebrew", stable.as_path(), "brew upgrade quickrust"));
        assert_eq!(p.wrapper(), dir.join("opt/quickrust/libexec/qrust-rustc-wrapper"));
        // Reached by a link (the package manager's on PATH, or ~/.qrust/bin's): the same package.
        let on_path = dir.join("bin-qrust");
        std::os::unix::fs::symlink(&stable, &on_path).unwrap();
        assert_eq!(of(&on_path).unwrap(), Some(p));
        // About another binary, malformed, of another format, or missing a command: an error.
        let other = program(&dir.join("elsewhere"));
        write(serde_json::json!({ "format": 1, "manager": "Homebrew", "program": util::path_str(&other), "update": "u", "uninstall": "r" }));
        assert!(of(&cellar).unwrap_err().contains("not the binary beside it"));
        fs::write(&marker, "{not json").unwrap();
        assert!(of(&cellar).unwrap_err().contains("not JSON"));
        write(serde_json::json!({ "format": 2, "manager": "Homebrew", "program": util::path_str(&stable), "update": "u", "uninstall": "r" }));
        assert!(of(&cellar).unwrap_err().contains("format is 2"));
        write(serde_json::json!({ "format": 1, "manager": "Homebrew", "program": util::path_str(&stable), "update": "u" }));
        assert!(of(&cellar).unwrap_err().contains("`uninstall` is missing"));
        write(serde_json::json!({ "format": 1, "manager": "Homebrew", "program": "opt/quickrust/libexec/qrust", "update": "u", "uninstall": "r" }));
        assert!(of(&cellar).unwrap_err().contains("not an absolute path"));
        let _ = fs::remove_dir_all(&dir);
    }
}
