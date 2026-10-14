//! Small shared helpers: ids, time, hashing, path tokenization, redaction, atomic writes.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_wall_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

pub fn mtime_ns(md: &fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// RFC 3339 UTC timestamp with millisecond precision.
pub fn iso8601(wall_ns: u64) -> String {
    let secs = (wall_ns / 1_000_000_000) as i64;
    let ms = (wall_ns % 1_000_000_000) / 1_000_000;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, m, d, rem / 3600, (rem % 3600) / 60, rem % 60, ms)
}

/// The instant an `iso8601` timestamp names (`2026-10-12T21:58:09.007Z`), as wall-clock
/// nanoseconds.
pub fn parse_iso8601(s: &str) -> Option<u64> {
    let (date, time) = s.strip_suffix('Z')?.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (hms, ms) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.split(':').map(|p| p.parse::<i64>().ok());
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next()??);
    let ms: i64 = ms.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || hh > 23 || mm > 59 || ss > 60 || d.next().is_some() || t.next().is_some() {
        return None;
    }
    // Days-from-civil (Howard Hinnant's algorithm), the inverse of `iso8601`'s.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3_600 + mm * 60 + ss;
    u64::try_from(secs).ok().map(|s| s * 1_000_000_000 + ms as u64 * 1_000_000)
}

/// How long ago `then` (wall-clock nanoseconds) was, as people say it: `just now`,
/// `40 seconds ago`, `3 minutes ago`, `2 hours ago`, `5 days ago`.
pub fn human_ago(then: u64, now: u64) -> String {
    let s = now.saturating_sub(then) / 1_000_000_000;
    let (n, unit) = match s {
        0..=9 => return "just now".to_string(),
        10..=59 => (s, "second"),
        60..=3_599 => (s / 60, "minute"),
        3_600..=86_399 => (s / 3_600, "hour"),
        _ => (s / 86_400, "day"),
    };
    format!("{} {}{} ago", n, unit, if n == 1 { "" } else { "s" })
}

fn random_u64() -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut h = RandomState::new().build_hasher();
    h.write_u64(now_wall_ns());
    h.write_u32(std::process::id());
    h.finish()
}

/// Time-sortable 26-character identifier (ULID layout, Crockford base32).
pub fn new_id() -> String {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let ms = now_wall_ns() / 1_000_000;
    let mut out = [b'0'; 26];
    for i in 0..10 {
        out[9 - i] = ALPHABET[((ms >> (5 * i)) & 31) as usize];
    }
    let (a, b) = (random_u64(), random_u64());
    for i in 0..16 {
        let src = if i < 8 { a } else { b };
        out[10 + i] = ALPHABET[((src >> (5 * (i % 8))) & 31) as usize];
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// BLAKE3 of a file's content, in hex: the artifact cache's content hash, several times
/// faster than SHA-256 on the tens of megabytes a library or binary weighs.
pub fn blake3_file(path: &Path) -> io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().to_hex().to_string())
}

/// Digest of an ordered list of strings with unambiguous framing.
pub fn digest_parts<I, S>(parts: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut h = Sha256::new();
    for p in parts {
        let p = p.as_ref();
        h.update((p.len() as u64).to_le_bytes());
        h.update(p.as_bytes());
    }
    hex(&h.finalize())[..32].to_string()
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The Cargo home of every build QuickRust runs: `$QRUST_HOME/cargo` (see `engine`).
pub fn cargo_home() -> PathBuf {
    qrust_home().join("cargo")
}

/// QuickRust's own state directory (`QRUST_HOME`, default `~/.qrust`).
pub fn qrust_home() -> PathBuf {
    std::env::var_os(crate::ENV_HOME)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".qrust")))
        .unwrap_or_else(|| std::env::temp_dir().join("qrust"))
}

/// QuickRust's disposable cache (`$QRUST_HOME/cache`): everything in it can be rebuilt or
/// downloaded again, and QuickRust removes what it must to stay within its space (see `space`).
pub fn cache_dir() -> PathBuf {
    qrust_home().join("cache")
}

/// Remembered answers (what a compiler says about itself, which workspace a directory is in).
pub fn memo_dir(kind: &str) -> PathBuf {
    cache_dir().join("memo").join(kind)
}

pub fn lossy(s: &OsStr) -> String {
    s.to_string_lossy().into_owned()
}

