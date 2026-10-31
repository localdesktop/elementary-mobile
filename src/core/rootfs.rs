//! The operating system bundled inside the APK.
//!
//! The APK carries elementary OS as an xz-compressed tarball (`ROOTFS_ARCHIVE`) next to a small
//! JSON manifest (`ROOTFS_MANIFEST`). On first launch, and again whenever an updated APK carries a
//! different archive, it is unpacked into the app's files directory. The user's files live outside
//! the root filesystem (in a directory bind-mounted over `/home`), so replacing the OS on an update
//! keeps them.

use serde::Deserialize;
use std::{
    cell::Cell,
    fs,
    io::{self, Read},
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    rc::Rc,
};
use tar::{Archive, EntryType};
use xz2::read::XzDecoder;

/// The compressed root filesystem, as an APK asset.
pub const ROOTFS_ARCHIVE: &str = "elementary-os-rootfs.tar.xz";

/// Metadata about `ROOTFS_ARCHIVE`, as an APK asset. Written by `scripts/build-rootfs.sh`.
pub const ROOTFS_MANIFEST: &str = "elementary-os-rootfs.json";

/// File inside an unpacked root filesystem recording which archive it came from.
const STAMP_FILE: &str = ".elementary-mobile-rootfs";

/// Suffix of the directory an archive is unpacked into before it is moved into place.
const PARTIAL_SUFFIX: &str = "partial";

/// File inside a root filesystem whose setuid programs (sudo, su, passwd) have their proot
/// metadata. Root filesystems unpacked before it existed get it from `restore_setuid_metadata`.
const SETUID_STAMP: &str = ".elementary-mobile-setuid";

#[derive(Debug, Clone, Deserialize)]
pub struct RootfsManifest {
    /// SHA-256 of the archive. Changes whenever the bundled OS does.
    pub id: String,
    pub uncompressed_size: u64,
    /// The desktop user created in the image.
    pub username: String,
}

