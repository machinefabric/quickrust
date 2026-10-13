//! Unpacking the archives QuickRust downloads: toolchain components (`.tar.xz`), crates (`.crate`,
//! a `.tar.gz`) and its own releases (`.tar.gz`, and `.zip` for Windows). Each holds one top-level
//! directory with a known name, and nothing may land outside it: an entry with an absolute path, a `..` component, a link pointing out of the
//! tree, an entry beneath a link, a hard link or a device is an error, and so is content beyond a size bound (a
//! decompression bomb). Files are created anew (a name twice in one archive is an error) with
//! their mode reduced to executable or not, and their modification time kept.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    Xz,
    Gz,
}

/// What an archive held.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Unpacked {
    pub files: u64,
    pub bytes: u64,
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Unpacks the archive read from `reader` into `dest`, which must not exist yet: the content of
/// its top-level directory `root` becomes the content of `dest`. At most `max_bytes` of file
/// content are written. On error, `dest` is removed.
pub fn unpack(reader: impl Read, compression: Compression, root: &str, dest: &Path, max_bytes: u64) -> io::Result<Unpacked> {
    if dest.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists already", dest.display())));
    }
    fs::create_dir_all(dest)?;
    let out = match compression {
        Compression::Xz => unpack_tar(liblzma::read::XzDecoder::new(reader), root, dest, max_bytes),
        Compression::Gz => unpack_tar(flate2::read::GzDecoder::new(reader), root, dest, max_bytes),
    };
    if out.is_err() {
        let _ = fs::remove_dir_all(dest);
    }
    out
}

/// Unpacks the archive file at `path` (see `unpack`).
pub fn unpack_file(path: &Path, compression: Compression, root: &str, dest: &Path, max_bytes: u64) -> io::Result<Unpacked> {
    let file = fs::File::open(path)?;
    unpack(io::BufReader::with_capacity(256 * 1024, file), compression, root, dest, max_bytes).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path.display(), e)))
}

/// Unpacks the zip file at `path` into `dest`, which must not exist yet, under the rules of
/// `unpack`: the content of its top-level directory `root` becomes the content of `dest`, at most
/// `max_bytes` of file content are written, and on error `dest` is removed. A zip entry is a
/// directory or a regular file; anything else (a symbolic link) is an error.
pub fn unpack_zip_file(path: &Path, root: &str, dest: &Path, max_bytes: u64) -> io::Result<Unpacked> {
    if dest.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists already", dest.display())));
    }
    let file = fs::File::open(path)?;
    fs::create_dir_all(dest)?;
    let out = unpack_zip(io::BufReader::with_capacity(256 * 1024, file), root, dest, max_bytes);
    if out.is_err() {
        let _ = fs::remove_dir_all(dest);
    }
    out.map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path.display(), e)))
}

fn unpack_zip(reader: impl Read + io::Seek, root: &str, dest: &Path, max_bytes: u64) -> io::Result<Unpacked> {
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| invalid(format!("not a zip archive: {}", e)))?;
    let mut done = Unpacked::default();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| invalid(format!("entry {}: {}", index, e)))?;
        // The name as written, checked here: no normalisation stands between it and the check.
        let name = PathBuf::from(entry.name().replace('\\', "/"));
        let Some(rel) = inside(&name, root)? else { continue };
        let target = dest.join(&rel);
        // The file type bits of the Unix mode, where the archive carries one.
        let kind = entry.unix_mode().map(|m| m & 0o170000).filter(|k| *k != 0);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if kind.is_some_and(|k| k != 0o100000) {
            return Err(invalid(format!("{}: only directories and regular files are unpacked from a zip", name.display())));
        }
        if let Some(dir) = target.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&target).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", name.display(), e)))?;
        // Counted as it is written: the size an entry declares is the archive's word for it.
        let left = max_bytes - done.bytes;
        let written = io::copy(&mut (&mut entry).take(left + 1), &mut file)?;
        if written > left {
            return Err(invalid(format!("the archive holds more than {} bytes", max_bytes)));
        }
        file.flush()?;
        set_executable(&file, entry.unix_mode().is_some_and(|m| m & 0o111 != 0))?;
        done.files += 1;
        done.bytes += written;
    }
    Ok(done)
}

