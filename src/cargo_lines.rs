//! Cargo's human-readable output, read: its status lines (`    Updating crates.io index`,
//! `  Downloaded serde v1.0.229`), its progress bars (`    Building [===>  ] 9/16: syn, quote`,
//! ` Downloading 3 crates, remaining bytes: 281.0KiB`) and everything else, which QuickRust shows as
//! Cargo printed it.

/// One line (or progress update) of Cargo's stderr, its colours removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line<'a> {
    /// A status line: a capitalised verb right-aligned in twelve columns, then what it is about.
    Status { verb: &'a str, rest: &'a str },
    /// A progress update: how far of how much, and what is under way.
    Progress { verb: &'a str, done: Option<u64>, total: Option<u64>, detail: &'a str },
    /// Anything else (diagnostics, notes, their continuation lines, a build script's output).
    Other(&'a str),
}

/// What `text` (one segment of Cargo's stderr, split at `\n` and `\r`, colours removed) is.
pub fn classify(text: &str) -> Line<'_> {
    let line = text.trim_end();
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    let verb = trimmed.split(' ').next().unwrap_or("");
    let is_verb = !verb.is_empty() && verb.bytes().next().is_some_and(|c| c.is_ascii_uppercase()) && verb.bytes().all(|c| c.is_ascii_alphabetic());
    // Right-aligned in 12 columns (a longer verb starts at column 0).
    if !is_verb || indent + verb.len() != 12.max(verb.len()) {
        return Line::Other(text);
    }
    let rest = trimmed[verb.len()..].trim_start();
    // `Building [====>     ] 9/16: syn, quote`; `Cleaning [=>   ]  12.50%`.
    if let Some(bar) = rest.strip_prefix('[') {
        if let Some((_, after)) = bar.split_once(']') {
            let after = after.trim_start();
            let (counts, detail) = after.split_once(':').map(|(c, d)| (c, d.trim())).unwrap_or((after, ""));
            let mut n = counts.trim().split('/').map(|x| x.trim().parse::<u64>().ok());
            return match (n.next().flatten(), n.next().flatten()) {
                (Some(done), Some(total)) => Line::Progress { verb, done: Some(done), total: Some(total), detail },
                _ => Line::Progress { verb, done: None, total: None, detail: after.trim() },
            };
        }
    }
    // The sparse index's lookups: `Fetch 26 complete; 4 pending` (no bar: its total grows as
    // the resolver finds more packages).
    if verb == "Fetch" {
        if let Some((done, pending)) = rest.split_once(" complete; ").and_then(|(d, p)| Some((d.parse::<u64>().ok()?, p.strip_suffix(" pending")?.parse::<u64>().ok()?))) {
            return Line::Progress { verb, done: Some(done), total: Some(done + pending), detail: "" };
        }
    }
    // `Downloading 3 crates, remaining bytes: 281.0KiB` / `Downloading 1 crate` / `Downloading crates ...`
    if verb == "Downloading" {
        let remaining = rest.split(' ').next().and_then(|n| n.parse::<u64>().ok());
        if remaining.is_some() || rest.starts_with("crates") {
            let detail = rest.split_once(", remaining bytes: ").map(|(_, b)| b).unwrap_or("");
            return Line::Progress { verb, done: None, total: remaining, detail };
        }
    }
    Line::Status { verb, rest }
}

/// `serde v1.0.229` → `("serde", "1.0.229")`; `serde v1.0.200 -> v1.0.229` → the first part.
pub fn package(rest: &str) -> Option<(&str, &str)> {
    let mut it = rest.split(' ');
    let name = it.next()?;
    let v = it.next()?.strip_prefix('v')?;
    v.bytes().next().filter(u8::is_ascii_digit)?;
    Some((name, v))
}