impl RootfsManifest {
    pub fn parse(json: &[u8]) -> io::Result<Self> {
        serde_json::from_slice(json).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

/// Whether `rootfs` holds a complete copy of the OS described by `manifest`.
pub fn is_installed(rootfs: &Path, manifest: &RootfsManifest) -> bool {
    fs::read_to_string(rootfs.join(STAMP_FILE))
        .map(|id| id.trim() == manifest.id)
        .unwrap_or(false)
}

/// Unpack `archive` (an xz-compressed tarball) into `rootfs`, replacing whatever OS is there.
///
/// The old root filesystem is deleted first, to need as little free space as possible: the user's
/// files are not in it. The archive is unpacked into a sibling directory that is renamed into
/// place only once it is complete, so an interrupted install is simply redone on the next launch.
///
/// `progress` is called as the archive is read with the number of uncompressed bytes so far,
/// comparable to `RootfsManifest::uncompressed_size`.
pub fn install<R: Read>(
    archive: R,
    rootfs: &Path,
    manifest: &RootfsManifest,
    mut progress: impl FnMut(u64),
) -> io::Result<()> {
    let partial = partial_dir(rootfs);
    remove_tree(&partial)?;
    remove_tree(rootfs)?;
    fs::create_dir_all(&partial)?;

    let read_bytes = Rc::new(Cell::new(0u64));
    let reader = CountingReader {
        inner: XzDecoder::new(archive),
        count: read_bytes.clone(),
    };
    let mut archive = Archive::new(reader);
    archive.set_overwrite(true);

    // Directory permissions are applied last: a read-only directory such as /proc would otherwise
    // stop everything below it from being unpacked.
    let mut directories = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        let mode = entry.header().mode().unwrap_or(0o644);

        match kind {
            // Apps cannot create device nodes or FIFOs. /dev comes from Android through proot.
            EntryType::Char | EntryType::Block | EntryType::Fifo => continue,
            EntryType::Directory => {
                let path = entry.path()?.into_owned();
                if let Some(dst) = destination(&partial, &path) {
                    fs::create_dir_all(&dst)?;
                    directories.push((dst, mode));
                }
            }
            EntryType::Link => {
                // Android does not let apps create hard links, so copy the file instead.
                if let Err(link_error) = entry.unpack_in(&partial) {
                    let (Some(dst), Some(src)) = (
                        destination(&partial, &entry.path()?),
                        entry
                            .link_name()?
                            .and_then(|target| destination(&partial, &target)),
                    ) else {
                        return Err(link_error);
                    };
                    fs::copy(&src, &dst)?;
                }
            }
            _ => {
                entry.unpack_in(&partial)?;
                write_setuid_metadata(&partial, &entry)?;
                // The guest runs as a fake root that should be able to change any file, but the
                // kernel still checks the owner bits of the app's own user.
                if kind == EntryType::Regular && mode & 0o600 != 0o600 {
                    if let Some(dst) = destination(&partial, &entry.path()?) {
                        fs::set_permissions(dst, fs::Permissions::from_mode((mode & 0o777) | 0o600))?;
                    }
                }
            }
        }
        progress(read_bytes.get());
    }

    for (dst, mode) in directories.into_iter().rev() {
        fs::set_permissions(dst, fs::Permissions::from_mode((mode & 0o777) | 0o700))?;
    }

    // tar stops at its end-of-archive marker. Reading the padding after it lets xz check the end
    // of the stream too, so a damaged archive never gets stamped as installed.
    io::copy(&mut archive.into_inner(), &mut io::sink())?;
    progress(read_bytes.get());

    fs::write(partial.join(STAMP_FILE), &manifest.id)?;
    fs::write(partial.join(SETUID_STAMP), "")?;
    fs::rename(&partial, rootfs)
}

/// Whether `rootfs` was unpacked before setuid programs got their proot metadata.
pub fn needs_setuid_metadata(rootfs: &Path) -> bool {
    rootfs.join(STAMP_FILE).exists() && !rootfs.join(SETUID_STAMP).exists()
}

/// Give the setuid programs of an already unpacked `rootfs` their proot metadata, reading
/// `archive` (the one it was unpacked from) without unpacking it again.
pub fn restore_setuid_metadata<R: Read>(
    archive: R,
    rootfs: &Path,
    mut progress: impl FnMut(u64),
) -> io::Result<()> {
    let read_bytes = Rc::new(Cell::new(0u64));
    let reader = CountingReader {
        inner: XzDecoder::new(archive),
        count: read_bytes.clone(),
    };
    let mut archive = Archive::new(reader);
    for entry in archive.entries()? {
        write_setuid_metadata(rootfs, &entry?)?;
        progress(read_bytes.get());
    }
    fs::write(rootfs.join(SETUID_STAMP), "")
}

/// Android does not let apps set the setuid bit or change owners, so proot's fake root keeps them
/// in a `.proot-meta-file.<name>.meta` file next to the file. Without it, sudo and su run as the
/// user who started them and cannot become root.
fn write_setuid_metadata<R: Read>(root: &Path, entry: &tar::Entry<R>) -> io::Result<()> {
    let header = entry.header();
    let mode = header.mode().unwrap_or(0);
    if header.entry_type() != EntryType::Regular || mode & 0o6000 == 0 {
        return Ok(());
    }
    let Some(dst) = destination(root, &entry.path()?) else {
        return Ok(());
    };
    let (Some(dir), Some(name)) = (dst.parent(), dst.file_name()) else {
        return Ok(());
    };
    let meta = dir.join(format!(".proot-meta-file.{}.meta", name.to_string_lossy()));
    let (uid, gid) = (header.uid().unwrap_or(0), header.gid().unwrap_or(0));
    // proot reads the mode as octal digits written in decimal.
    fs::write(meta, format!("{:o}\n{uid}\n{gid}\n", mode & 0o7777))
}

/// Remove what an interrupted `install` left behind. Cheap when there is nothing to clean up.
pub fn remove_partial(rootfs: &Path) -> io::Result<()> {
    remove_tree(&partial_dir(rootfs))
}

/// Give the user a home directory in `home_root` (bind-mounted over the guest's `/home`) if they
/// do not have one yet, copied from the image's own `/home/<username>` or `/etc/skel`.
///
/// Returns whether a home directory was created.
pub fn seed_home(rootfs: &Path, home_root: &Path, username: &str) -> io::Result<bool> {
    let home = home_root.join(username);
    if home.exists() {
        return Ok(false);
    }

    let template = [rootfs.join("home").join(username), rootfs.join("etc/skel")]
        .into_iter()
        .find(|dir| dir.is_dir());

    let partial = home_root.join(format!(".{username}.{PARTIAL_SUFFIX}"));
    remove_tree(&partial)?;
    fs::create_dir_all(&partial)?;
    if let Some(template) = template {
        copy_tree(&template, &partial)?;
    }
    fs::set_permissions(&partial, fs::Permissions::from_mode(0o755))?;
    fs::rename(&partial, &home)?;
    Ok(true)
}

fn partial_dir(rootfs: &Path) -> PathBuf {
    let mut name = rootfs.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(PARTIAL_SUFFIX);
    rootfs.with_file_name(name)
}

/// Where an archive path lands under `root`, or `None` if it would escape it.
fn destination(root: &Path, path: &Path) -> Option<PathBuf> {
    use std::path::Component;

    let mut dst = root.to_path_buf();
    for component in path.components() {
        match component {
            Component::Normal(part) => dst.push(part),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::ParentDir => return None,
        }
    }
    (dst != root).then_some(dst)
}

/// `fs::remove_dir_all` that also gets through directories without write permission.
fn remove_tree(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !metadata.is_dir() {
        return fs::remove_file(path);
    }

    make_writable(path)?;
    fs::remove_dir_all(path)
}

fn make_writable(dir: &Path) -> io::Result<()> {
    let mode = fs::symlink_metadata(dir)?.permissions().mode();
    if mode & 0o700 != 0o700 {
        fs::set_permissions(dir, fs::Permissions::from_mode(mode | 0o700))?;
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            make_writable(&entry.path())?;
        }
    }
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            symlink(fs::read_link(&from)?, &to)?;
        } else if file_type.is_dir() {
            fs::create_dir(&to)?;
            copy_tree(&from, &to)?;
            fs::set_permissions(&to, entry.metadata()?.permissions())?;
        } else if file_type.is_file() {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

struct CountingReader<R> {
    inner: R,
    count: Rc<Cell<u64>>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.set(self.count.get() + n as u64);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use tar::{Builder, Header};
    use tempfile::tempdir;
    use xz2::write::XzEncoder;

    fn manifest(id: &str) -> RootfsManifest {
        RootfsManifest {
            id: id.into(),
            uncompressed_size: 0,
            username: "elementary".into(),
        }
    }

    fn header(kind: EntryType, path: &str, mode: u32, size: u64) -> Header {
        let mut header = Header::new_gnu();
        header.set_entry_type(kind);
        header.set_path(path).unwrap();
        header.set_mode(mode);
        header.set_size(size);
        header.set_mtime(1_700_000_000);
        header.set_cksum();
        header
    }

    /// A tarball shaped like a real root filesystem: `./` prefixes, a read-only directory listed
    /// before its contents, usrmerge symlinks, a hard link, a device node and a FIFO.
    fn sample_archive() -> Vec<u8> {
        let mut builder = Builder::new(XzEncoder::new(Vec::new(), 1));
        let mut add = |kind, path: &str, mode, data: &[u8], link: Option<&str>| {
            let mut header = header(kind, path, mode, data.len() as u64);
            if let Some(link) = link {
                header.set_link_name(link).unwrap();
                header.set_cksum();
            }
            builder.append(&header, data).unwrap();
        };
        add(EntryType::Directory, "./", 0o755, b"", None);
        add(EntryType::Directory, "./proc/", 0o555, b"", None);
        add(EntryType::Regular, "./proc/.version", 0o444, b"Linux\n", None);
        add(EntryType::Directory, "./usr/bin/", 0o755, b"", None);
        add(EntryType::Regular, "./usr/bin/perl", 0o755, b"#!perl\n", None);
        add(EntryType::Link, "./usr/bin/perl5.38", 0o755, b"", Some("./usr/bin/perl"));
        add(EntryType::Symlink, "./bin", 0o777, b"", Some("usr/bin"));
        add(EntryType::Char, "./dev/null", 0o666, b"", None);
        add(EntryType::Fifo, "./run/initctl", 0o600, b"", None);
        add(EntryType::Directory, "./home/elementary/", 0o750, b"", None);
        add(EntryType::Regular, "./home/elementary/.bashrc", 0o644, b"# bashrc\n", None);
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn installs_a_root_filesystem() {
        let dir = tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        let mut last_progress = 0;

        install(&sample_archive()[..], &rootfs, &manifest("a"), |n| last_progress = n).unwrap();

        assert_eq!(fs::read_to_string(rootfs.join("proc/.version")).unwrap(), "Linux\n");
        assert_eq!(fs::read_to_string(rootfs.join("usr/bin/perl5.38")).unwrap(), "#!perl\n");
        assert_eq!(fs::read_link(rootfs.join("bin")).unwrap(), Path::new("usr/bin"));
        assert!(!rootfs.join("dev/null").exists(), "device nodes are skipped");
        assert!(!rootfs.join("run/initctl").exists(), "FIFOs are skipped");
        assert!(last_progress > 0);
        assert!(!partial_dir(&rootfs).exists());
    }

    #[test]
    fn keeps_permissions_usable_by_the_app_user() {
        let dir = tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        install(&sample_archive()[..], &rootfs, &manifest("a"), |_| {}).unwrap();

        let mode = |path: &str| fs::metadata(rootfs.join(path)).unwrap().mode() & 0o777;
        assert_eq!(mode("proc"), 0o755, "read-only directory gains owner write");
        assert_eq!(mode("proc/.version"), 0o644, "read-only file gains owner write");
        assert_eq!(mode("usr/bin/perl"), 0o755);
        assert_eq!(mode("home/elementary"), 0o750);
    }

    #[test]
    fn tracks_which_archive_is_installed() {
        let dir = tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        assert!(!is_installed(&rootfs, &manifest("a")));

        install(&sample_archive()[..], &rootfs, &manifest("a"), |_| {}).unwrap();
        assert!(is_installed(&rootfs, &manifest("a")));
        assert!(!is_installed(&rootfs, &manifest("b")));
    }

    #[test]
    fn replaces_an_older_root_filesystem() {
        let dir = tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        install(&sample_archive()[..], &rootfs, &manifest("a"), |_| {}).unwrap();
        fs::write(rootfs.join("usr/bin/installed-later"), "x").unwrap();
        // A read-only directory must not stop the old tree from being deleted.
        fs::set_permissions(rootfs.join("usr/bin"), fs::Permissions::from_mode(0o555)).unwrap();

        install(&sample_archive()[..], &rootfs, &manifest("b"), |_| {}).unwrap();

        assert!(is_installed(&rootfs, &manifest("b")));
        assert!(!rootfs.join("usr/bin/installed-later").exists());
    }

    #[test]
    fn redoes_an_interrupted_install() {
        let dir = tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        let partial = partial_dir(&rootfs);
        fs::create_dir_all(partial.join("usr")).unwrap();

        assert!(!is_installed(&rootfs, &manifest("a")));
        remove_partial(&rootfs).unwrap();
        assert!(!partial.exists());

        install(&sample_archive()[..], &rootfs, &manifest("a"), |_| {}).unwrap();
        assert!(is_installed(&rootfs, &manifest("a")));
    }

    #[test]
    fn rejects_a_truncated_archive() {
        let dir = tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        let archive = sample_archive();

        let result = install(&archive[..archive.len() / 2], &rootfs, &manifest("a"), |_| {});

        assert!(result.is_err());
        assert!(!is_installed(&rootfs, &manifest("a")));
    }

    #[test]
    fn seeds_the_home_directory_once() {
        let dir = tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        let home_root = dir.path().join("home");
        install(&sample_archive()[..], &rootfs, &manifest("a"), |_| {}).unwrap();

        assert!(seed_home(&rootfs, &home_root, "elementary").unwrap());
        let bashrc = home_root.join("elementary/.bashrc");
        assert_eq!(fs::read_to_string(&bashrc).unwrap(), "# bashrc\n");

        fs::write(&bashrc, "# edited\n").unwrap();
        assert!(!seed_home(&rootfs, &home_root, "elementary").unwrap());
        assert_eq!(fs::read_to_string(&bashrc).unwrap(), "# edited\n");
    }

    /// Unpacks the archive built by scripts/build-rootfs.sh:
    /// `cargo test --lib -- --ignored installs_the_built_archive`
    #[test]
    #[ignore = "needs assets/rootfs from scripts/build-rootfs.sh"]
    fn installs_the_built_archive() {
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/rootfs");
        let manifest =
            RootfsManifest::parse(&fs::read(assets.join(ROOTFS_MANIFEST)).unwrap()).unwrap();
        let archive = fs::File::open(assets.join(ROOTFS_ARCHIVE)).unwrap();
        let dir = tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        let mut read = 0;

        install(io::BufReader::new(archive), &rootfs, &manifest, |n| read = n).unwrap();

        assert!(is_installed(&rootfs, &manifest));
        assert_eq!(read, manifest.uncompressed_size);
        for path in [
            "usr/bin/gala",
            "usr/bin/Xwayland",
            "usr/local/bin/elementary-mobile-session",
            "etc/elementary-mobile/dbus-system.conf",
        ] {
            assert!(rootfs.join(path).exists(), "{path} missing");
        }
        assert!(rootfs.join("home").join(&manifest.username).is_dir());
    }

    #[test]
    fn parses_the_manifest_written_by_the_build() {
        let manifest = RootfsManifest::parse(
            br#"{
                "id": "0f3a",
                "archive": "elementary-os-rootfs.tar.xz",
                "compressed_size": 214748364,
                "uncompressed_size": 1073741824,
                "entries": 28420,
                "username": "elementary",
                "built_at": "2026-10-15T16:00:00Z"
            }"#,
        )
        .unwrap();
        assert_eq!(manifest.id, "0f3a");
        assert_eq!(manifest.uncompressed_size, 1073741824);
        assert_eq!(manifest.username, "elementary");
    }
}
