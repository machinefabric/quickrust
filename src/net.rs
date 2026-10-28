//! QuickRust's network access: one HTTP client for toolchain manifests and components, registry index
//! lines and crate files.
//!
//! Downloads are verified and resumable: a file is written to `<dest>.part` and renamed to `dest`
//! only once its SHA-256 matches the one published with it. A transfer that breaks off keeps its
//! part, and the next attempt asks for the rest (`Range`); a server that answers a range request
//! with the whole file starts it over. A part whose resumed content does not verify is fetched
//! once more from its first byte: only a fresh download that does not verify is an error.

use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Bodies read into memory (manifests, index lines, release notes) are bounded: a larger one is a
/// server error, not something to hold.
const MAX_IN_MEMORY: u64 = 64 << 20;
/// Attempts of one download whose transfer breaks off (each resumes where the last stopped).
const ATTEMPTS: u32 = 4;
/// What every request says it comes from: QuickRust and its version, and nothing about the user.
const USER_AGENT: &str = concat!("qrust/", env!("CARGO_PKG_VERSION"));

#[derive(Debug)]
pub enum Error {
    /// The server answered with a status that is not success.
    Status { url: String, status: u16 },
    /// The connection could not be made or broke off.
    Transport { url: String, message: String },
    /// A downloaded file is not the one published.
    Checksum { url: String, expected: String, actual: String },
    /// A body larger than QuickRust reads into memory.
    TooLarge { url: String, limit: u64 },
    /// Writing the download failed.
    Io { path: PathBuf, error: io::Error },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Status { url, status } => write!(f, "{} answered {}", url, status),
            Error::Transport { url, message } => write!(f, "could not download {}: {}", url, message),
            Error::Checksum { url, expected, actual } => write!(f, "{} is not the published file: its SHA-256 is {}, the published one {}", url, actual, expected),
            Error::TooLarge { url, limit } => write!(f, "{} is larger than {} bytes", url, limit),
            Error::Io { path, error } => write!(f, "could not write {}: {}", path.display(), error),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// Worth another attempt: the connection, or the server, failed rather than the request.
    fn transient(&self) -> bool {
        match self {
            Error::Transport { .. } => true,
            Error::Status { status, .. } => *status == 408 || *status == 429 || *status >= 500,
            _ => false,
        }
    }
}

/// The HTTP client: proxies from the environment (`https_proxy`, `ALL_PROXY`, `no_proxy`),
/// connection reuse, and bounded waits.
#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    /// Attempts of a request that fails transiently.
    attempts: u32,
}

/// The answer to a conditional GET.
pub enum Fetched {
    Body { bytes: Vec<u8>, etag: Option<String> },
    NotModified,
}

impl Default for Client {
    fn default() -> Self {
        Client::new()
    }
}

impl Client {
    pub fn new() -> Client {
        let agent = ureq::AgentBuilder::new()
            .try_proxy_from_env(true)
            .timeout_connect(Duration::from_secs(30))
            .timeout_read(Duration::from_secs(60))
            .user_agent(USER_AGENT)
            .build();
        Client { agent, attempts: ATTEMPTS }
    }

    /// A client for a request nobody waits on and that is asked again later anyway: each request
    /// is one attempt, over within `whole` from the connection to the last byte of its answer.
    pub fn bounded(whole: Duration) -> Client {
        let agent = ureq::AgentBuilder::new().try_proxy_from_env(true).timeout_connect(whole).timeout(whole).user_agent(USER_AGENT).build();
        Client { agent, attempts: 1 }
    }

    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<ureq::Response, Error> {
        let mut req = self.agent.get(url);
        for (k, v) in headers {
            req = req.set(k, v);
        }
        match req.call() {
            Ok(resp) => Ok(resp),
            Err(ureq::Error::Status(status, _)) => Err(Error::Status { url: url.to_string(), status }),
            Err(ureq::Error::Transport(t)) => Err(Error::Transport { url: url.to_string(), message: t.to_string() }),
        }
    }

    /// The body of `url`, read into memory.
    pub fn get_bytes(&self, url: &str, headers: &[(&str, &str)]) -> Result<Vec<u8>, Error> {
        retrying(self.attempts, || read_body(url, self.get(url, headers)?))
    }

    /// The body of `url` unless it still has the entity tag `etag` (HTTP 304).
    pub fn get_if_changed(&self, url: &str, etag: Option<&str>, headers: &[(&str, &str)]) -> Result<Fetched, Error> {
        retrying(self.attempts, || {
            let mut all: Vec<(&str, &str)> = headers.to_vec();
            if let Some(tag) = etag {
                all.push(("If-None-Match", tag));
            }
            let resp = self.get(url, &all)?;
            if resp.status() == 304 {
                return Ok(Fetched::NotModified);
            }
            let etag = resp.header("ETag").map(str::to_string);
            Ok(Fetched::Body { bytes: read_body(url, resp)?, etag })
        })
    }

