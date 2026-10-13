//! Builds QuickRust's release archives.
//!
//! An archive is `qrust-<version>-<platform>.tar.gz` (`.zip` for Windows) and unpacks to one
//! directory, `quickrust/`, holding the two programs of that platform (`qrust` and
//! `qrust-rustc-wrapper`), the licences and the README. The programs are built with their release
//! line baked in (`build.rs`): the registry they are published in, their channel, and the keys a
//! release must be signed under. An archive without one would install a QuickRust that can never
//! update itself, so none is built without one.
//!
//! - `quickrust-dist platforms`: what a release is made of, as JSON.
//! - `quickrust-dist release --platform <platform> --out-dir <dir> [--work-dir <dir>]`: one
//!   archive, whose path is printed.
//!
//! The macOS programs are linked by the host's toolchain, so they are built on a Mac. The Linux
//! programs are static (musl) and the Windows ones are for the GNU ABI; both are linked by zig
//! (`cargo zigbuild`), so one machine builds every platform.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

type Result<T> = std::result::Result<T, String>;

/// The platforms QuickRust is published for (the release registry's names) and their targets.
pub const PLATFORMS: &[(&str, &str)] = &[
    ("darwin-arm64", "aarch64-apple-darwin"),
    ("darwin-x86_64", "x86_64-apple-darwin"),
    ("linux-x86_64", "x86_64-unknown-linux-musl"),
    ("linux-arm64", "aarch64-unknown-linux-musl"),
    ("windows-x86_64", "x86_64-pc-windows-gnu"),
];

/// The directory every archive unpacks to.
const ROOT: &str = "quickrust";
const PROGRAMS: [&str; 2] = ["qrust", "qrust-rustc-wrapper"];
const DOCUMENTS: [&str; 3] = ["LICENSE-MIT", "LICENSE-APACHE", "README.md"];
/// What a release build is given, and `build.rs` bakes: all four, or it is not a release build.
const RELEASE_LINE: [&str; 4] = ["MFR_RELEASES_BASE_URL", "MFR_RELEASE_CHANNEL", "MFR_SIGNING_ENVIRONMENT", "MFR_CARTRIDGE_ROOT_PUBKEYS"];
/// The oldest macOS the programs run on: the oldest the Rust toolchains they install run on.
const MACOS_DEPLOYMENT_TARGET: &str = "11.0";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let done = match args.first().map(String::as_str) {
        Some("platforms") if args.len() == 1 => platforms(),
        Some("release") => release_args(&args[1..]).and_then(|(platform, out_dir, work_dir)| release(&platform, &out_dir, &work_dir)),
        _ => Err("usage: quickrust-dist platforms | quickrust-dist release --platform <platform> --out-dir <dir> [--work-dir <dir>]".into()),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("quickrust-dist: {e}");
            ExitCode::FAILURE
        }
    }
}

fn release_args(args: &[String]) -> Result<(String, PathBuf, PathBuf)> {
    let (mut platform, mut out_dir, mut work_dir) = (None, None, None);
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let slot = match flag.as_str() {
            "--platform" => &mut platform,
            "--out-dir" => &mut out_dir,
            "--work-dir" => &mut work_dir,
            other => return Err(format!("unknown argument {other:?}")),
        };
        if slot.is_some() {
            return Err(format!("{flag} is given twice"));
        }
        *slot = Some(rest.next().ok_or_else(|| format!("{flag} takes a value"))?.clone());
    }
    let platform = platform.ok_or("release takes --platform")?;
    let out_dir = PathBuf::from(out_dir.ok_or("release takes --out-dir")?);
    // Beside the build that runs this tool, unless told otherwise.
    let work_dir = work_dir.map(PathBuf::from).unwrap_or_else(|| std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| repo().join("target")).join("quickrust-dist"));
    Ok((platform, out_dir, work_dir))
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn io<T>(what: impl std::fmt::Display, r: std::io::Result<T>) -> Result<T> {
    r.map_err(|e| format!("{what}: {e}"))
}

fn run(cmd: &mut Command) -> Result<std::process::Output> {
    let shown = format!("{cmd:?}");
    let out = io(format!("cannot run {shown}"), cmd.stderr(std::process::Stdio::inherit()).output())?;
    if !out.status.success() {
        return Err(format!("{shown} failed ({})", out.status));
    }
    Ok(out)
}