fn unpack_tar(reader: impl Read, root: &str, dest: &Path, max_bytes: u64) -> io::Result<Unpacked> {
    let mut archive = tar::Archive::new(reader);
    let mut done = Unpacked::default();
    // Directories known to be real ones (not links), relative to `dest`.
    let mut real: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() || kind.is_pax_local_extensions() || kind.is_gnu_longname() || kind.is_gnu_longlink() {
            continue;
        }
        let name = entry.path()?.into_owned();
        let Some(rel) = inside(&name, root)? else { continue };
        not_beneath_a_link(dest, &rel, &mut real).map_err(|e| invalid(format!("{}: {}", name.display(), e)))?;
        let target = dest.join(&rel);
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
        } else if kind.is_file() || kind.is_contiguous() {
            let size = entry.size();
            if done.bytes + size > max_bytes {
                return Err(invalid(format!("the archive holds more than {} bytes", max_bytes)));
            }
            if let Some(dir) = target.parent() {
                fs::create_dir_all(dir)?;
            }
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&target).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", name.display(), e)))?;
            let written = io::copy(&mut (&mut entry).take(size), &mut file)?;
            if written != size {
                return Err(invalid(format!("{} ends after {} of {} bytes", name.display(), written, size)));
            }
            file.flush()?;
            let mode = entry.header().mode()?;
            set_executable(&file, mode & 0o111 != 0)?;
            if let Ok(mtime) = entry.header().mtime() {
                file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime))?;
            }
            done.files += 1;
            done.bytes += size;
        } else if kind.is_symlink() {
            let link = entry.link_name()?.ok_or_else(|| invalid(format!("{}: a symbolic link without a target", name.display())))?.into_owned();
            if !link_stays_inside(&rel, &link) {
                return Err(invalid(format!("{} links outside the archive ({})", name.display(), link.display())));
            }
            if let Some(dir) = target.parent() {
                fs::create_dir_all(dir)?;
            }
            symlink(&link, &target)?;
        } else {
            return Err(invalid(format!("{}: entries of type {:?} are not unpacked", name.display(), kind)));
        }
    }
    Ok(done)
}

/// The path of `name` inside the top-level directory `root`: `None` for `root` itself, an error
/// for anything outside it or not a plain relative path.
fn inside(name: &Path, root: &str) -> io::Result<Option<PathBuf>> {
    let mut parts = name.components().filter(|c| *c != Component::CurDir);
    match parts.next() {
        Some(Component::Normal(first)) if first == root => {}
        _ => return Err(invalid(format!("{} is not inside {}/", name.display(), root))),
    }
    let mut rel = PathBuf::new();
    for c in parts {
        match c {
            Component::Normal(p) => rel.push(p),
            _ => return Err(invalid(format!("{} is not a plain relative path", name.display()))),
        }
    }
    Ok(if rel.as_os_str().is_empty() { None } else { Some(rel) })
}

