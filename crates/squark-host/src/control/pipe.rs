// SPDX-License-Identifier: MIT

//! The test-control channel on Windows: a named pipe.
//!
//! std has no Unix domain sockets on Windows, and Node's `net` module, which
//! every test driver uses, does not speak `AF_UNIX` there either: it maps a
//! `\\.\pipe\name` path to a named pipe. A named pipe therefore keeps the
//! drivers' `net.connect(path)` unchanged, needs no port or secret handshake,
//! and is closed to other users by its access control list:
//!
//! - the pipe's DACL grants access to the current user's SID alone (the
//!   default DACL would give everyone read access);
//! - `PIPE_REJECT_REMOTE_CLIENTS` refuses SMB clients from other machines;
//! - `FILE_FLAG_FIRST_PIPE_INSTANCE` fails `bind` when another process has
//!   already created a pipe of that name, so the host cannot be squatted.
//!
//! The pipe has one instance and serves one client at a time, like the
//! accept loop on Unix. The handle stays in `PIPE_NOWAIT` mode, so nothing
//! on it blocks: `accept` polls, reads peek first, and a write that finds
//! the pipe's buffer full retries until [`REQUEST_TIMEOUT`], the same bound
//! as the Unix socket's write timeout. Disconnecting waits a bounded time
//! for the client to read the last reply.

use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read, Write},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{
        ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING, HANDLE,
        INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX},
    System::{
        IO::CancelSynchronousIo,
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_NOWAIT,
            PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PeekNamedPipe,
        },
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

use super::{IO_POLL, REQUEST_TIMEOUT};

const PIPE_PREFIX: &str = r"\\.\pipe\";
const BUFFER: u32 = 64 * 1024;
/// How long a disconnect waits for the client to read what is buffered.
const FLUSH_LIMIT: Duration = Duration::from_secs(2);
/// How often a write retries while the pipe's buffer is full.
const WRITE_RETRY: Duration = Duration::from_millis(5);

pub(super) struct Listener {
    pipe: File,
    write_timeout: Duration,
    flush_limit: Duration,
}

impl Listener {
    /// Creates the pipe. `path` must be a local pipe name, `\\.\pipe\…`.
    pub(super) fn bind(path: &Path) -> io::Result<Self> {
        let name = path.as_os_str();
        if !name
            .to_str()
            .is_some_and(|name| name.len() > PIPE_PREFIX.len() && name.starts_with(PIPE_PREFIX))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the Windows test-control path must be a named pipe, {PIPE_PREFIX}<name>: {}",
                    path.display()
                ),
            ));
        }
        let name = wide(name);
        let descriptor = SecurityDescriptor::current_user_only()?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        // SAFETY: `name` is a NUL-terminated UTF-16 string and `attributes`
        // points at a security descriptor that outlives the call.
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                BUFFER,
                BUFFER,
                0,
                &attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `handle` is a new, valid handle that nothing else owns.
        let pipe = File::from(unsafe { OwnedHandle::from_raw_handle(handle) });
        Ok(Self {
            pipe,
            write_timeout: REQUEST_TIMEOUT,
            flush_limit: FLUSH_LIMIT,
        })
    }

    /// Returns the connected client, or `WouldBlock` while there is none.
    pub(super) fn accept(&self) -> io::Result<Option<Stream<'_>>> {
        match self.listen() {
            Ok(()) => {}
            Err(error) => match error.raw_os_error().map(|code| code as u32) {
                Some(ERROR_PIPE_LISTENING) => return Err(io::ErrorKind::WouldBlock.into()),
                // A client connected and left before it was served.
                Some(ERROR_NO_DATA) => {
                    self.disconnect();
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                _ => return Err(error),
            },
        }
        Ok(Some(Stream { listener: self }))
    }

    /// Puts the pipe in the listening state, so a client can open it, and
    /// reports `Ok` once one has.
    fn listen(&self) -> io::Result<()> {
        // SAFETY: the handle is a valid pipe handle in `PIPE_NOWAIT` mode, so
        // the call returns at once; it is not overlapped.
        if unsafe { ConnectNamedPipe(self.handle(), ptr::null_mut()) } != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error().map(|code| code as u32) {
            Some(ERROR_PIPE_CONNECTED) => Ok(()),
            _ => Err(error),
        }
    }

    fn handle(&self) -> HANDLE {
        self.pipe.as_raw_handle()
    }

    /// Ends the current client's connection and listens again at once:
    /// between `DisconnectNamedPipe` and the next `ConnectNamedPipe` a
    /// client's open fails with `ERROR_PIPE_BUSY`.
    ///
    /// Disconnecting discards what the client has not read yet, such as the
    /// reply to `app.quit`, so it first flushes, which waits for the client
    /// to read it. A client that stops reading would hold that flush, and the
    /// host's shutdown, forever, so the flush runs on its own thread for at
    /// most `flush_limit` and is then cancelled.
    ///
    /// The handle is not overlapped, so Windows runs one call on it at a
    /// time: a `DisconnectNamedPipe` issued while the flush is pending would
    /// wait behind it. The flush therefore has to end, by cancellation if the
    /// client never reads, before the disconnect.
    fn disconnect(&self) {
        let handle = self.handle() as usize;
        let (flushed, done) = mpsc::channel();
        let flusher = thread::spawn(move || {
            // SAFETY: the pipe handle stays open until this thread is joined
            // below; FlushFileBuffers only waits on it.
            unsafe { FlushFileBuffers(handle as HANDLE) };
            let _ = flushed.send(());
        });
        if let Err(RecvTimeoutError::Timeout) = done.recv_timeout(self.flush_limit) {
            // A cancel that lands before the thread has entered the flush
            // finds nothing to cancel, so repeat it until the flush returns.
            while let Err(RecvTimeoutError::Timeout) = done.recv_timeout(WRITE_RETRY) {
                // SAFETY: the thread handle is valid until `join` below, and
                // the flush is the only I/O that thread performs.
                unsafe { CancelSynchronousIo(flusher.as_raw_handle()) };
            }
        }
        let _ = flusher.join();
        // SAFETY: the handle is a valid server end of a pipe.
        unsafe { DisconnectNamedPipe(self.handle()) };
        let _ = self.listen();
    }
}

