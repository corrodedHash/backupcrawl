//! backupcrawl CLI: detect files which are not backed up.

mod config;
mod printer;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{ArgAction, Parser, ValueEnum};

use backupcrawl_core::ignore::IgnoreMatcher;
use backupcrawl_core::walker;

/// Search for non-backed up files.
#[derive(Debug, Parser)]
#[command(
    name = "backupcrawl",
    version,
    about = "Detect files which are not backed up"
)]
struct Cli {
    /// Directory to scan.
    #[arg(default_value = "/")]
    path: PathBuf,

    /// Output format.
    #[arg(long, short, value_enum, default_value_t = Format::Console)]
    format: Format,

    /// Also show backed-up paths (no effect until detectors land).
    #[arg(long, short)]
    all: bool,

    /// Extra ignore glob, repeatable; applied on top of the config file.
    #[arg(long, short, value_name = "GLOB")]
    ignore: Vec<String>,

    /// Config file override [default: ~/.config/backupcrawl/config.toml].
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Increase log verbosity: -v info, -vv debug.
    #[arg(long, short, action = ArgAction::Count)]
    verbose: u8,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Format {
    Console,
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level)).init();

    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    let config = match &cli.config {
        Some(path) => config::Config::load(path, true)?,
        None => match config::Config::default_path() {
            Some(path) => config::Config::load(&path, false)?,
            None => config::Config::default(),
        },
    };

    let patterns = config
        .ignore
        .paths
        .iter()
        .cloned()
        .chain(cli.ignore.iter().cloned());
    let matcher = IgnoreMatcher::new(patterns).context("invalid ignore patterns")?;

    let result = walker::scan(&cli.path, &matcher);
    match cli.format {
        Format::Console => printer::print_console(&result, cli.all),
        Format::Json => printer::print_json(&result)?,
    }
    Ok(())
}