pub fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Canonicalize for identity purposes only; never used to rewrite user-facing paths.
pub fn canon(p: &Path) -> PathBuf {
    without_verbatim_prefix(fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
}

/// Windows' canonical paths are verbatim (`\\?\C:\…`, `\\?\UNC\server\share\…`), a form many
/// programs do not take and nobody writes: the path as written (`C:\…`, `\\server\share\…`) when
/// it means the same, which a path short enough for the classic limit does. Other paths (every
/// canonical path elsewhere) are left as they are.
fn without_verbatim_prefix(p: PathBuf) -> PathBuf {
    const MAX_PATH: usize = 260;
    let Some(s) = p.to_str() else { return p };
    let plain = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{}", rest)
    } else if let Some(rest) = s.strip_prefix(r"\\?\").filter(|r| r.as_bytes().get(1) == Some(&b':')) {
        rest.to_string()
    } else {
        return p;
    };
    if plain.len() < MAX_PATH {
        PathBuf::from(plain)
    } else {
        p
    }
}

/// Replaces machine-specific prefixes with portable tokens (DATA-005, PRIV-004).
pub struct Tokenizer {
    pairs: Vec<(String, &'static str)>,
}

impl Tokenizer {
    pub fn new(workspace: Option<&Path>, redact_home: bool) -> Self {
        let mut pairs: Vec<(String, &'static str)> = Vec::new();
        let mut add = |p: PathBuf, tok: &'static str| {
            for v in [path_str(&p), path_str(&canon(&p))] {
                if v.len() > 1 && !pairs.iter().any(|(e, _)| *e == v) {
                    pairs.push((v, tok));
                }
            }
        };
        if let Some(ws) = workspace {
            add(ws.to_path_buf(), "$WORKSPACE");
        }
        if redact_home {
            add(cargo_home(), "$CARGO_HOME");
            add(qrust_home(), "$QRUST_HOME");
            if let Some(h) = home_dir() {
                add(h, "$HOME");
            }
        }
        // Longest prefix first so nested roots win over their parents.
        pairs.sort_by_key(|p| std::cmp::Reverse(p.0.len()));
        Tokenizer { pairs }
    }

    pub fn apply(&self, s: &str) -> String {
        let mut out = s.to_string();
        for (prefix, tok) in &self.pairs {
            if out.contains(prefix.as_str()) {
                out = replace_path_prefix(&out, prefix, tok);
            }
        }
        out
    }
}

/// Replaces `prefix` where it is a whole path prefix: `/a/repo` must not match inside `/a/repo-feature`.
fn replace_path_prefix(s: &str, prefix: &str, token: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find(prefix) {
        let after = &rest[pos + prefix.len()..];
        let continues_name = after.chars().next().map(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '+')).unwrap_or(false);
        out.push_str(&rest[..pos]);
        out.push_str(if continues_name { prefix } else { token });
        rest = after;
    }
    out.push_str(rest);
    out
}

pub fn is_secret_name(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    const MARKERS: &[&str] = &["TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "AUTH", "PRIVATE", "API_KEY", "APIKEY", "ACCESS_KEY", "SESSION_KEY", "SIGNING"];
    MARKERS.iter().any(|m| n.contains(m)) || n.ends_with("_KEY") || n.starts_with("AWS_") || n.starts_with("SCCACHE_")
}

fn redact_url_userinfo(s: &str) -> String {
    if let Some(scheme) = s.find("://") {
        let rest = &s[scheme + 3..];
        let end = rest.find(['/', '?', '#', ' ']).unwrap_or(rest.len());
        if let Some(at) = rest[..end].rfind('@') {
            return format!("{}://<redacted>@{}", &s[..scheme], &rest[at + 1..]);
        }
    }
    s.to_string()
}

/// Removes credential material from an argument vector before it is persisted (DATA-008, SEC-016).
pub fn redact_args(args: &[String]) -> Vec<String> {
    const VALUE_FLAGS: &[&str] = &["--token", "--password", "--secret", "--key", "--secret-key"];
    let mut out = Vec::with_capacity(args.len());
    let mut redact_next = false;
    let login = args.iter().find(|a| !a.starts_with('-') && !a.starts_with('+')).map(|a| a == "login").unwrap_or(false);
    let mut seen_login = false;
    for a in args {
        if redact_next {
            out.push("<redacted>".to_string());
            redact_next = false;
            continue;
        }
        if VALUE_FLAGS.contains(&a.as_str()) {
            out.push(a.clone());
            redact_next = true;
            continue;
        }
        if let Some((k, _)) = a.split_once('=') {
            let key = k.trim_start_matches('-').rsplit('.').next().unwrap_or(k);
            if is_secret_name(key) && !key.eq_ignore_ascii_case("SCCACHE_DIR") {
                out.push(format!("{}=<redacted>", k));
                continue;
            }
        }
        if login {
            if seen_login && !a.starts_with('-') {
                out.push("<redacted>".to_string());
                continue;
            }
            if a == "login" {
                seen_login = true;
            }
        }
        out.push(redact_url_userinfo(a));
    }
    out
}

fn write_replace(path: &Path, bytes: &[u8], durable: bool) -> io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.{}.tmp", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), std::process::id()));
    let res = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        if durable {
            f.sync_all()?;
        }
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// Crash-safe write: temp file in the same directory, fsync, atomic rename (NFR-024).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_replace(path, bytes, true)
}

