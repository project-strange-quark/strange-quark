// SPDX-License-Identifier: MIT

use std::{
    collections::HashMap,
    env,
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

use crate::shutdown::STORAGE_CLOSE_BUDGET;

type Entries = Vec<(Vec<u16>, Vec<u16>)>;

#[derive(Clone)]
pub(crate) struct Storage(Arc<StorageInner>);

struct StorageInner {
    root: PathBuf,
    entries: Mutex<Result<Entries, String>>,
    writer: Sender<WriterMessage>,
}

static STORAGE_REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<StorageInner>>>> = OnceLock::new();

pub(crate) enum Change {
    Set(Vec<u16>, Vec<u16>),
    Remove(Vec<u16>),
    Clear,
}

enum WriterMessage {
    Change(Change),
    Flush {
        deadline: Instant,
        reply: SyncSender<io::Result<()>>,
    },
}

impl Storage {
    pub(crate) fn new(app: &str, root: Option<&Path>) -> Option<Self> {
        let root = root.map(Path::to_owned).or_else(data_directory)?;
        let app = hex(app.as_bytes());
        let root = match canonical_root(&root.join(format!("app-{app}"))) {
            Ok(root) => root,
            Err(error) => {
                eprintln!("squark localStorage root could not be canonicalized: {error}");
                return None;
            }
        };
        let registry = STORAGE_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        let mut registry = registry.lock().unwrap_or_else(|error| error.into_inner());
        registry.retain(|_, storage| storage.strong_count() > 0);
        if let Some(storage) = registry.get(&root).and_then(Weak::upgrade) {
            return Some(Self(storage));
        }
        let entries = read_local_storage(&root).map_err(|error| error.to_string());
        let (writer, receiver) = mpsc::channel();
        let worker_root = root.clone();
        if let Err(error) = thread::Builder::new()
            .name("squark-local-storage".into())
            .spawn(move || local_storage_writer(worker_root, receiver))
        {
            eprintln!("squark localStorage writer could not start: {error}");
            return None;
        }
        let storage = Arc::new(StorageInner {
            root,
            entries: Mutex::new(entries),
            writer,
        });
        registry.insert(storage.root.clone(), Arc::downgrade(&storage));
        Some(Self(storage))
    }

    pub(crate) fn read(&self) -> io::Result<Entries> {
        match &*self
            .0
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
        {
            Ok(entries) => Ok(entries.clone()),
            Err(error) => Err(io::Error::other(error.clone())),
        }
    }

    pub(crate) fn commit(&self, change: Change) -> io::Result<()> {
        let mut cache = self
            .0
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let entries = cache
            .as_mut()
            .map_err(|error| io::Error::other(error.clone()))?;
        apply_change(entries, &change);
        if let Err(error) = self.0.writer.send(WriterMessage::Change(change)) {
            eprintln!("squark localStorage change queue failed: {error}");
        }
        Ok(())
    }

    pub(crate) fn flush(&self, deadline: Instant) -> io::Result<()> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.0
            .writer
            .send(WriterMessage::Flush { deadline, reply })
            .map_err(|_| io::Error::other("localStorage writer closed"))?;
        receive
            .recv_timeout(time_remaining(deadline)?)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => io::Error::new(
                    io::ErrorKind::TimedOut,
                    "localStorage shutdown flush deadline expired",
                ),
                mpsc::RecvTimeoutError::Disconnected => {
                    io::Error::other("localStorage writer closed before flush")
                }
            })?
    }

    fn lock(&self, deadline: Option<Instant>) -> io::Result<File> {
        Self::lock_at(&self.0.root, deadline)
    }

    fn lock_at(root: &Path, deadline: Option<Instant>) -> io::Result<File> {
        let mut directory = fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        directory.mode(0o700);
        directory.create(root)?;

        let mut options = OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let lock = options.open(root.join("local-storage.lock"))?;
        if let Some(deadline) = deadline {
            time_remaining(deadline)?;
        }
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
            Err(std::fs::TryLockError::WouldBlock) => {
                if let Some(deadline) = deadline {
                    loop {
                        let remaining = time_remaining(deadline)?;
                        thread::sleep(remaining.min(Duration::from_millis(5)));
                        time_remaining(deadline)?;
                        match lock.try_lock() {
                            Ok(()) => break,
                            Err(std::fs::TryLockError::WouldBlock) => {}
                            Err(std::fs::TryLockError::Error(error)) => return Err(error),
                        }
                    }
                } else {
                    lock.lock()?;
                }
            }
        }
        Ok(lock)
    }

    fn publish(
        &self,
        name: &str,
        deadline: Option<Instant>,
        write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    ) -> io::Result<()> {
        Self::publish_at(&self.0.root, name, deadline, write)
    }

    fn publish_at(
        root: &Path,
        name: &str,
        deadline: Option<Instant>,
        write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    ) -> io::Result<()> {
        let pending = root.join(format!("{name}.pending"));
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(&pending)?;
        let mut writer = BufWriter::new(file);
        write(&mut writer)?;
        writer.flush()?;
        let file = writer.into_inner().map_err(|error| error.into_error())?;
        file.sync_all()?;
        if let Some(deadline) = deadline {
            time_remaining(deadline)?;
        }
        fs::rename(pending, root.join(name))?;
        #[cfg(unix)]
        {
            File::open(root)?.sync_all()
        }
        #[cfg(not(unix))]
        {
            Ok(())
        }
    }

    pub(crate) fn read_database(
        &self,
        name: &[u16],
        deadline: Option<Instant>,
    ) -> io::Result<(u32, Option<Vec<u8>>)> {
        let name = database_name(name);
        if let Some(deadline) = deadline {
            let storage = self.clone();
            run_before(deadline, move || storage.database_file(&name))
        } else {
            self.database_file(&name)
        }
    }

    fn database_file(&self, name: &str) -> io::Result<(u32, Option<Vec<u8>>)> {
        match fs::read(self.0.root.join(name)) {
            Ok(bytes) => {
                let (revision, data) = bytes.split_first_chunk::<4>().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid IndexedDB snapshot")
                })?;
                Ok((
                    u32::from_le_bytes(*revision),
                    (!data.is_empty()).then(|| data.to_vec()),
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok((0, None)),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn write_database(
        &self,
        name: &[u16],
        revision: u32,
        data: Option<Vec<u8>>,
        deadline: Instant,
    ) -> io::Result<bool> {
        let name = database_name(name);
        let storage = self.clone();
        run_before(deadline, move || {
            let _lock = storage.lock(Some(deadline))?;
            if storage.database_file(&name)?.0 != revision {
                return Ok(false);
            }
            let next = revision
                .checked_add(1)
                .ok_or_else(|| io::Error::other("IndexedDB revision exhausted"))?;
            storage.publish(&name, Some(deadline), |file| {
                file.write_all(&next.to_le_bytes())?;
                if let Some(bytes) = data {
                    file.write_all(&bytes)?;
                }
                Ok(())
            })?;
            Ok(true)
        })
    }
}

impl Drop for StorageInner {
    fn drop(&mut self) {
        let deadline = Instant::now() + STORAGE_CLOSE_BUDGET;
        let (reply, receive) = mpsc::sync_channel(1);
        if self
            .writer
            .send(WriterMessage::Flush { deadline, reply })
            .is_err()
        {
            return;
        }
        match receive.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("squark localStorage final flush failed: {error}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                eprintln!("squark localStorage final flush deadline expired");
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("squark localStorage writer closed before final flush");
            }
        }
    }
}

fn canonical_root(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        env::current_dir()?.join(path)
    };
    if let Ok(root) = fs::canonicalize(&absolute) {
        return Ok(root);
    }

    let mut ancestor = absolute.as_path();
    loop {
        match fs::canonicalize(ancestor) {
            Ok(mut root) => {
                for component in absolute
                    .strip_prefix(ancestor)
                    .map_err(io::Error::other)?
                    .components()
                {
                    match component {
                        std::path::Component::CurDir => {}
                        std::path::Component::ParentDir => {
                            root.pop();
                        }
                        std::path::Component::Normal(name) => root.push(name),
                        std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                            unreachable!("suffix after canonical ancestor is relative")
                        }
                    }
                }
                return Ok(fs::canonicalize(&root).unwrap_or(root));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ancestor = ancestor.parent().ok_or(error)?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn time_remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "IndexedDB storage deadline expired",
            )
        })
}