/// The connected end of the pipe. Dropping it disconnects the client.
pub(super) struct Stream<'a> {
    listener: &'a Listener,
}

impl Read for Stream<'_> {
    /// Reads what is available. With nothing to read it waits [`IO_POLL`]
    /// and reports `WouldBlock`, like a Unix socket's read timeout; a client
    /// that has closed its end reads as end of file.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut available = 0u32;
        // SAFETY: the handle is a valid pipe handle; no buffer is passed, so
        // only the count of available bytes is written.
        let peeked = unsafe {
            PeekNamedPipe(
                self.listener.handle(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                &mut available,
                ptr::null_mut(),
            )
        };
        if peeked == 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error().map(|code| code as u32) {
                Some(ERROR_BROKEN_PIPE | ERROR_NO_DATA) => Ok(0),
                _ => Err(error),
            };
        }
        if available == 0 {
            thread::sleep(IO_POLL);
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let len = buf.len().min(available as usize);
        (&self.listener.pipe).read(&mut buf[..len])
    }
}

impl Write for Stream<'_> {
    /// Writes at most one buffer's worth. In `PIPE_NOWAIT` mode a write
    /// that does not fit in the pipe's free buffer space writes nothing, so
    /// it retries while the client drains the pipe and gives up with
    /// `TimedOut` after the listener's write timeout.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let chunk = &buf[..buf.len().min(BUFFER as usize)];
        let deadline = Instant::now() + self.listener.write_timeout;
        loop {
            match (&self.listener.pipe).write(chunk) {
                Ok(0) if !chunk.is_empty() => {}
                result => return result,
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the test-control client stopped reading",
                ));
            }
            thread::sleep(WRITE_RETRY);
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Stream<'_> {
    fn drop(&mut self) {
        self.listener.disconnect();
    }
}

/// A self-relative security descriptor allocated by `LocalAlloc`.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    /// A protected DACL with one entry: full access for the current user.
    fn current_user_only() -> io::Result<Self> {
        let sddl = wide(OsStr::new(&format!("D:P(A;;GA;;;{})", current_user_sid()?)));
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated and `descriptor` receives a
        // `LocalAlloc` pointer that `Drop` frees.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `LocalAlloc` and is freed once.
        unsafe { LocalFree(self.0) };
    }
}