/// Atomic replace without the fsync, for hot paths where a lost record after a
/// power failure is acceptable but a torn one is not.
pub fn write_atomic_fast(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_replace(path, bytes, false)
}

pub fn write_json(path: &Path, v: &Value) -> io::Result<()> {
    let mut s = serde_json::to_vec_pretty(v).map_err(io::Error::other)?;
    s.push(b'\n');
    write_atomic(path, &s)
}

/// `write_json` without the fsync, for records whose loss after a power failure is harmless.
pub fn write_json_fast(path: &Path, v: &Value) -> io::Result<()> {
    let mut s = serde_json::to_vec_pretty(v).map_err(io::Error::other)?;
    s.push(b'\n');
    write_atomic_fast(path, &s)
}

pub fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// An exclusive lock on a file, held until dropped; waits for whoever holds it. Unix: `flock`,
/// which the kernel releases when the process ends, so a crash never leaves it held. Windows: the
/// file opened without sharing, which other openers wait out.
pub struct ExclusiveLock {
    _file: fs::File,
}

impl ExclusiveLock {
    /// The lock if nobody holds it, `None` if somebody does.
    pub fn try_acquire(path: &Path) -> io::Result<Option<ExclusiveLock>> {
        if let Some(dir) = path.parent() {
            create_private_dir(dir)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let file = fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
            loop {
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    return Ok(Some(ExclusiveLock { _file: file }));
                }
                let e = io::Error::last_os_error();
                match e.kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::WouldBlock => return Ok(None),
                    _ => return Err(e),
                }
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const ERROR_SHARING_VIOLATION: i32 = 32;
            match fs::OpenOptions::new().create(true).truncate(false).write(true).share_mode(0).open(path) {
                Ok(file) => Ok(Some(ExclusiveLock { _file: file })),
                Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => Ok(None),
                Err(e) => Err(e),
            }
        }
    }

    pub fn acquire(path: &Path) -> io::Result<ExclusiveLock> {
        if let Some(dir) = path.parent() {
            create_private_dir(dir)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let file = fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
            loop {
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                    return Ok(ExclusiveLock { _file: file });
                }
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::Interrupted {
                    return Err(e);
                }
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const ERROR_SHARING_VIOLATION: i32 = 32;
            loop {
                match fs::OpenOptions::new().create(true).truncate(false).write(true).share_mode(0).open(path) {
                    Ok(file) => return Ok(ExclusiveLock { _file: file }),
                    Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => std::thread::sleep(std::time::Duration::from_millis(10)),
                    Err(e) => return Err(e),
                }
            }
        }
    }
}

/// A shared lock on a file, held until dropped, alongside other shared holders; an exclusive
/// holder waits for all of them (Unix: `flock`). On Windows it holds the file open only.
pub struct SharedLock {
    _file: fs::File,
}

impl SharedLock {
    pub fn acquire(path: &Path) -> io::Result<SharedLock> {
        if let Some(dir) = path.parent() {
            create_private_dir(dir)?;
        }
        let file = fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            loop {
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) } == 0 {
                    break;
                }
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::Interrupted {
                    return Err(e);
                }
            }
        }
        Ok(SharedLock { _file: file })
    }
}

/// Private directories for QuickRust state (SEC-017).
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().recursive(true).mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// `p` with the home directory written `~`, as a user reads paths.
pub fn tilde(p: &Path) -> String {
    home_dir().and_then(|h| p.strip_prefix(&h).ok().map(|r| format!("~/{}", r.display()))).unwrap_or_else(|| p.display().to_string())
}

