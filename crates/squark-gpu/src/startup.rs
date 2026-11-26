// SPDX-License-Identifier: MIT

use std::sync::OnceLock;
use std::time::Instant;

/// An opt-in phase duration for the initial window and renderer setup.
#[doc(hidden)]
#[derive(Debug)]
pub struct StartupSpan(Option<(&'static str, Instant)>);

impl StartupSpan {
    pub fn new(phase: &'static str) -> Self {
        let start = enabled().then(|| (phase, Instant::now()));
        Self(start)
    }
}

impl Drop for StartupSpan {
    fn drop(&mut self) {
        if let Some((phase, start)) = self.0 {
            eprintln!(
                "squark-startup {phase} {:.3} ms",
                start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("SQUARK_STARTUP_PROFILE").is_some())
}
