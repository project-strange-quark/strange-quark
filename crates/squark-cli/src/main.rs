// SPDX-License-Identifier: MIT
//! `squark` CLI entry point.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use squark_cli::build::{BuildOptions, build_app};
use squark_cli::dev::{DevOptions, run_dev};
use squark_cli::error::CliError;
use squark_cli::inspect::{format_inspection, inspect_path};
use squark_cli::pack::{PackOptions, pack_directory};
use squark_cli::screenshot::{ScreenshotOptions, run_screenshot};

#[derive(Debug, Parser)]
#[command(
    name = "squark",
    version,
    about = "Build, pack, inspect, and screenshot Strange Quark applications"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Compile index.html -> document.bin and pack a built web directory into a .sqpak or runtime binary
    Pack(PackArgs),
    /// Inspect a .sqpak archive, appended runtime binary, or document.bin
    Inspect(InspectArgs),
    /// Run a Vite dev server with @strange-quark/vite-plugin and launch the native runtime
    Dev(DevArgs),
    /// Build an app and pack it into a standalone native binary
    Build(BuildArgs),
    /// Render an HTML file, directory, .sqpak, or packed binary headlessly to PNG
    Screenshot(ScreenshotArgs),
}

#[derive(Debug, Args)]
struct PackArgs {
    /// Directory containing the built web assets (including index.html)
    web_dir: PathBuf,
    /// Write the .sqpak archive to this path
    #[arg(long)]
    out: Option<PathBuf>,
    /// Assemble a standalone executable by appending the .sqpak + trailer to --runtime
    #[arg(long)]
    binary: Option<PathBuf>,
    /// Prebuilt squark-runtime binary used with --binary
    #[arg(long)]
    runtime: Option<PathBuf>,
    /// Override the entry HTML file (defaults to <web-dir>/index.html)
    #[arg(long)]
    html: Option<PathBuf>,
    /// Include .map sourcemap files in the pack (omitted by default)
    #[arg(long)]
    include_sourcemaps: bool,
    /// Override the app name written to /__squark/manifest.json
    #[arg(long)]
    name: Option<String>,
}

#[derive(Debug, Args)]
struct DevArgs {
    /// Application directory containing package.json / vite.config.ts
    #[arg(default_value = ".")]
    dir: PathBuf,
    /// Vite port; 0 picks a free port
    #[arg(long, default_value_t = 5173)]
    port: u16,
    /// Vite bind address (default: 127.0.0.1)
    #[arg(long)]
    host: Option<String>,
    /// Path to squark.config.{ts,mjs,js,json}
    #[arg(long)]
    config: Option<PathBuf>,
    /// Prebuilt squark-runtime binary path (or set SQUARK_RUNTIME_BIN)
    #[arg(long)]
    runtime: Option<PathBuf>,
    /// Window width in CSS pixels (default: squark.config window.width or 1280)
    #[arg(long)]
    width: Option<u32>,
    /// Window height in CSS pixels (default: squark.config window.height or 800)
    #[arg(long)]
    height: Option<u32>,
    /// Directory for index.html, squark-dev.json and the dev pack
    #[arg(long = "work-dir")]
    work_dir: Option<PathBuf>,
    /// Enable the runtime test-control socket at this path
    #[arg(long = "test-control")]
    test_control: Option<PathBuf>,
    /// Run the runtime without a window (requires --test-control)
    #[arg(long)]
    headless: bool,
    /// Write the dev pack and stop without launching the runtime
    #[arg(long = "prepare-only")]
    prepare_only: bool,
}

#[derive(Debug, Args)]
struct BuildArgs {
    /// Application directory containing package.json / squark.config.ts
    #[arg(default_value = ".")]
    dir: PathBuf,
    /// Path to squark.config.{ts,mjs,js,json}
    #[arg(long)]
    config: Option<PathBuf>,
    /// Output native binary path (default: <dir>/dist/<name>)
    #[arg(long)]
    out: Option<PathBuf>,
    /// Also write the standalone .sqpak archive to this path
    #[arg(long = "sqpak-out")]
    sqpak_out: Option<PathBuf>,
    /// Prebuilt squark-runtime binary path (or set SQUARK_RUNTIME_BIN)
    #[arg(long)]
    runtime: Option<PathBuf>,
    /// Override the built web output directory (auto-detects dist/ then build/)
    #[arg(long)]
    dist: Option<PathBuf>,
    /// Skip running the app's own build command
    #[arg(long)]
    skip_app_build: bool,
    /// Include .map sourcemap files in the pack (omitted by default)
    #[arg(long)]
    include_sourcemaps: bool,
}