pub fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} B", b)
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

pub fn human_dur(ns: u64) -> String {
    let s = ns as f64 / 1e9;
    if s >= 60.0 {
        format!("{}m{:04.1}s", (s / 60.0) as u64, s % 60.0)
    } else if s >= 1.0 {
        format!("{:.2}s", s)
    } else {
        format!("{:.0}ms", s * 1e3)
    }
}

/// Parses `90s`, `2m`, `500ms`, `1.5h` or a bare nanosecond count.
pub fn parse_duration_ns(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mul) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1e6)
    } else if let Some(n) = s.strip_suffix("ns") {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1e9)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60e9)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600e9)
    } else {
        (s, 1.0)
    };
    num.trim().parse::<f64>().ok().filter(|v| *v >= 0.0).map(|v| (v * mul) as u64)
}

/// Parses `20GiB`, `500MB`, `1024` (bytes).
pub fn parse_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    let idx = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let n: f64 = s[..idx].parse().ok()?;
    let mul = match s[idx..].trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024.0f64.powi(4),
        _ => return None,
    };
    Some((n * mul) as u64)
}

pub fn median(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2 })
}

/// Finds an executable by name on `PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for cand in exe_candidates(&dir, name) {
            if is_executable(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

pub fn exe_candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    if cfg!(windows) {
        vec![dir.join(format!("{}.exe", name)), dir.join(name)]
    } else {
        vec![dir.join(name)]
    }
}

pub fn is_executable(p: &Path) -> bool {
    match fs::metadata(p) {
        Ok(md) if md.is_file() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                md.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                true
            }
        }
        _ => false,
    }
}

/// `(device, inode)` identity of a file, used to recognise ourselves on PATH.
pub fn file_identity(p: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(p).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        None
    }
}

pub fn is_ci() -> bool {
    ["CI", "GITHUB_ACTIONS", "GITLAB_CI", "BUILDKITE", "JENKINS_URL", "TF_BUILD", "TEAMCITY_VERSION"]
        .iter()
        .any(|k| std::env::var_os(k).map(|v| !v.is_empty() && v != "false" && v != "0").unwrap_or(false))
}

