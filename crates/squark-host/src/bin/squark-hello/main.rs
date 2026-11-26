// SPDX-License-Identifier: MIT

mod scene;
mod window;

use std::{env, fs::File, path::Path, process::ExitCode};

use squark_gpu::{Error, Renderer};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("squark-hello: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Error> {
    let mut args = env::args_os().skip(1);
    match (args.next(), args.next(), args.next()) {
        (None, None, None) => window::run(),
        (Some(option), Some(path), None) if option == "--headless" => write_png(Path::new(&path)),
        _ => Err("usage: squark-hello [--headless <output.png>]".into()),
    }
}

fn write_png(path: &Path) -> Result<(), Error> {
    let renderer = pollster::block_on(Renderer::headless())?;
    eprintln!(
        "{} adapter: {}",
        squark_gpu::native_backend_name(),
        renderer.adapter_name()
    );
    let rgba = renderer.render_rgba(&scene::frame(), scene::SIZE)?;
    let [width, height] = scene::SIZE;
    let mut encoder = png::Encoder::new(File::create(path)?, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&rgba)?;
    writer.finish()?;
    Ok(())
}
