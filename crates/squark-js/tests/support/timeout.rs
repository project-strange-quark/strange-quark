// SPDX-License-Identifier: MIT

//! Runs test subprocesses under a hard deadline with std alone.
//!
//! This replaces spawning GNU `timeout`, which Windows lacks: its
//! `System32\TIMEOUT.EXE` is a different program, and `Command` searches the
//! system directory before `PATH`. As with `timeout --kill-after`, a process
//! still running at the deadline is killed hard.

#![allow(dead_code)]

use std::{
    io::{self, Read},
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(10);
/// How long to wait for a killed process's pipes to close. A grandchild that
/// inherited them may keep them open; its output is then cut off.
const DRAIN: Duration = Duration::from_secs(2);

/// Like [`Command::output`], but kills the process at `limit`. Stdin is
/// null and stdout and stderr are captured. When the deadline kills the
/// process, its status is unsuccessful and stderr ends with a note saying so.
pub fn output(command: &mut Command, limit: Duration) -> io::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = Capture::start(child.stdout.take());
    let stderr = Capture::start(child.stderr.take());
    let deadline = Instant::now() + limit;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (status, false);
        }
        if Instant::now() >= deadline {
            kill(&mut child);
            break (child.wait()?, true);
        }
        thread::sleep(POLL);
    };
    let stdout = stdout.finish(timed_out);
    let mut stderr = stderr.finish(timed_out);
    if timed_out {
        stderr.extend_from_slice(
            format!(
                "\n[timeout] killed {:?} after {limit:?}\n",
                command.get_program()
            )
            .as_bytes(),
        );
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Kills `child` if it is still running at `limit`, for a process the test
/// keeps talking to. Dropping the guard cancels it; drop it only once the
/// child has been waited for or is about to be.
pub struct Deadline {
    cancel: Option<mpsc::Sender<()>>,
    watchdog: Option<JoinHandle<()>>,
}

impl Deadline {
    pub fn start(child: &Child, limit: Duration) -> Self {
        let pid = child.id();
        let (cancel, cancelled) = mpsc::channel::<()>();
        let watchdog = thread::spawn(move || {
            if cancelled.recv_timeout(limit) == Err(RecvTimeoutError::Timeout) {
                eprintln!("[timeout] killing process {pid} after {limit:?}");
                kill_pid(pid);
            }
        });
        Self {
            cancel: Some(cancel),
            watchdog: Some(watchdog),
        }
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        drop(self.cancel.take());
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
    }
}

struct Capture {
    bytes: Arc<Mutex<Vec<u8>>>,
    done: mpsc::Receiver<()>,
}

impl Capture {
    fn start(pipe: Option<impl Read + Send + 'static>) -> Self {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (finished, done) = mpsc::channel();
        let sink = Arc::clone(&bytes);
        thread::spawn(move || {
            if let Some(mut pipe) = pipe {
                let mut chunk = [0u8; 8192];
                while let Ok(len @ 1..) = pipe.read(&mut chunk) {
                    sink.lock().unwrap().extend_from_slice(&chunk[..len]);
                }
            }
            let _ = finished.send(());
        });
        Self { bytes, done }
    }

    /// Waits for the pipe to close: without limit after a normal exit, and
    /// for [`DRAIN`] after a kill.
    fn finish(self, killed: bool) -> Vec<u8> {
        let _ = if killed {
            self.done.recv_timeout(DRAIN).map_err(|_| ())
        } else {
            self.done.recv().map_err(|_| ())
        };
        std::mem::take(&mut *self.bytes.lock().unwrap())
    }
}

/// Kills the process and, on Windows, its descendants, which GNU `timeout`
/// reaches through its process group.
fn kill(child: &mut Child) {
    if cfg!(windows) {
        kill_pid(child.id());
    }
    let _ = child.kill();
}

fn kill_pid(pid: u32) {
    let pid = pid.to_string();
    let _ = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        Command::new("kill")
            .args(["-KILL", &pid])
            .stderr(Stdio::null())
            .status()
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(script: &str) -> Command {
        let mut command = Command::new("node");
        command.args(["-e", script]);
        command
    }

    #[test]
    fn returns_the_output_of_a_process_that_finishes() {
        let output = output(
            &mut node("process.stdout.write('out'); process.stderr.write('err')"),
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
    }

    #[test]
    fn kills_a_process_at_the_deadline() {
        let started = Instant::now();
        let output = output(
            &mut node("process.stdout.write('started'); setInterval(() => {}, 1000)"),
            Duration::from_millis(1500),
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"started");
        assert!(String::from_utf8_lossy(&output.stderr).contains("[timeout] killed"));
    }

    #[test]
    fn a_deadline_kills_a_spawned_child_and_cancels_when_dropped() {
        let mut child = node("setInterval(() => {}, 1000)").spawn().unwrap();
        let _deadline = Deadline::start(&child, Duration::from_millis(500));
        assert!(!child.wait().unwrap().success());

        let mut child = node("setTimeout(() => {}, 300)").spawn().unwrap();
        drop(Deadline::start(&child, Duration::from_secs(30)));
        assert!(child.wait().unwrap().success());
    }
}
