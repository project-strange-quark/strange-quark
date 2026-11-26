// SPDX-License-Identifier: MIT

//! The test-control channel on Unix: a mode-0600 Unix domain socket.

use std::{
    fs, io,
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

use super::{IO_POLL, REQUEST_TIMEOUT};

pub(super) struct Listener {
    listener: UnixListener,
    _socket: SocketPath,
}

impl Listener {
    pub(super) fn bind(path: &Path) -> io::Result<Self> {
        let listener = bind_private(path)?;
        let metadata = fs::symlink_metadata(path)?;
        let socket = SocketPath {
            path: path.to_owned(),
            identity: (metadata.dev(), metadata.ino()),
        };
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            _socket: socket,
        })
    }

    /// Returns the next client of the host's own user, or `WouldBlock`.
    /// Reads on the stream time out after [`IO_POLL`] so the server can
    /// notice a stop request.
    pub(super) fn accept(&self) -> io::Result<Option<UnixStream>> {
        let (stream, _) = self.listener.accept()?;
        if !same_user(&stream) {
            return Ok(None);
        }
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(IO_POLL))?;
        stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
        Ok(Some(stream))
    }
}

/// Binds the socket where no other user can reach it before it is mode
/// 0600: in a fresh 0700 directory beside `path`, made private before the
/// bind, and only then linked into place. Binding at `path` directly would
/// leave the socket at the umask's mode until the chmod, and Apple platforms
/// have no peer-uid check to fall back on.
///
/// The socket is hard-linked to `path`, not renamed there: like `bind`,
/// `link` fails with `AlreadyExists` when anything is at `path`, so a second
/// host given the same path cannot take over a live socket, and an existing
/// file there is left alone. A rename would silently replace either.
///
/// The staging path `<parent>/.j<n>/s` adds at most a few bytes over a
/// typical socket name, because socket paths are limited to about 104 bytes.
/// Its counter is shared by every bind in the process; concurrent processes
/// in the same directory step past each other's directories.
fn bind_private(path: &Path) -> io::Result<UnixListener> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut attempts = 0;
    let staging = loop {
        let candidate = parent.join(format!(".j{:x}", NEXT.fetch_add(1, Ordering::Relaxed)));
        match fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempts < 256 => {
                attempts += 1;
            }
            Err(error) => return Err(error),
        }
    };
    let staged = staging.join("s");
    let bound = (|| {
        let listener = UnixListener::bind(&staged)?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o600))?;
        fs::hard_link(&staged, path)?;
        Ok(listener)
    })();
    let _ = fs::remove_file(&staged);
    let _ = fs::remove_dir(&staging);
    bound
}

struct SocketPath {
    path: PathBuf,
    identity: (u64, u64),
}

impl Drop for SocketPath {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == self.identity)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Only the user who started the host may drive it.
#[cfg(target_os = "linux")]
fn same_user(stream: &UnixStream) -> bool {
    rustix::net::sockopt::socket_peercred(stream)
        .is_ok_and(|peer| peer.uid == rustix::process::geteuid())
}

/// rustix has no peer credentials on Apple platforms. The socket is bound
/// privately and is mode 0600 before it appears at its path (`bind_private`),
/// and XNU checks write access to the socket file on `connect`, so only its
/// owner reaches this point.
#[cfg(not(target_os = "linux"))]
fn same_user(_stream: &UnixStream) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_privately_then_appears_at_its_path_mode_0600() {
        let dir = std::env::temp_dir().join(format!("squark-unix-bind-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("squark.sock");

        let listener = Listener::bind(&path).unwrap();
        let mode = fs::symlink_metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // Only the socket remains; the staging directory is gone.
        let entries: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("squark.sock")]);

        // The linked socket accepts connections at its final path.
        let _client = UnixStream::connect(&path).unwrap();
        let mut accepted = None;
        for _ in 0..200 {
            match listener.accept() {
                Ok(stream) => {
                    accepted = stream;
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        }
        assert!(accepted.is_some(), "the same user was not accepted");

        drop(listener);
        assert!(!path.exists(), "the socket is removed with its listener");
        fs::remove_dir_all(&dir).unwrap();
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("squark-unix-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<std::ffi::OsString> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn refuses_a_path_where_a_file_already_exists_and_leaves_it_alone() {
        let dir = scratch("file");
        let path = dir.join("report.json");
        fs::write(&path, "keep me").unwrap();

        let error = Listener::bind(&path).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&path).unwrap(), "keep me");
        assert_eq!(names(&dir), vec![std::ffi::OsString::from("report.json")]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_the_path_of_a_live_socket_and_keeps_serving_it() {
        let dir = scratch("live");
        let path = dir.join("squark.sock");
        let first = Listener::bind(&path).unwrap();

        let error = Listener::bind(&path).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(names(&dir), vec![std::ffi::OsString::from("squark.sock")]);
        // The first host still owns the path and accepts its clients.
        let _client = UnixStream::connect(&path).unwrap();
        let accepted = (0..200).find_map(|_| match first.accept() {
            Ok(stream) => stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(5));
                None
            }
            Err(error) => panic!("accept failed: {error}"),
        });
        assert!(accepted.is_some(), "the first listener lost its socket");
        drop(first);
        fs::remove_dir_all(&dir).unwrap();
    }
}