const LOCAL_STORAGE_DEBOUNCE: Duration = Duration::from_millis(75);
const LOCAL_STORAGE_MAX_DELAY: Duration = Duration::from_secs(1);

fn read_local_storage(root: &Path) -> io::Result<Entries> {
    match File::open(root.join("local-storage.json")) {
        Ok(file) => serde_json::from_reader(io::BufReader::new(file)).map_err(io::Error::from),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn apply_change(entries: &mut Entries, change: &Change) {
    match change {
        Change::Set(key, value) => {
            if let Some((_, old)) = entries.iter_mut().find(|(name, _)| name == key) {
                *old = value.clone();
            } else {
                entries.push((key.clone(), value.clone()));
            }
        }
        Change::Remove(key) => entries.retain(|(name, _)| name != key),
        Change::Clear => entries.clear(),
    }
}

fn persist_local_storage(root: &Path, changes: &[Change], deadline: Instant) -> io::Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let _lock = Storage::lock_at(root, Some(deadline))?;
    let mut entries = read_local_storage(root)?;
    for change in changes {
        apply_change(&mut entries, change);
    }
    Storage::publish_at(root, "local-storage.json", Some(deadline), |file| {
        serde_json::to_writer(file, &entries)?;
        Ok(())
    })
}

fn local_storage_writer(root: PathBuf, receiver: Receiver<WriterMessage>) {
    let mut changes = Vec::new();
    let mut first_change: Option<Instant> = None;
    let mut last_change: Option<Instant> = None;
    loop {
        let message = if changes.is_empty() {
            receiver
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        } else {
            let now = Instant::now();
            let quiet_deadline = last_change.unwrap() + LOCAL_STORAGE_DEBOUNCE;
            let cap_deadline = first_change.unwrap() + LOCAL_STORAGE_MAX_DELAY;
            let deadline = quiet_deadline.min(cap_deadline);
            if now >= deadline {
                Err(mpsc::RecvTimeoutError::Timeout)
            } else {
                receiver.recv_timeout(deadline - now)
            }
        };
        match message {
            Ok(WriterMessage::Change(change)) => {
                let now = Instant::now();
                if changes.is_empty() {
                    first_change = Some(now);
                }
                changes.push(change);
                last_change = Some(now);
            }
            Ok(WriterMessage::Flush { deadline, reply }) => {
                let result = persist_local_storage(&root, &changes, deadline);
                if let Err(error) = &result {
                    eprintln!("squark localStorage persistence failed: {error}");
                } else {
                    changes.clear();
                }
                first_change = (!changes.is_empty()).then(Instant::now);
                last_change = first_change;
                let _ = reply.send(result);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let deadline = Instant::now() + STORAGE_CLOSE_BUDGET;
                match persist_local_storage(&root, &changes, deadline) {
                    Ok(()) => changes.clear(),
                    Err(error) => eprintln!("squark localStorage persistence failed: {error}"),
                }
                first_change = (!changes.is_empty()).then(Instant::now);
                last_change = first_change;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let deadline = Instant::now() + STORAGE_CLOSE_BUDGET;
                if let Err(error) = persist_local_storage(&root, &changes, deadline) {
                    eprintln!("squark localStorage persistence failed: {error}");
                }
                break;
            }
        }
    }
}

fn run_before<T: Send + 'static>(
    deadline: Instant,
    operation: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    time_remaining(deadline)?;
    let (send, receive) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("squark-idb".into())
        .spawn(move || {
            let result = time_remaining(deadline).and_then(|_| operation());
            let _ = send.send(result);
        })?;
    receive
        .recv_timeout(time_remaining(deadline)?)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => io::Error::new(
                io::ErrorKind::TimedOut,
                "IndexedDB storage deadline expired",
            ),
            mpsc::RecvTimeoutError::Disconnected => {
                io::Error::other("IndexedDB storage worker closed")
            }
        })?
}