pub fn archive_name(version: &str, platform: &str) -> String {
    format!("qrust-{version}-{platform}{}", if platform.starts_with("windows-") { ".zip" } else { ".tar.gz" })
}

/// The version being released: `version.txt`, which the package's own version must agree with
/// (the programs report the package's).
fn version(repo: &Path) -> Result<String> {
    let file = repo.join("version.txt");
    let version = io(format!("cannot read {}", file.display()), fs::read_to_string(&file))?.trim().to_owned();
    let manifest = repo.join("Cargo.toml");
    let text = io(format!("cannot read {}", manifest.display()), fs::read_to_string(&manifest))?;
    let parsed: toml::Table = text.parse().map_err(|e| format!("{}: {e}", manifest.display()))?;
    let package = parsed.get("package").and_then(|p| p.get("version")).and_then(|v| v.as_str()).ok_or_else(|| format!("{} has no package version", manifest.display()))?;
    if package != version {
        return Err(format!("version.txt says {version} and Cargo.toml says {package}: a release has one version"));
    }
    Ok(version)
}

fn platforms() -> Result<()> {
    let version = version(&repo())?;
    let all: serde_json::Map<String, serde_json::Value> =
        PLATFORMS.iter().map(|(platform, target)| (platform.to_string(), serde_json::json!({ "target": target, "name": archive_name(&version, platform) }))).collect();
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "version": version, "platforms": all })).expect("JSON"));
    Ok(())
}

/// The release line this build is given, as `build.rs` bakes it into the programs: the registry,
/// the channel, the signing environment and the roots, with a space between them.
fn release_line(get: &dyn Fn(&str) -> Option<String>) -> Result<String> {
    let given: Vec<(&str, String)> = RELEASE_LINE.iter().map(|name| (*name, get(name).unwrap_or_default().trim().to_owned())).collect();
    let missing: Vec<&str> = given.iter().filter(|(_, v)| v.is_empty()).map(|(n, _)| *n).collect();
    if !missing.is_empty() {
        return Err(format!(
            "a release archive is built with its release line ({}), and {} {} not set: a QuickRust without one cannot update itself",
            RELEASE_LINE.join(", "),
            missing.join(", "),
            if missing.len() == 1 { "is" } else { "are" }
        ));
    }
    let roots: Vec<&str> = given[3].1.split(',').map(str::trim).collect();
    Ok(format!("{} {} {} {}", given[0].1, given[1].1, given[2].1, roots.join(",")))
}