/// The process user's SID in string form, `S-1-5-21-…`.
fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo handle that needs no
    // closing; `token` receives a new handle that `OwnedHandle` closes.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `token` is a valid handle that nothing else owns.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut len = 0u32;
    // SAFETY: a null buffer of length zero only asks for the needed size.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut len,
        )
    };
    // `u64` words keep the buffer aligned for `TOKEN_USER`'s pointer.
    let mut buffer = vec![0u64; (len as usize).div_ceil(size_of::<u64>())];
    // SAFETY: `buffer` holds at least `len` writable bytes.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            len,
            &mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call above filled `buffer` with an aligned `TOKEN_USER`
    // whose SID points into the same buffer, which outlives this borrow.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut string = ptr::null_mut();
    // SAFETY: the SID is valid, and `string` receives a `LocalAlloc`
    // NUL-terminated UTF-16 string that is freed below.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut string) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `string` is NUL-terminated, so the scan stays in bounds.
    let sid = unsafe {
        let len = (0..).take_while(|&i| *string.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(string, len))
    };
    // SAFETY: `string` came from `LocalAlloc` and is freed once.
    unsafe { LocalFree(string.cast()) };
    Ok(sid)
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use std::{
        fs::OpenOptions,
        io::{BufRead, BufReader, Read, Write},
        path::PathBuf,
    };

    use super::*;

    fn name(tag: &str) -> PathBuf {
        PathBuf::from(format!(
            r"\\.\pipe\squark-test-{tag}-{}",
            std::process::id()
        ))
    }

    fn accept(listener: &Listener) -> Stream<'_> {
        for _ in 0..200 {
            match listener.accept() {
                Ok(Some(stream)) => return stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(IO_POLL),
                other => panic!("accept failed: {:?}", other.err()),
            }
        }
        panic!("no client connected");
    }

    #[test]
    fn rejects_paths_that_are_not_pipe_names() {
        for path in [r"C:\Temp\squark.sock", r"\\.\pipe\", "squark.sock"] {
            let error = Listener::bind(Path::new(path)).err().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{path}");
        }
    }

    #[test]
    fn refuses_a_second_pipe_of_the_same_name() {
        let path = name("squat");
        let _first = Listener::bind(&path).unwrap();
        assert!(Listener::bind(&path).is_err());
    }

    #[test]
    fn exchanges_lines_and_serves_clients_in_turn() {
        let path = name("lines");
        let listener = Listener::bind(&path).unwrap();
        assert_eq!(
            listener.accept().err().map(|error| error.kind()),
            Some(io::ErrorKind::WouldBlock)
        );
        for round in 0..2 {
            let client = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            let mut stream = accept(&listener);
            let mut buf = [0u8; 8];
            assert_eq!(
                stream.read(&mut buf).err().map(|error| error.kind()),
                Some(io::ErrorKind::WouldBlock)
            );
            (&client)
                .write_all(format!("ping {round}\n").as_bytes())
                .unwrap();
            let mut line = Vec::new();
            loop {
                match BufReader::new(&mut stream).read_until(b'\n', &mut line) {
                    Ok(_) if line.ends_with(b"\n") => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("{error}"),
                }
            }
            assert_eq!(line, format!("ping {round}\n").as_bytes());
            let reply = vec![b'x'; 3 * BUFFER as usize];
            let reader = thread::spawn(move || {
                // The server's disconnect ends the stream with an error
                // rather than a zero-length read; count what arrived first.
                let mut received = 0;
                let mut chunk = [0u8; 4096];
                while let Ok(len @ 1..) = (&client).read(&mut chunk) {
                    received += len;
                }
                received
            });
            stream.write_all(&reply).unwrap();
            drop(stream);
            assert_eq!(reader.join().unwrap(), reply.len());
        }
    }

    #[test]
    fn a_write_to_a_client_that_stops_reading_times_out() {
        let path = name("stalled-write");
        let mut listener = Listener::bind(&path).unwrap();
        listener.write_timeout = Duration::from_millis(300);
        listener.flush_limit = Duration::from_millis(300);
        let _client = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let mut stream = accept(&listener);
        let started = Instant::now();
        let error = stream
            .write_all(&vec![b'x'; 4 * BUFFER as usize])
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        // Disconnecting from the stalled client is bounded too.
        drop(stream);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn disconnecting_from_a_client_that_stops_reading_is_bounded() {
        let path = name("stalled-flush");
        let mut listener = Listener::bind(&path).unwrap();
        listener.flush_limit = Duration::from_millis(300);
        let _client = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let mut stream = accept(&listener);
        stream.write_all(b"a reply nobody reads\n").unwrap();
        let started = Instant::now();
        drop(stream);
        assert!(started.elapsed() < Duration::from_secs(5));
        // The pipe listens again for the next client.
        let _next = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        drop(accept(&listener));
    }

    #[test]
    fn a_closed_client_reads_as_end_of_file() {
        let path = name("eof");
        let listener = Listener::bind(&path).unwrap();
        let client = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let mut stream = accept(&listener);
        drop(client);
        assert_eq!(stream.read(&mut [0u8; 4]).unwrap(), 0);
    }
}