fn database_name(name: &[u16]) -> String {
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    for unit in name {
        hash.update(&unit.to_le_bytes());
    }
    format!("idb-{}", hex(hash.finish().as_ref()))
}

fn data_directory() -> Option<PathBuf> {
    data_directory_with(|name| env::var_os(name))
}

fn data_directory_with(
    mut get_env: impl FnMut(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let xdg = absolute_env("XDG_DATA_HOME", &mut get_env);
    #[cfg(target_os = "linux")]
    let base =
        xdg.or_else(|| absolute_env("HOME", &mut get_env).map(|home| home.join(".local/share")));
    #[cfg(target_os = "macos")]
    let base = xdg.or_else(|| {
        absolute_env("HOME", &mut get_env).map(|home| home.join("Library/Application Support"))
    });
    #[cfg(target_os = "windows")]
    let base = xdg.or_else(|| absolute_env("APPDATA", &mut get_env));
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let base = xdg;
    base.map(|base| base.join("strange-quark"))
}

fn absolute_env(
    name: &str,
    get_env: &mut impl FnMut(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    get_env(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use std::env;

    use super::data_directory_with;

    #[test]
    fn data_directory_uses_xdg_override() {
        let xdg = env::temp_dir().join("strange-quark-xdg-data");
        let root = data_directory_with(|name| match name {
            "XDG_DATA_HOME" => Some(xdg.clone().into_os_string()),
            _ => None,
        });
        assert_eq!(root, Some(xdg.join("strange-quark")));
    }

    #[test]
    fn data_directory_uses_platform_default() {
        let home = env::temp_dir().join("strange-quark-test-home");
        let appdata = env::temp_dir().join("strange-quark-test-appdata");
        let root = data_directory_with(|name| match name {
            "HOME" => Some(home.clone().into_os_string()),
            "APPDATA" => Some(appdata.clone().into_os_string()),
            _ => None,
        });

        #[cfg(target_os = "linux")]
        let expected = home.join(".local/share/strange-quark");
        #[cfg(target_os = "macos")]
        let expected = home.join("Library/Application Support/strange-quark");
        #[cfg(target_os = "windows")]
        let expected = appdata.join("strange-quark");

        assert_eq!(root, Some(expected));
    }
}