/// Whether the program in `bytes` has `line` baked in.
fn carries(bytes: &[u8], line: &str) -> bool {
    let line = line.as_bytes();
    !line.is_empty() && bytes.windows(line.len()).any(|w| w == line)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Linker {
    Host,
    Zig,
}

fn linker_for(target: &str) -> Result<Linker> {
    if target.ends_with("-apple-darwin") {
        if !cfg!(target_os = "macos") {
            return Err(format!("{target} is linked by Apple's toolchain: build it on a Mac"));
        }
        return Ok(Linker::Host);
    }
    for (tool, hint) in [("zig", "zig links the Linux and Windows programs"), ("cargo-zigbuild", "`cargo install --locked cargo-zigbuild`")] {
        let asked = Command::new(tool).arg(if tool == "zig" { "version" } else { "--version" }).output();
        if !asked.is_ok_and(|o| o.status.success()) {
            return Err(format!("{target} is linked by zig through `cargo zigbuild`, and {tool} is not installed ({hint})"));
        }
    }
    Ok(Linker::Zig)
}

/// Where Cargo keeps the sources of registry crates, read from the dependency graph: the paths a
/// release must not carry are the ones the build actually reads from.
fn registry_home(repo: &Path) -> Result<Option<PathBuf>> {
    let out = run(Command::new("cargo").current_dir(repo).args(["metadata", "--format-version", "1", "--locked"]))?;
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {e}"))?;
    let packages = meta["packages"].as_array().ok_or("cargo metadata lists no packages")?;
    for package in packages {
        let manifest = package["manifest_path"].as_str().unwrap_or_default();
        for marker in ["/registry/src/", "\\registry\\src\\"] {
            if let Some(at) = manifest.find(marker) {
                return Ok(Some(PathBuf::from(&manifest[..at])));
            }
        }
    }
    Ok(None)
}

/// Flags that replace this machine's paths in the programs (in panic messages) with fixed ones:
/// a release does not say where it was built.
fn remapped_paths(repo: &Path) -> Result<Vec<String>> {
    let sysroot = run(Command::new("rustc").current_dir(repo).args(["--print", "sysroot"]))?;
    let sysroot = PathBuf::from(String::from_utf8_lossy(&sysroot.stdout).trim());
    let repo = io("cannot resolve the repository", fs::canonicalize(repo))?;
    let mut pairs = vec![(sysroot, "/rustc"), (repo.clone(), "/quickrust")];
    if let Some(home) = registry_home(&repo)? {
        pairs.push((home, "/cargo"));
    }
    Ok(pairs.into_iter().map(|(from, to)| format!("--remap-path-prefix={}={to}", from.display())).collect())
}

/// Builds the two programs for `target` and returns the directory they are in.
fn build(repo: &Path, target: &str, linker: Linker, work: &Path) -> Result<PathBuf> {
    let target_dir = work.join("build");
    let mut cmd = Command::new("cargo");
    cmd.current_dir(repo).env("CARGO_TARGET_DIR", &target_dir).env("CARGO_ENCODED_RUSTFLAGS", remapped_paths(repo)?.join("\u{1f}"));
    if target.ends_with("-apple-darwin") {
        cmd.env("MACOSX_DEPLOYMENT_TARGET", MACOS_DEPLOYMENT_TARGET);
    }
    cmd.arg(match linker {
        Linker::Host => "build",
        Linker::Zig => "zigbuild",
    });
    cmd.args(["--release", "--locked", "-p", "quickrust", "--target", target]);
    for program in PROGRAMS {
        cmd.args(["--bin", program]);
    }
    run(&mut cmd)?;
    Ok(target_dir.join(target).join("release"))
}

/// One file of an archive: its name in `quickrust/`, where it is, and whether it is a program.
struct Entry {
    name: String,
    from: PathBuf,
    program: bool,
}

fn entries(repo: &Path, built: &Path, target: &str) -> Vec<Entry> {
    let exe = if target.contains("-windows-") { ".exe" } else { "" };
    let mut entries: Vec<Entry> = PROGRAMS.iter().map(|p| Entry { name: format!("{p}{exe}"), from: built.join(format!("{p}{exe}")), program: true }).collect();
    entries.extend(DOCUMENTS.iter().map(|d| Entry { name: d.to_string(), from: repo.join(d), program: false }));
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// A `.tar.gz` of `entries` under `quickrust/`. The same entries make the same bytes: names in
/// order, no times, no owner.
fn tar_gz(entries: &[Entry], out: &Path) -> Result<()> {
    let file = io(format!("cannot create {}", out.display()), fs::File::create(out))?;
    let gz = flate2::GzBuilder::new().mtime(0).write(file, flate2::Compression::best());
    let mut b = tar::Builder::new(gz);
    let mut dir = tar::Header::new_gnu();
    dir.set_entry_type(tar::EntryType::Directory);
    dir.set_mode(0o755);
    dir.set_size(0);
    dir.set_mtime(0);
    dir.set_uid(0);
    dir.set_gid(0);
    io("cannot archive", b.append_data(&mut dir, format!("{ROOT}/"), std::io::empty()))?;
    for entry in entries {
        let bytes = io(format!("cannot read {}", entry.from.display()), fs::read(&entry.from))?;
        let mut h = tar::Header::new_gnu();
        h.set_mode(if entry.program { 0o755 } else { 0o644 });
        h.set_size(bytes.len() as u64);
        h.set_mtime(0);
        h.set_uid(0);
        h.set_gid(0);
        io("cannot archive", b.append_data(&mut h, format!("{ROOT}/{}", entry.name), bytes.as_slice()))?;
    }
    let gz = io("cannot finish the archive", b.into_inner())?;
    io("cannot finish the archive", gz.finish().map(|_| ()))
}

/// A `.zip` of `entries` under `quickrust/`, with fixed times.
fn zip(entries: &[Entry], out: &Path) -> Result<()> {
    let file = io(format!("cannot create {}", out.display()), fs::File::create(out))?;
    let mut z = zip::ZipWriter::new(file);
    let options = |mode: u32| zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).last_modified_time(zip::DateTime::default()).unix_permissions(mode);
    for entry in entries {
        z.start_file(format!("{ROOT}/{}", entry.name), options(if entry.program { 0o755 } else { 0o644 })).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
        let bytes = io(format!("cannot read {}", entry.from.display()), fs::read(&entry.from))?;
        io(format!("cannot write {}", out.display()), z.write_all(&bytes))?;
    }
    z.finish().map(|_| ()).map_err(|e| format!("cannot finish {}: {e}", out.display()))
}

/// The platform of the machine this runs on, where its programs run here.
fn host_platform() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-arm64",
        ("macos", "x86_64") => "darwin-x86_64",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-arm64",
        ("windows", "x86_64") => "windows-x86_64",
        _ => return None,
    })
}