/// `serde v1.0.200 -> v1.0.229` → `("serde", "1.0.200", "1.0.229")`.
pub fn change(rest: &str) -> Option<(&str, &str, &str)> {
    let (name, from) = package(rest)?;
    let to = rest.split(" -> ").nth(1)?.split(' ').next()?.strip_prefix('v')?;
    Some((name, from, to))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st<'a>(verb: &'a str, rest: &'a str) -> Line<'a> {
        Line::Status { verb, rest }
    }

    #[test]
    fn reads_cargos_status_lines() {
        // Captured from Cargo 1.98 and 1.99.
        for (line, want) in [
            ("    Updating crates.io index", st("Updating", "crates.io index")),
            ("     Locking 7 packages to highest Rust 1.99.0 compatible versions", st("Locking", "7 packages to highest Rust 1.99.0 compatible versions")),
            ("      Adding serde v1.0.229 to dependencies", st("Adding", "serde v1.0.229 to dependencies")),
            ("  Downloaded serde_core v1.0.229", st("Downloaded", "serde_core v1.0.229")),
            ("  Downloaded 7 crates (645.0KiB) in 0.16s", st("Downloaded", "7 crates (645.0KiB) in 0.16s")),
            ("   Compiling proc-macro2 v1.0.107", st("Compiling", "proc-macro2 v1.0.107")),
            (" Documenting demo v0.1.0 (/x/demo)", st("Documenting", "demo v0.1.0 (/x/demo)")),
            ("     Removed 125 files, 75.8MiB total", st("Removed", "125 files, 75.8MiB total")),
            ("    Blocking waiting for file lock on package cache", st("Blocking", "waiting for file lock on package cache")),
            ("    Building [==============>             ] 9/16: quote, proc-macro2, serde_core", Line::Progress { verb: "Building", done: Some(9), total: Some(16), detail: "quote, proc-macro2, serde_core" }),
            (" Downloading 5 crates, remaining bytes: 30.9KiB", Line::Progress { verb: "Downloading", done: None, total: Some(5), detail: "30.9KiB" }),
            (" Downloading 1 crate", Line::Progress { verb: "Downloading", done: None, total: Some(1), detail: "" }),
            (" Downloading crates ...", Line::Progress { verb: "Downloading", done: None, total: None, detail: "" }),
            (" Downloading 6 crates, extracting regex-automata ...", Line::Progress { verb: "Downloading", done: None, total: Some(6), detail: "" }),
            ("       Fetch 26 complete; 4 pending", Line::Progress { verb: "Fetch", done: Some(26), total: Some(30), detail: "" }),
            ("  Downloaded 25 crates (4.4MiB) in 0.32s (largest was `encoding_rs` at 1.3MiB)", st("Downloaded", "25 crates (4.4MiB) in 0.32s (largest was `encoding_rs` at 1.3MiB)")),
            ("    Cleaning [=====>                   ]  21.43%", Line::Progress { verb: "Cleaning", done: None, total: None, detail: "21.43%" }),
        ] {
            assert_eq!(classify(line), want, "{:?}", line);
        }
    }

    #[test]
    fn everything_else_is_shown_as_it_is() {
        for line in ["error: failed to select a version for the requirement `serde = \"^1.0.229\"`", "             Features:", "             + derive", "  |", "note: see https://doc.rust-lang.org/cargo", "hello from a build script", "Compiling x", "   compiling x"] {
            assert_eq!(classify(line), Line::Other(line), "{:?}", line);
        }
    }

    #[test]
    fn names_packages_and_changes() {
        assert_eq!(package("serde v1.0.229 to dependencies"), Some(("serde", "1.0.229")));
        assert_eq!(change("serde v1.0.200 -> v1.0.229"), Some(("serde", "1.0.200", "1.0.229")));
        assert_eq!(change("libc v0.2.150 -> v0.2.190 (available: v0.3.0)"), Some(("libc", "0.2.150", "0.2.190")));
        assert_eq!(package("serde from dependencies"), None);
    }
}
