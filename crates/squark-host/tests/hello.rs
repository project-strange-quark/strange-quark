// SPDX-License-Identifier: MIT

#[path = "support/timeout.rs"]
mod timeout;

use std::{env, fs, io::Cursor, path::Path, process::Command, time::Duration};

const BINARY: &str = env!("CARGO_BIN_EXE_squark-hello");
const REFERENCE: &[u8] = include_bytes!("fixtures/squark-hello.png");

#[test]
fn headless_png_matches_chromium() {
    if env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }

    let path =
        Path::new(BINARY).with_file_name(format!("squark-hello-test-{}.png", std::process::id()));
    let output = timeout::output(
        Command::new(BINARY)
            .args(["--headless"])
            .arg(&path)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET"),
        Duration::from_secs(30),
    )
    .unwrap();
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{log}");
    eprint!("{log}");
    let actual = fs::read(&path).unwrap();
    fs::remove_file(path).unwrap();

    let (actual_size, actual) = decode(&actual);
    let (reference_size, reference) = decode(REFERENCE);
    assert_eq!(actual_size, [401, 240]);
    assert_eq!(actual_size, reference_size);

    let mut sum = 0u64;
    let mut peak = 0;
    let mut outliers = 0;
    let (actual, actual_tail) = actual.as_chunks::<4>();
    let (reference, reference_tail) = reference.as_chunks::<4>();
    assert!(actual_tail.is_empty() && reference_tail.is_empty());
    assert_eq!(actual.len(), reference.len());
    for (actual, reference) in actual.iter().zip(reference) {
        assert_eq!(actual[3], reference[3], "the frame is opaque");
        let delta = actual
            .iter()
            .zip(reference)
            .map(|(&a, &b)| a.abs_diff(b))
            .max()
            .unwrap();
        peak = peak.max(delta);
        sum += u64::from(delta);
        outliers += usize::from(delta > 8);
    }
    let pixels = actual.len();
    let mean = sum as f64 / pixels as f64;
    let diagnostics =
        format!("mean pixel delta {mean:.3}, peak {peak}, {outliers}/{pixels} above 8");
    eprintln!("{diagnostics}");

    // Most pixels agree within rounding. Allow local differences where
    // Chromium samples rounded edges and the analytic shadow differently.
    assert!(mean <= 0.7, "{diagnostics}");
    assert!(outliers * 200 <= pixels, "{diagnostics}");
    assert!(peak <= 48, "{diagnostics}");
}

fn decode(bytes: &[u8]) -> ([u32; 2], Vec<u8>) {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::ALPHA);
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    assert_eq!(info.buffer_size(), pixels.len());
    ([info.width, info.height], pixels)
}

#[test]
fn window_presents_then_closes_on_timer() {
    if env::var("SQUARK_WINDOW_TESTS").as_deref() != Ok("1") {
        return;
    }
    let output = timeout::output(&mut Command::new(BINARY), Duration::from_secs(25)).unwrap();
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{log}");
    assert!(log.contains("presented its first frame"), "{log}");
    assert!(log.contains("closed on its timer"), "{log}");
    eprint!("{log}");
}