pub fn vstr(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

pub fn vu64(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

/// Walks a `a.b.c` path through nested JSON objects.
pub fn vpath<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for part in path.split('.') {
        cur = cur.get(part)?;
    }
    Some(cur)
}

#[cfg(test)]
mod tests {

    // TEST0159: Timestamps read back and read as people say them
    #[test]
    fn test0159_timestamps_read_back_and_read_as_people_say_them() {
        for ns in [0u64, 1_791_798_758_123_000_000, 951_782_400_000_000_000, 4_102_444_799_999_000_000] {
            assert_eq!(parse_iso8601(&iso8601(ns)), Some(ns), "{}", iso8601(ns));
        }
        assert_eq!(parse_iso8601("2026-10-12T21:58:09.007Z"), Some(1_791_842_289_007_000_000));
        for bad in ["2026-10-12 21:58:09Z", "2026-13-01T00:00:00Z", "2026-10-12T21:58:09", "x"] {
            assert_eq!(parse_iso8601(bad), None, "{}", bad);
        }
        let s = 1_000_000_000u64;
        let now = 1_000_000 * s;
        assert_eq!(human_ago(now - 3 * s, now), "just now");
        assert_eq!(human_ago(now - 40 * s, now), "40 seconds ago");
        assert_eq!(human_ago(now - 60 * s, now), "1 minute ago");
        assert_eq!(human_ago(now - 7_300 * s, now), "2 hours ago");
        assert_eq!(human_ago(now - 5 * 86_400 * s, now), "5 days ago");
        assert_eq!(human_ago(now + s, now), "just now", "a clock that moved back");
    }

    // TEST0160: Verbatim windows paths are written as people write them
    #[test]
    fn test0160_verbatim_windows_paths_are_written_as_people_write_them() {
        let p = |s: &str| without_verbatim_prefix(PathBuf::from(s));
        assert_eq!(p(r"\\?\D:\a\_temp\qrust-home\bin\qrust.exe"), PathBuf::from(r"D:\a\_temp\qrust-home\bin\qrust.exe"));
        assert_eq!(p(r"\\?\UNC\server\share\x"), PathBuf::from(r"\\server\share\x"));
        // Not a drive path (a volume GUID), or too long for the classic limit: as it is.
        assert_eq!(p(r"\\?\Volume{0}\x"), PathBuf::from(r"\\?\Volume{0}\x"));
        let long = format!(r"\\?\C:\{}", "d\\".repeat(200));
        assert_eq!(p(&long), PathBuf::from(&long));
        assert_eq!(p("/home/u/.qrust/bin/qrust"), PathBuf::from("/home/u/.qrust/bin/qrust"));
    }
    use super::*;

    // TEST0161: An exclusive lock waits for its holder
    #[test]
    fn test0161_an_exclusive_lock_waits_for_its_holder() {
        let dir = std::env::temp_dir().join(format!("qrust-lock-{}", new_id()));
        let path = dir.join("x.lock");
        let held = ExclusiveLock::acquire(&path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let other = path.clone();
        let waiter = std::thread::spawn(move || {
            let _l = ExclusiveLock::acquire(&other).unwrap();
            tx.send(std::time::Instant::now()).unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(rx.try_recv().is_err(), "acquired while held");
        let released = std::time::Instant::now();
        drop(held);
        assert!(rx.recv().unwrap() >= released);
        waiter.join().unwrap();
        let again = ExclusiveLock::try_acquire(&path).unwrap().expect("free once released");
        assert!(ExclusiveLock::try_acquire(&path).unwrap().is_none(), "taken while held");
        drop(again);
        fs::remove_dir_all(&dir).ok();
    }

    // TEST0162: Iso epoch and known date
    #[test]
    fn test0162_iso_epoch_and_known_date() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.000Z");
        // 2026-10-05T10:15:30.123Z
        assert_eq!(iso8601(1_791_195_330_123_000_000), "2026-10-05T10:15:30.123Z");
    }

    // TEST0163: Ids are sortable and unique
    #[test]
    fn test0163_ids_are_sortable_and_unique() {
        let a = new_id();
        let b = new_id();
        assert_eq!(a.len(), 26);
        assert_ne!(a, b);
        assert_eq!(&a[..6], &b[..6]);
    }

    // TEST0164: Redacts tokens and login
    #[test]
    fn test0164_redacts_tokens_and_login() {
        let r = redact_args(&["publish".into(), "--token".into(), "abc123".into()]);
        assert_eq!(r, vec!["publish", "--token", "<redacted>"]);
        let r = redact_args(&["login".into(), "s3cr3t".into()]);
        assert_eq!(r, vec!["login", "<redacted>"]);
        let r = redact_args(&["build".into(), "--config".into(), "registries.x.token=abc".into()]);
        assert_eq!(r[2], "registries.x.token=<redacted>");
        let r = redact_args(&["fetch".into(), "https://user:pw@example.com/x".into()]);
        assert_eq!(r[1], "https://<redacted>@example.com/x");
        let r = redact_args(&["build".into(), "--features".into(), "a,b".into()]);
        assert_eq!(r, vec!["build", "--features", "a,b"]);
    }

    // TEST0165: Tokenizer respects path boundaries
    #[test]
    fn test0165_tokenizer_respects_path_boundaries() {
        assert_eq!(replace_path_prefix("/a/repo/src", "/a/repo", "$W"), "$W/src");
        assert_eq!(replace_path_prefix("/a/repo", "/a/repo", "$W"), "$W");
        assert_eq!(replace_path_prefix("/a/repo-feature/src", "/a/repo", "$W"), "/a/repo-feature/src");
        assert_eq!(replace_path_prefix("file:///a/repo#0.1.0 and /a/repo.bak", "/a/repo", "$W"), "file://$W#0.1.0 and /a/repo.bak");
        assert_eq!(replace_path_prefix("-L /a/repo/t -L /a/repo2/t", "/a/repo", "$W"), "-L $W/t -L /a/repo2/t");
    }

    // TEST0166: Parses units
    #[test]
    fn test0166_parses_units() {
        assert_eq!(parse_duration_ns("2s"), Some(2_000_000_000));
        assert_eq!(parse_duration_ns("500ms"), Some(500_000_000));
        assert_eq!(parse_bytes("1GiB"), Some(1 << 30));
        assert_eq!(median(&[3, 1, 2]), Some(2));
        assert_eq!(median(&[]), None);
    }
}