#[derive(Debug, Args)]
struct ScreenshotArgs {
    /// Input HTML file, web directory, .sqpak archive, or packed runtime binary
    input: PathBuf,
    /// Output PNG path
    #[arg(long)]
    out: PathBuf,
    /// Viewport width in CSS pixels
    #[arg(long, default_value_t = 800)]
    width: u32,
    /// Viewport height in CSS pixels
    #[arg(long, default_value_t = 600)]
    height: u32,
    /// Device pixel ratio
    #[arg(long, default_value_t = 1.0)]
    dpr: f64,
    /// Timeout in seconds before aborting headless render
    #[arg(long, default_value_t = 30)]
    timeout: u64,
    /// GPU adapter preference: auto, software, or hardware
    #[arg(long = "gpu-adapter", default_value = "auto")]
    gpu_adapter: String,
    /// Path to squark-runtime binary (or set SQUARK_RUNTIME_BIN)
    #[arg(long)]
    runtime: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct InspectArgs {
    /// Dump and validate a .sqpak archive, appended runtime binary, or document.bin
    #[arg(long = "dump-pack")]
    dump_pack: Option<PathBuf>,
    /// Emit JSON instead of human-readable text
    #[arg(long)]
    json: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        Command::Pack(args) => {
            if args.out.is_none() && args.binary.is_none() {
                return Err(CliError::Usage(
                    "specify at least one of --out <file.sqpak> or --binary <out-exe>".to_owned(),
                ));
            }
            let summary = pack_directory(&PackOptions {
                web_dir: args.web_dir,
                html_path: args.html,
                out_sqpak: args.out.clone(),
                out_binary: args.binary.clone(),
                runtime_binary: args.runtime,
                include_sourcemaps: args.include_sourcemaps,
                app_name: args.name,
                skip_other_html: false,
                window: None,
                network_allow: Vec::new(),
                extra_assets: Vec::new(),
            })?;
            println!(
                "packed {} entries ({} bytes, crc32={:#010x}) from {} (nodes={}, batches={}, scripts={})",
                summary.entry_count,
                summary.sqpak_bytes,
                summary.sqpak_crc32,
                summary.entry_html,
                summary.document.node_count,
                summary.document.batch_count,
                summary.document.script_count
            );
            Ok(())
        }
        Command::Inspect(args) => {
            let Some(path) = args.dump_pack else {
                return Err(CliError::Usage(
                    "pass --dump-pack <file> (runtime overlays arrive in a later phase)".to_owned(),
                ));
            };
            let info = inspect_path(&path)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                print!("{}", format_inspection(&info));
            }
            Ok(())
        }
        Command::Dev(args) => {
            let summary = run_dev(&DevOptions {
                project_dir: args.dir,
                config_path: args.config,
                port: args.port,
                host: args.host,
                runtime_binary: args.runtime,
                width: args.width,
                height: args.height,
                work_dir: args.work_dir,
                test_control: args.test_control,
                headless: args.headless,
                prepare_only: args.prepare_only,
            })?;
            if args.prepare_only {
                println!(
                    "wrote dev pack for {} at {} (origin {})",
                    summary.app_name,
                    summary.pack.display(),
                    summary.origin
                );
            }
            Ok(())
        }
        Command::Build(args) => {
            let summary = build_app(&BuildOptions {
                project_dir: args.dir,
                config_path: args.config,
                out_binary: args.out,
                out_sqpak: args.sqpak_out,
                runtime_binary: args.runtime,
                dist_dir: args.dist,
                skip_app_build: args.skip_app_build,
                include_sourcemaps: args.include_sourcemaps,
            })?;
            println!(
                "built {} -> {} ({:.1} MiB; {} entries, {} bytes pack, crc32={:#010x})",
                summary.app_name,
                summary.out_binary.display(),
                summary.out_bytes as f64 / (1024.0 * 1024.0),
                summary.pack.entry_count,
                summary.pack.sqpak_bytes,
                summary.pack.sqpak_crc32
            );
            Ok(())
        }
        Command::Screenshot(args) => {
            run_screenshot(&ScreenshotOptions {
                input: args.input,
                out: args.out.clone(),
                width: args.width,
                height: args.height,
                dpr: args.dpr,
                gpu_adapter: args.gpu_adapter,
                runtime_binary: args.runtime,
                timeout_secs: args.timeout,
            })?;
            println!("wrote screenshot {}", args.out.display());
            Ok(())
        }
    }
}