fn release(platform: &str, out_dir: &Path, work_dir: &Path) -> Result<()> {
    let target = PLATFORMS.iter().find(|(p, _)| *p == platform).map(|(_, t)| *t).ok_or_else(|| {
        let known: Vec<&str> = PLATFORMS.iter().map(|(p, _)| *p).collect();
        format!("{platform} is not a platform QuickRust is published for (platforms: {})", known.join(", "))
    })?;
    let repo = io("cannot resolve the repository", fs::canonicalize(repo()))?;
    let version = version(&repo)?;
    let line = release_line(&|name| std::env::var(name).ok())?;
    let linker = linker_for(target)?;
    io(format!("cannot create {}", work_dir.display()), fs::create_dir_all(work_dir))?;
    let work = io("cannot resolve the work directory", fs::canonicalize(work_dir))?;
    let built = build(&repo, target, linker, &work)?;
    let entries = entries(&repo, &built, target);
    for entry in entries.iter().filter(|e| e.program) {
        let bytes = io(format!("cannot read {}", entry.from.display()), fs::read(&entry.from))?;
        // Only `qrust` updates itself; the wrapper has no release line to carry.
        if entry.name.starts_with("qrust-rustc-wrapper") {
            continue;
        }
        if !carries(&bytes, &line) {
            return Err(format!("{} was built without the release line it was given ({line})", entry.from.display()));
        }
    }
    // What runs here says which release it is.
    if host_platform() == Some(platform) {
        let qrust = &entries.iter().find(|e| e.program && !e.name.starts_with("qrust-rustc-wrapper")).expect("qrust is a program").from;
        let says = run(Command::new(qrust).arg("--version"))?;
        let says = String::from_utf8_lossy(&says.stdout).trim().to_owned();
        if says != format!("qrust {version}") {
            return Err(format!("{} says {says:?}, and the release is qrust {version}", qrust.display()));
        }
    }
    io(format!("cannot create {}", out_dir.display()), fs::create_dir_all(out_dir))?;
    let out = out_dir.join(archive_name(&version, platform));
    if platform.starts_with("windows-") {
        zip(&entries, &out)?;
    } else {
        tar_gz(&entries, &out)?;
    }
    println!("{}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("quickrust-dist-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The files of an archive, as a build leaves them: none of them marked executable, so that
    /// what is executable in the archive is what the archive says is a program.
    fn staged(dir: &Path, exe: &str) -> Vec<Entry> {
        let mut entries = Vec::new();
        for (name, body, program) in [("qrust", "the program", true), ("qrust-rustc-wrapper", "the wrapper", true), ("LICENSE-MIT", "mit", false), ("LICENSE-APACHE", "apache", false), ("README.md", "read me", false)] {
            let name = if program { format!("{name}{exe}") } else { name.to_string() };
            let from = dir.join(format!("staged-{name}"));
            fs::write(&from, body).unwrap();
            entries.push(Entry { name, from, program });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_tarball_is_what_quickrust_unpacks_and_is_the_same_bytes_every_time() {
        let dir = scratch("tarball");
        let entries = staged(&dir, "");
        let (first, second) = (dir.join("a.tar.gz"), dir.join("b.tar.gz"));
        tar_gz(&entries, &first).unwrap();
        tar_gz(&entries, &second).unwrap();
        assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap(), "the same files make the same archive");

        let unpacked = dir.join("unpacked");
        let held = quickrust::archive::unpack_file(&first, quickrust::archive::Compression::Gz, quickrust::release::ARCHIVE_ROOT, &unpacked, 1 << 20).unwrap();
        assert_eq!(held.files, 5);
        assert_eq!(fs::read_to_string(unpacked.join("qrust")).unwrap(), "the program");
        assert_eq!(fs::read_to_string(unpacked.join("LICENSE-APACHE")).unwrap(), "apache");
        #[cfg(unix)]
        {
            assert_eq!((mode(&unpacked.join("qrust")), mode(&unpacked.join("qrust-rustc-wrapper"))), (0o755, 0o755));
            assert_eq!(mode(&unpacked.join("README.md")), 0o644);
        }
    }

    #[test]
    fn a_zip_is_what_quickrust_unpacks_on_windows() {
        let dir = scratch("zip");
        let entries = staged(&dir, ".exe");
        let (first, second) = (dir.join("a.zip"), dir.join("b.zip"));
        zip(&entries, &first).unwrap();
        zip(&entries, &second).unwrap();
        assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap(), "the same files make the same archive");

        let unpacked = dir.join("unpacked");
        let held = quickrust::archive::unpack_zip_file(&first, quickrust::release::ARCHIVE_ROOT, &unpacked, 1 << 20).unwrap();
        assert_eq!(held.files, 5);
        assert_eq!(fs::read_to_string(unpacked.join("qrust.exe")).unwrap(), "the program");
        assert_eq!(fs::read_to_string(unpacked.join("qrust-rustc-wrapper.exe")).unwrap(), "the wrapper");
        #[cfg(unix)]
        assert_eq!((mode(&unpacked.join("qrust.exe")), mode(&unpacked.join("LICENSE-MIT"))), (0o755, 0o644));
    }

    #[test]
    fn every_platform_is_archived_in_the_form_quickrust_updates_from() {
        for (platform, target) in PLATFORMS {
            let form = quickrust::release::Archive::of(platform);
            let name = archive_name("0.150.0", platform);
            assert_eq!(name, format!("qrust-0.150.0-{platform}{}", form.extension()), "{platform}");
            assert_eq!(target.contains("-windows-"), form == quickrust::release::Archive::Zip, "{platform}");
        }
        // The machine this runs on is one of them, under the name QuickRust knows itself by.
        let here = quickrust::release::platform().expect("the tests run on a platform QuickRust is published for");
        assert!(PLATFORMS.iter().any(|(p, _)| *p == here), "{here}");
        assert_eq!(host_platform(), Some(here));
    }

    #[test]
    fn a_release_line_is_whole_and_is_the_one_the_programs_carry() {
        let given = |missing: &'static str| {
            move |name: &str| {
                if name == missing {
                    return None;
                }
                Some(match name {
                    "MFR_RELEASES_BASE_URL" => " https://r.example ".to_string(),
                    "MFR_RELEASE_CHANNEL" => "nightly".to_string(),
                    "MFR_SIGNING_ENVIRONMENT" => "staging".to_string(),
                    _ => "rootA, rootB ,rootC".to_string(),
                })
            }
        };
        let line = release_line(&given("")).unwrap();
        assert_eq!(line, "https://r.example nightly staging rootA,rootB,rootC");
        for missing in RELEASE_LINE {
            let e = release_line(&given(missing)).unwrap_err();
            assert!(e.contains(&format!("and {missing} is not set")), "{e}");
        }
        assert!(release_line(&|_| None).unwrap_err().contains("are not set"));

        let program = [b"\x7fELF...".as_slice(), line.as_bytes(), b"...rest"].concat();
        assert!(carries(&program, &line));
        assert!(!carries(&program, "https://r.example release staging rootA,rootB,rootC"), "another channel's line is not this one");
        assert!(!carries(b"\x7fELF built from source", &line));
        assert!(!carries(&program, ""));
    }
}