/// Every directory above `rel` is a real directory (or does not exist yet): an entry is never
/// written through a link, which could lead anywhere once links point at links.
fn not_beneath_a_link(dest: &Path, rel: &Path, real: &mut std::collections::HashSet<PathBuf>) -> io::Result<()> {
    let mut dir = PathBuf::new();
    for c in rel.parent().into_iter().flat_map(Path::components) {
        dir.push(c);
        if real.contains(&dir) {
            continue;
        }
        match fs::symlink_metadata(dest.join(&dir)) {
            Ok(md) if md.file_type().is_symlink() => return Err(invalid(format!("{} is a symbolic link", dir.display()))),
            Ok(md) if !md.is_dir() => return Err(invalid(format!("{} is not a directory", dir.display()))),
            Ok(_) => {
                real.insert(dir.clone());
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// A symbolic link at `at` (relative to the unpacked tree) to `link` resolves inside the tree.
fn link_stays_inside(at: &Path, link: &Path) -> bool {
    let mut depth: usize = at.components().count().saturating_sub(1);
    for c in link.components() {
        match c {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => match depth.checked_sub(1) {
                Some(d) => depth = d,
                None => return false,
            },
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

#[cfg(unix)]
fn set_executable(file: &fs::File, executable: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }))
}

#[cfg(not(unix))]
fn set_executable(_file: &fs::File, _executable: bool) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn symlink(link: &Path, at: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(link, at)
}

#[cfg(not(unix))]
fn symlink(link: &Path, at: &Path) -> io::Result<()> {
    Err(invalid(format!("{} is a symbolic link (to {}), which is not unpacked on this platform", at.display(), link.display())))
}

#[cfg(test)]
pub mod testing {
    //! Builds archives for tests, including ones no well-behaved tool would write.

    use std::io::Write;

    pub enum Item<'a> {
        File(&'a str, &'a [u8], u32),
        Dir(&'a str),
        Symlink(&'a str, &'a str),
        HardLink(&'a str, &'a str),
    }

    /// A tar archive of `items`, names written verbatim (no normalisation).
    pub fn tar(items: &[Item]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for item in items {
            let mut h = tar::Header::new_gnu();
            let (name, data): (&str, &[u8]) = match item {
                Item::File(n, d, mode) => {
                    h.set_entry_type(tar::EntryType::Regular);
                    h.set_mode(*mode);
                    h.set_size(d.len() as u64);
                    (n, d)
                }
                Item::Dir(n) => {
                    h.set_entry_type(tar::EntryType::Directory);
                    h.set_mode(0o755);
                    h.set_size(0);
                    (n, &[])
                }
                Item::Symlink(n, to) | Item::HardLink(n, to) => {
                    h.set_entry_type(if matches!(item, Item::Symlink(..)) { tar::EntryType::Symlink } else { tar::EntryType::Link });
                    h.set_size(0);
                    h.set_mode(0o777);
                    let raw = &mut h.as_old_mut().linkname;
                    raw[..to.len()].copy_from_slice(to.as_bytes());
                    (n, &[])
                }
            };
            h.set_mtime(1_700_000_000);
            if name.len() > 100 {
                // A long name takes a GNU extension entry; such names are well-formed here.
                b.append_data(&mut h, name, data).unwrap();
                continue;
            }
            // Raw name bytes: `set_path` would refuse `..` and absolute names.
            let raw = &mut h.as_old_mut().name;
            raw[..name.len()].copy_from_slice(name.as_bytes());
            h.set_cksum();
            b.append(&h, data).unwrap();
        }
        b.into_inner().unwrap()
    }

    /// A zip archive of `(name, content, unix mode)`, names written verbatim. A mode with the
    /// symbolic-link type bits makes the entry a link, as a Unix zip tool records one.
    pub fn zip(items: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data, mode) in items {
            let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).unix_permissions(*mode);
            if name.ends_with('/') {
                z.add_directory(name.trim_end_matches('/'), options).unwrap();
            } else if mode & 0o170000 == 0o120000 {
                z.add_symlink(*name, String::from_utf8_lossy(data), options).unwrap();
            } else {
                z.start_file(*name, options).unwrap();
                z.write_all(data).unwrap();
            }
        }
        z.finish().unwrap().into_inner()
    }

    pub fn gz(bytes: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(bytes).unwrap();
        e.finish().unwrap()
    }

    pub fn xz(bytes: &[u8]) -> Vec<u8> {
        let mut e = liblzma::write::XzEncoder::new(Vec::new(), 1);
        e.write_all(bytes).unwrap();
        e.finish().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{gz, tar, xz, zip, Item};
    use super::*;

    fn dest() -> PathBuf {
        std::env::temp_dir().join(format!("qrust-archive-{}", crate::util::new_id())).join("out")
    }

    fn fails(items: &[Item], expect: &str) {
        let d = dest();
        let e = unpack(&gz(&tar(items))[..], Compression::Gz, "pkg-1.0", &d, 1 << 20).unwrap_err();
        assert!(e.to_string().contains(expect), "{} (expected {:?})", e, expect);
        assert!(!d.exists(), "a failed unpack leaves nothing");
    }

    /// The entries of a test zip: name, content, Unix mode.
    type Zipped<'a> = [(&'a str, &'a [u8], u32)];

    /// Unpacks a zip of `items` whose top-level directory is `quickrust`.
    fn unzip(items: &Zipped, max_bytes: u64) -> (PathBuf, io::Result<Unpacked>) {
        let d = dest();
        fs::create_dir_all(d.parent().unwrap()).unwrap();
        let file = d.with_file_name("archive.zip");
        fs::write(&file, zip(items)).unwrap();
        let out = unpack_zip_file(&file, "quickrust", &d, max_bytes);
        (d, out)
    }

    #[test]
    fn a_zip_unpacks_its_top_level_directory_and_keeps_what_is_executable() {
        let (d, out) = unzip(&[("quickrust/", b"", 0o755), ("quickrust/qrust.exe", b"MZ program", 0o755), ("quickrust/docs/README.md", b"read me", 0o644)], 1 << 20);
        assert_eq!(out.unwrap(), Unpacked { files: 2, bytes: 17 });
        assert_eq!(fs::read(d.join("qrust.exe")).unwrap(), b"MZ program");
        assert_eq!(fs::read(d.join("docs/README.md")).unwrap(), b"read me");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(d.join("qrust.exe")).unwrap().permissions().mode() & 0o777, 0o755);
            assert_eq!(fs::metadata(d.join("docs/README.md")).unwrap().permissions().mode() & 0o777, 0o644);
        }
    }

    #[test]
    fn a_zip_lands_nothing_outside_its_directory_and_nothing_beyond_the_bound() {
        let refused: [(&Zipped, &str); 5] = [
            (&[("other/qrust", b"x", 0o755)], "is not inside quickrust/"),
            (&[("quickrust/../evil", b"x", 0o644)], "is not a plain relative path"),
            (&[("/quickrust/qrust", b"x", 0o644)], "is not inside quickrust/"),
            (&[("quickrust/qrust", b"/bin/sh", 0o120777)], "only directories and regular files are unpacked from a zip"),
            (&[("quickrust/a", b"12345678", 0o644), ("quickrust/b", b"12345678", 0o644)], "the archive holds more than 15 bytes"),
        ];
        for (items, expect) in refused {
            let (d, out) = unzip(items, 15);
            let e = out.unwrap_err();
            assert!(e.to_string().contains(expect), "{} (expected {:?})", e, expect);
            assert!(!d.exists(), "a failed unpack leaves nothing");
        }
        // Exactly the bound is within it.
        let (_, out) = unzip(&[("quickrust/a", b"12345678", 0o644), ("quickrust/b", b"1234567", 0o644)], 15);
        assert_eq!(out.unwrap(), Unpacked { files: 2, bytes: 15 });
        assert!(unpack_zip_file(Path::new("/nonexistent/archive.zip"), "quickrust", &dest(), 15).is_err());
    }

    #[test]
    fn unpacks_the_top_level_directory_with_modes_times_and_inner_links() {
        let items = [
            Item::Dir("pkg-1.0/"),
            Item::File("pkg-1.0/bin/tool", b"#!/bin/sh\n", 0o775),
            Item::File("pkg-1.0/./lib/data.txt", b"data", 0o664),
            Item::Symlink("pkg-1.0/lib/current", "../bin/tool"),
        ];
        for (bytes, c) in [(xz(&tar(&items)), Compression::Xz), (gz(&tar(&items)), Compression::Gz)] {
            let d = dest();
            let n = unpack(&bytes[..], c, "pkg-1.0", &d, 1 << 20).unwrap();
            assert_eq!(n, Unpacked { files: 2, bytes: 14 });
            assert_eq!(fs::read(d.join("lib/data.txt")).unwrap(), b"data");
            assert_eq!(fs::read(d.join("lib/current")).unwrap(), b"#!/bin/sh\n");
            let md = fs::metadata(d.join("bin/tool")).unwrap();
            assert_eq!(crate::util::mtime_ns(&md), 1_700_000_000 * 1_000_000_000);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(md.permissions().mode() & 0o777, 0o755);
                assert_eq!(fs::metadata(d.join("lib/data.txt")).unwrap().permissions().mode() & 0o777, 0o644);
            }
        }
    }

    #[test]
    fn nothing_lands_outside_the_destination() {
        fails(&[Item::File("pkg-1.0/../evil", b"x", 0o644)], "not a plain relative path");
        fails(&[Item::File("/etc/evil", b"x", 0o644)], "is not inside pkg-1.0/");
        fails(&[Item::File("other-2.0/file", b"x", 0o644)], "is not inside pkg-1.0/");
        fails(&[Item::Symlink("pkg-1.0/escape", "../../etc/passwd")], "links outside the archive");
        fails(&[Item::Symlink("pkg-1.0/abs", "/etc/passwd")], "links outside the archive");
        fails(&[Item::File("pkg-1.0/a", b"x", 0o644), Item::HardLink("pkg-1.0/b", "pkg-1.0/a")], "are not unpacked");
        // Links that each stay inside, chained to lead out: `up` is the root itself, so
        // `up/out` points one level above the destination.
        fails(&[Item::Symlink("pkg-1.0/up", "."), Item::Symlink("pkg-1.0/up/out", "../escaped")], "up is a symbolic link");
        fails(&[Item::Symlink("pkg-1.0/dir", "."), Item::File("pkg-1.0/dir/x", b"x", 0o644)], "dir is a symbolic link");
    }

    #[test]
    fn names_twice_and_content_beyond_the_bound_are_errors() {
        fails(&[Item::File("pkg-1.0/a", b"x", 0o644), Item::File("pkg-1.0/a", b"y", 0o644)], "exists");
        let big = vec![7u8; 600 << 10];
        fails(&[Item::File("pkg-1.0/a", &big, 0o644), Item::File("pkg-1.0/b", &big, 0o644)], "more than");
        let d = dest();
        fs::create_dir_all(&d).unwrap();
        let e = unpack(&gz(&tar(&[Item::File("pkg-1.0/a", b"x", 0o644)]))[..], Compression::Gz, "pkg-1.0", &d, 1 << 20).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert!(d.exists(), "an existing destination is left alone");
    }
}
