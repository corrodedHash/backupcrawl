//! backupcrawl CLI: detect files which are not backed up.

mod config;
mod printer;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use backupcrawl_core::ignore::IgnoreMatcher;
use backupcrawl_core::walker;
use backupcrawl_host::discovery::{self, SearchPaths};

/// Search for non-backed up files.
#[derive(Debug, Parser)]
#[command(
    name = "backupcrawl",
    version,
    about = "Detect files which are not backed up"
)]
struct Cli {
    /// Subcommand; without one, PATH (default `/`) is crawled.
    #[command(subcommand)]
    command: Option<Command>,

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

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect and manage detector plugins.
    Plugins(PluginsArgs),
}

#[derive(Debug, Args)]
struct PluginsArgs {
    #[command(subcommand)]
    command: PluginsCommand,
}

#[derive(Debug, Subcommand)]
enum PluginsCommand {
    /// List discovered plugins.
    List(PluginDirArgs),

    /// Show the resolved manifest and capabilities of one plugin.
    Info(PluginInfoArgs),
}

/// Options shared by the `plugins` subcommands.
#[derive(Debug, Args)]
struct PluginDirArgs {
    /// Extra plugin directory to scan; repeatable, tried in the given order
    /// before the user, system and bundled sources.
    #[arg(long = "plugin-dir", value_name = "DIR")]
    plugin_dir: Vec<PathBuf>,

    /// Config file override [default: ~/.config/backupcrawl/config.toml].
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct PluginInfoArgs {
    #[command(flatten)]
    dirs: PluginDirArgs,

    /// Plugin name to describe.
    name: String,
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
    if let Some(Command::Plugins(plugins)) = &cli.command {
        return run_plugins(plugins);
    }
    crawl(cli)
}

fn crawl(cli: &Cli) -> Result<()> {
    let config = load_config(cli.config.as_deref())?;

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

fn run_plugins(args: &PluginsArgs) -> Result<()> {
    match &args.command {
        PluginsCommand::List(list) => plugins_list(list),
        PluginsCommand::Info(info) => plugins_info(info),
    }
}

fn plugins_list(args: &PluginDirArgs) -> Result<()> {
    let config = load_config(args.config.as_deref())?;
    let outcome = discover_plugins(&args.plugin_dir);
    printer::print_plugins_list(&outcome, &config.plugins);
    Ok(())
}

fn plugins_info(args: &PluginInfoArgs) -> Result<()> {
    let outcome = discover_plugins(&args.dirs.plugin_dir);

    match outcome.get(&args.name) {
        Some(plugin) => {
            printer::print_plugins_info(plugin);
            Ok(())
        }
        None => {
            for error in &outcome.errors {
                eprintln!("warning: {}", error.display());
            }
            bail!("plugin `{}` not found among discovered plugins", args.name)
        }
    }
}

/// Discovers plugins from explicit dirs plus the user/system/bundled sources.
fn discover_plugins(explicit: &[PathBuf]) -> discovery::DiscoveryOutcome {
    discovery::discover(&SearchPaths::from_env(explicit.to_vec()))
}

/// Loads the config from `path`, falling back to the XDG default.
fn load_config(path: Option<&Path>) -> Result<config::Config> {
    match path {
        Some(path) => config::Config::load(path, true),
        None => match config::Config::default_path() {
            Some(path) => config::Config::load(&path, false),
            None => Ok(config::Config::default()),
        },
    }
}
