//! A crates registry for the suites: Cargo's sparse protocol over HTTP from a thread of the test,
//! with crates made in the test. Haul's Cargo home in a sandbox points crates.io at it, so
//! fixtures with dependencies build offline, through the same downloads Cargo makes from
//! crates.io.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    files: HashMap<String, Vec<u8>>,
    requests: Vec<String>,
}

pub struct Registry {
    pub url: String,
    state: Arc<Mutex<State>>,
}

/// A crate to publish: its files (relative to the package root) and its dependencies.
pub struct Crate<'a> {
    pub name: &'a str,
    pub vers: &'a str,
    pub files: Vec<(&'a str, String)>,
    /// `(name, requirement)`.
    pub deps: Vec<(&'a str, &'a str)>,
}

impl<'a> Crate<'a> {
    /// A library crate whose `lib.rs` is `lib`.
    pub fn lib(name: &'a str, vers: &'a str, lib: &str) -> Crate<'a> {
        Crate { name, vers, files: vec![("src/lib.rs", lib.to_string())], deps: vec![] }
    }

    /// A crate with a binary printing `says`.
    pub fn bin(name: &'a str, vers: &'a str, says: &str) -> Crate<'a> {
        Crate { name, vers, files: vec![("src/main.rs", format!("fn main() {{\n    println!(\"{}\");\n}}\n", says))], deps: vec![] }
    }
}

fn index_path(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    match n.len() {
        1 => format!("1/{}", n),
        2 => format!("2/{}", n),
        3 => format!("3/{}/{}", &n[..1], n),
        _ => format!("{}/{}/{}", &n[..2], &n[2..4], n),
    }
}

impl Registry {
    pub fn start() -> Registry {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        state.lock().unwrap().files.insert("/index/config.json".into(), format!("{{\"dl\":\"{}/dl\"}}", url).into_bytes());
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let st = shared.clone();
                std::thread::spawn(move || serve(stream, &st));
            }
        });
        Registry { url, state }
    }

    /// Publishes `c`: its `.crate` (a real package archive) and its index line. Its checksum
    /// (as a lockfile records it).
    pub fn publish(&self, c: &Crate) -> String {
        let mut manifest = format!("[package]\nname = \"{}\"\nversion = \"{}\"\nedition = \"2021\"\n", c.name, c.vers);
        if !c.deps.is_empty() {
            manifest.push_str("\n[dependencies]\n");
            for (n, req) in &c.deps {
                manifest.push_str(&format!("{} = \"{}\"\n", n, req));
            }
        }
        let root = format!("{}-{}", c.name, c.vers);
        let mut b = tar::Builder::new(Vec::new());
        let mut add = |path: &str, data: &[u8]| {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_mtime(1_700_000_000);
            b.append_data(&mut h, format!("{}/{}", root, path), data).unwrap();
        };
        add("Cargo.toml", manifest.as_bytes());
        for (p, text) in &c.files {
            add(p, text.as_bytes());
        }
        let tar = b.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();
        let bytes = gz.finish().unwrap();
        let cksum = sha256(&bytes);
        let deps: Vec<String> = c.deps.iter().map(|(n, req)| format!("{{\"name\":\"{}\",\"req\":\"{}\",\"features\":[],\"optional\":false,\"default_features\":true,\"target\":null,\"kind\":\"normal\"}}", n, req)).collect();
        let line = format!("{{\"name\":\"{}\",\"vers\":\"{}\",\"deps\":[{}],\"cksum\":\"{}\",\"features\":{{}},\"yanked\":false}}\n", c.name, c.vers, deps.join(","), cksum);
        let mut st = self.state.lock().unwrap();
        st.files.insert(format!("/dl/{}/{}/download", c.name, c.vers), bytes);
        st.files.entry(format!("/index/{}", index_path(c.name))).or_default().extend_from_slice(line.as_bytes());
        cksum
    }

    /// Serves `bytes` at `path` (any file: the server is a plain one).
    pub fn put(&self, path: &str, bytes: Vec<u8>) {
        self.state.lock().unwrap().files.insert(path.to_string(), bytes);
    }

    /// Paths requested so far.
    pub fn requests(&self) -> Vec<String> {
        self.state.lock().unwrap().requests.clone()
    }

    /// The Cargo configuration that makes crates.io this registry.
    pub fn cargo_config(&self) -> String {
        format!("[source.crates-io]\nreplace-with = \"haul-test\"\n\n[source.haul-test]\nregistry = \"sparse+{}/index/\"\n\n[net]\nretry = 0\n", self.url)
    }

    /// Makes crates.io this registry for the Haul home `home`.
    pub fn serve_crates_io_for(&self, home: &Path) {
        std::fs::create_dir_all(home.join("cargo")).unwrap();
        std::fs::write(home.join("cargo/config.toml"), self.cargo_config()).unwrap();
    }
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{:02x}", b)).collect()
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
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim_end().is_empty() {
                break;
            }
        }
        let body = {
            let mut st = state.lock().unwrap();
            st.requests.push(path.clone());
            st.files.get(&path).cloned()
        };
        let ok = match body {
            Some(b) => write!(out, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", b.len()).and_then(|_| out.write_all(&b)),
            None => out.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"),
        };
        if ok.is_err() {
            return;
        }
    }
}