    /// Downloads `url` to `dest`, verified against `sha256` (hex), resuming a part an earlier
    /// attempt left. `progress` sees the bytes received so far and the total when known.
    pub fn download(&self, url: &str, dest: &Path, sha256: &str, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<(), Error> {
        let part = part_path(dest);
        let io_err = |path: &Path| { let path = path.to_path_buf(); move |error| Error::Io { path, error } };
        if let Some(dir) = dest.parent() {
            fs::create_dir_all(dir).map_err(io_err(dir))?;
        }
        let mut fresh = !part.exists();
        loop {
            let actual = retrying(self.attempts, || self.transfer(url, &part, progress))?;
            if actual.eq_ignore_ascii_case(sha256) {
                fs::rename(&part, dest).map_err(io_err(dest))?;
                return Ok(());
            }
            fs::remove_file(&part).map_err(io_err(&part))?;
            if fresh {
                return Err(Error::Checksum { url: url.to_string(), expected: sha256.to_ascii_lowercase(), actual });
            }
            // What an earlier attempt left was not the start of this file: once more from byte 0.
            fresh = true;
        }
    }

    /// Completes `part` from `url` and returns the SHA-256 of its content.
    fn transfer(&self, url: &str, part: &Path, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<String, Error> {
        let io_err = |error| Error::Io { path: part.to_path_buf(), error };
        let have = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        let range = format!("bytes={}-", have);
        let ranged = [("Range", range.as_str())];
        let resp = match self.get(url, if have > 0 { &ranged } else { &[] }) {
            // The part is the whole file already.
            Err(Error::Status { status: 416, .. }) if have > 0 => return sha256_of(part).map_err(io_err),
            other => other?,
        };
        let resumed = have > 0 && resp.status() == 206 && content_range_start(&resp) == Some(have);
        let start = if resumed { have } else { 0 };
        let total = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok()).map(|n| n + start);
        let mut hasher = Sha256::new();
        let mut file = if resumed {
            hash_into(part, &mut hasher).map_err(io_err)?;
            fs::OpenOptions::new().append(true).open(part).map_err(io_err)?
        } else {
            fs::File::create(part).map_err(io_err)?
        };
        let mut received = start;
        progress(received, total);
        let mut body = resp.into_reader();
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = match body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    file.flush().map_err(io_err)?;
                    return Err(Error::Transport { url: url.to_string(), message: e.to_string() });
                }
            };
            file.write_all(&buf[..n]).map_err(io_err)?;
            hasher.update(&buf[..n]);
            received += n as u64;
            progress(received, total);
        }
        file.sync_all().map_err(io_err)?;
        if let Some(total) = total.filter(|t| *t != received) {
            return Err(Error::Transport { url: url.to_string(), message: format!("the transfer ended after {} of {} bytes", received, total) });
        }
        Ok(crate::util::hex(&hasher.finalize()))
    }
}

/// `<dest>.part`: where a download is written until it verifies.
pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

fn read_body(url: &str, resp: ureq::Response) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    resp.into_reader().take(MAX_IN_MEMORY + 1).read_to_end(&mut bytes).map_err(|e| Error::Transport { url: url.to_string(), message: e.to_string() })?;
    if bytes.len() as u64 > MAX_IN_MEMORY {
        return Err(Error::TooLarge { url: url.to_string(), limit: MAX_IN_MEMORY });
    }
    Ok(bytes)
}

/// Runs `attempt` until it succeeds, fails for good, or has failed transiently `attempts` times.
fn retrying<T>(attempts: u32, mut attempt: impl FnMut() -> Result<T, Error>) -> Result<T, Error> {
    let mut n = 1;
    loop {
        match attempt() {
            Err(e) if e.transient() && n < attempts => {
                std::thread::sleep(Duration::from_millis(250 << n));
                n += 1;
            }
            other => return other,
        }
    }
}

/// The first byte of a `206` answer (`Content-Range: bytes <start>-<end>/<total>`).
fn content_range_start(resp: &ureq::Response) -> Option<u64> {
    let v = resp.header("Content-Range")?;
    v.trim().strip_prefix("bytes ")?.split('-').next()?.trim().parse().ok()
}

fn hash_into(path: &Path, hasher: &mut Sha256) -> io::Result<()> {
    let mut f = fs::File::open(path)?;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        hasher.update(&buf[..n]);
    }
}

fn sha256_of(path: &Path) -> io::Result<String> {
    let mut h = Sha256::new();
    hash_into(path, &mut h)?;
    Ok(crate::util::hex(&h.finalize()))
}

#[cfg(test)]
pub mod testserver {
    //! A minimal HTTP/1.1 server for tests: serves fixed bodies, honours `Range`, and can be told
    //! to break a transfer off or to ignore ranges.

    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct Route {
        pub body: Vec<u8>,
        /// Close the connection after this many body bytes, for this many requests.
        pub cut_after: Option<(usize, usize)>,
        pub ignore_range: bool,
        pub status: Option<u16>,
        pub etag: Option<String>,
    }

    #[derive(Default)]
    pub struct State {
        pub routes: HashMap<String, Route>,
        /// `(path, Range header)` of every request served.
        pub log: Vec<(String, Option<String>)>,
    }

    pub struct Server {
        pub base: String,
        pub state: Arc<Mutex<State>>,
    }

    impl Server {
        pub fn start() -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let state = Arc::new(Mutex::new(State::default()));
            let shared = state.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let state = shared.clone();
                    std::thread::spawn(move || serve(stream, &state));
                }
            });
            Server { base, state }
        }

        pub fn route(&self, path: &str, route: Route) {
            self.state.lock().unwrap().routes.insert(path.to_string(), route);
        }

        pub fn url(&self, path: &str) -> String {
            format!("{}{}", self.base, path)
        }

        pub fn requests(&self, path: &str) -> Vec<Option<String>> {
            self.state.lock().unwrap().log.iter().filter(|(p, _)| p == path).map(|(_, r)| r.clone()).collect()
        }
    }

    fn serve(stream: TcpStream, state: &Mutex<State>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut out = stream;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let mut headers = HashMap::new();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 {
                    return;
                }
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                }
            }
            let range = headers.get("range").cloned();
            let (route, cut) = {
                let mut st = state.lock().unwrap();
                st.log.push((path.clone(), range.clone()));
                let Some(route) = st.routes.get_mut(&path) else {
                    let _ = out.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                    continue;
                };
                let cut = match &mut route.cut_after {
                    Some((after, times)) if *times > 0 => {
                        *times -= 1;
                        Some(*after)
                    }
                    _ => None,
                };
                (route.clone(), cut)
            };
            if let Some(status) = route.status {
                let _ = write!(out, "HTTP/1.1 {} Status\r\nContent-Length: 0\r\n\r\n", status);
                continue;
            }
            if let (Some(tag), Some(have)) = (&route.etag, headers.get("if-none-match")) {
                if tag == have {
                    let _ = write!(out, "HTTP/1.1 304 Not Modified\r\nETag: {}\r\n\r\n", tag);
                    continue;
                }
            }
            let start = range.as_deref().filter(|_| !route.ignore_range).and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.trim_end_matches('-').parse::<usize>().ok());
            let etag = route.etag.as_ref().map(|t| format!("ETag: {}\r\n", t)).unwrap_or_default();
            let body = match start {
                Some(s) if s >= route.body.len() => {
                    let _ = write!(out, "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\n\r\n");
                    continue;
                }
                Some(s) => {
                    let _ = write!(out, "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\n{}\r\n", route.body.len() - s, s, route.body.len() - 1, route.body.len(), etag);
                    &route.body[s..]
                }
                None => {
                    let _ = write!(out, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{}\r\n", route.body.len(), etag);
                    &route.body[..]
                }
            };
            match cut {
                Some(n) => {
                    let _ = out.write_all(&body[..n.min(body.len())]);
                    let _ = out.flush();
                    return;
                }
                None => {
                    let _ = out.write_all(body);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testserver::{Route, Server};
    use super::*;

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("qrust-net-{}", crate::util::new_id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn body(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 % 251) as u8).collect()
    }

    // TEST0063: A download that breaks off resumes where it stopped
    #[test]
    fn test0063_a_download_that_breaks_off_resumes_where_it_stopped() {
        let srv = Server::start();
        let data = body(3 << 20);
        srv.route("/f", Route { body: data.clone(), cut_after: Some((1 << 20, 1)), ..Default::default() });
        let dest = scratch().join("f");
        let mut seen = 0;
        Client::new().download(&srv.url("/f"), &dest, &crate::util::sha256_hex(&data), &mut |n, _| seen = n).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), data);
        assert!(!part_path(&dest).exists());
        assert_eq!(seen, data.len() as u64);
        let reqs = srv.requests("/f");
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[1].as_deref(), Some("bytes=1048576-"));
    }

    // TEST0064: A part left by another process is completed and a server ignoring ranges starts over
    #[test]
    fn test0064_a_part_left_by_another_process_is_completed_and_a_server_ignoring_ranges_starts_over() {
        let srv = Server::start();
        let data = body(100_000);
        let sum = crate::util::sha256_hex(&data);
        let dir = scratch();
        let dest = dir.join("a");
        fs::write(part_path(&dest), &data[..40_000]).unwrap();
        srv.route("/a", Route { body: data.clone(), ..Default::default() });
        Client::new().download(&srv.url("/a"), &dest, &sum, &mut |_, _| {}).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), data);
        let dest = dir.join("b");
        fs::write(part_path(&dest), &data[..40_000]).unwrap();
        srv.route("/b", Route { body: data.clone(), ignore_range: true, ..Default::default() });
        Client::new().download(&srv.url("/b"), &dest, &sum, &mut |_, _| {}).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), data);
    }

    // TEST0065: A stale part is fetched again and only a fresh mismatch fails
    #[test]
    fn test0065_a_stale_part_is_fetched_again_and_only_a_fresh_mismatch_fails() {
        let srv = Server::start();
        let data = body(50_000);
        let dir = scratch();
        // A part that is not the start of this file (an older release under the same name).
        let dest = dir.join("c");
        fs::write(part_path(&dest), body(60_000).iter().map(|b| b ^ 0xff).take(20_000).collect::<Vec<_>>()).unwrap();
        srv.route("/c", Route { body: data.clone(), ..Default::default() });
        Client::new().download(&srv.url("/c"), &dest, &crate::util::sha256_hex(&data), &mut |_, _| {}).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), data);
        assert_eq!(srv.requests("/c"), vec![Some("bytes=20000-".to_string()), None]);
        // A part as long as the file, but wrong: the 416 answer leads to a fresh download.
        let dest = dir.join("d");
        fs::write(part_path(&dest), vec![0u8; 50_000]).unwrap();
        srv.route("/d", Route { body: data.clone(), ..Default::default() });
        Client::new().download(&srv.url("/d"), &dest, &crate::util::sha256_hex(&data), &mut |_, _| {}).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), data);
        // A fresh download that does not verify is an error, and leaves nothing behind.
        let dest = dir.join("e");
        srv.route("/e", Route { body: data.clone(), ..Default::default() });
        let wrong = "0".repeat(64);
        match Client::new().download(&srv.url("/e"), &dest, &wrong, &mut |_, _| {}) {
            Err(Error::Checksum { expected, actual, .. }) => {
                assert_eq!(expected, wrong);
                assert_eq!(actual, crate::util::sha256_hex(&data));
            }
            other => panic!("{:?}", other.map(|_| ())),
        }
        assert!(!dest.exists() && !part_path(&dest).exists());
    }

    // TEST0066: Missing files fail at once and conditional gets use the entity tag
    #[test]
    fn test0066_missing_files_fail_at_once_and_conditional_gets_use_the_entity_tag() {
        let srv = Server::start();
        let c = Client::new();
        match c.get_bytes(&srv.url("/nothing"), &[]) {
            Err(Error::Status { status: 404, .. }) => {}
            other => panic!("{:?}", other.map(|_| ())),
        }
        assert_eq!(srv.requests("/nothing").len(), 1, "a 404 is not retried");
        srv.route("/i", Route { body: b"line\n".to_vec(), etag: Some("\"v1\"".into()), ..Default::default() });
        match c.get_if_changed(&srv.url("/i"), None, &[]).unwrap() {
            Fetched::Body { bytes, etag } => {
                assert_eq!(bytes, b"line\n");
                assert_eq!(etag.as_deref(), Some("\"v1\""));
            }
            Fetched::NotModified => panic!("no tag was sent"),
        }
        assert!(matches!(c.get_if_changed(&srv.url("/i"), Some("\"v1\""), &[]).unwrap(), Fetched::NotModified));
        assert!(matches!(c.get_if_changed(&srv.url("/i"), Some("\"v0\""), &[]).unwrap(), Fetched::Body { .. }));
    }

    // TEST0067: A server error is retried and reported when it persists
    #[test]
    fn test0067_a_server_error_is_retried_and_reported_when_it_persists() {
        let srv = Server::start();
        srv.route("/busy", Route { status: Some(503), ..Default::default() });
        match Client::new().get_bytes(&srv.url("/busy"), &[]) {
            Err(Error::Status { status: 503, .. }) => {}
            other => panic!("{:?}", other.map(|_| ())),
        }
        assert_eq!(srv.requests("/busy").len(), ATTEMPTS as usize);
    }
}
