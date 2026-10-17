//! backupcrawl CLI: detect files which are not backed up.

mod config;
mod printer;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use backupcrawl_core::detector::{DetectorSlot, Hooks};
use backupcrawl_core::ignore::IgnoreMatcher;
use backupcrawl_core::walker;
use backupcrawl_host::discovery::{self, DiscoveredPlugin, DiscoveryOutcome, SearchPaths};
use backupcrawl_host::trust::{self, PluginTrust, TrustPrompt, TrustRequest};
use backupcrawl_host::{Limits, Runtime};

use crate::printer::PluginRunStatus;

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

    /// Also show clean (backed-up) claims in the panels.
    #[arg(long, short)]
    all: bool,

    /// Extra ignore glob, repeatable; applied on top of the config file.
    #[arg(long, short, value_name = "GLOB")]
    ignore: Vec<String>,

    /// Extra plugin directory to scan; repeatable, tried in the given order
    /// before the user, system and bundled sources.
    #[arg(long = "plugin-dir", value_name = "DIR")]
    plugin_dir: Vec<PathBuf>,

    /// Only run the named detectors (comma-separated or repeated).
    #[arg(
        long = "plugins",
        value_name = "a,b,c",
        value_delimiter = ',',
        conflicts_with = "no_plugins"
    )]
    plugins: Vec<String>,

    /// Run the crawl without any detectors.
    #[arg(long = "no-plugins")]
    no_plugins: bool,

    /// Trust the named plugin without prompting and persist its manifest hash;
    /// repeatable. Non-interactive equivalent of the trust prompt.
    #[arg(long = "trust-plugin", value_name = "NAME")]
    trust_plugin: Vec<String>,

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

    /// Check the trust state of every discovered plugin (read-only, no prompt).
    Verify(PluginDirArgs),
}

/// Options shared by the `plugins` subcommands.
#[derive(Debug, Args)]
struct PluginDirArgs {
    /// Extra plugin directory to scan; repeatable, tried in the given order
    /// before the user, system and bundled sources.
    #[arg(long = "plugin-dir", value_name = "DIR")]
    plugin_dir: Vec<PathBuf>,

    /// Trust the named plugin without prompting and persist its manifest hash;
    /// repeatable. Non-interactive equivalent of the trust prompt.
    #[arg(long = "trust-plugin", value_name = "NAME")]
    trust_plugin: Vec<String>,

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

/// Crawls `cli.path` with the selected detectors and prints the result.
fn crawl(cli: &Cli) -> Result<()> {
    let context = trust_context(cli.config.as_deref())?;
    let config = &context.config;

    let patterns = config
        .ignore
        .paths
        .iter()
        .cloned()
        .chain(cli.ignore.iter().cloned());
    let matcher = IgnoreMatcher::new(patterns).context("invalid ignore patterns")?;

    let outcome = discover_plugins(&cli.plugin_dir);
    for error in &outcome.errors {
        eprintln!("warning: {}", error.display());
    }

    let enabled = select_enabled(cli, config, &outcome);
    for name in requested_names(cli, config) {
        if outcome.get(name).is_none() {
            eprintln!("warning: plugin `{name}` was requested but not discovered");
        }
    }

    let plugins = ordered_plugins(&outcome, &enabled, &config.plugins.order);
    let trust = apply_trust(&context, &plugins, &outcome, &cli.trust_plugin, true)?;

    // Build the detector slots and remember, in detector order, what happened to
    // each plugin so the run report line can include skipped ones too.
    enum Pending {
        Ran(usize),
        Skipped(String),
    }

    let mut slots: Vec<DetectorSlot> = Vec::new();
    let mut pending: Vec<(String, Pending)> = Vec::new();
    let mut runtime: Option<Runtime> = None;

    for plugin in &plugins {
        if !trust[&plugin.name].trusted {
            pending.push((
                plugin.name.clone(),
                Pending::Skipped("untrusted".to_owned()),
            ));
            continue;
        }
        if runtime.is_none() {
            runtime = Some(Runtime::new(Limits::default())?);
        }
        match runtime
            .as_ref()
            .expect("runtime")
            .instantiate_plugin(plugin, &cli.path)
        {
            Ok(detector) => {
                let index = slots.len();
                slots.push(DetectorSlot::new(
                    plugin.name.clone(),
                    plugin_config(config, &plugin.name),
                    hooks_of(plugin),
                    Box::new(detector),
                ));
                pending.push((plugin.name.clone(), Pending::Ran(index)));
            }
            Err(err) => {
                eprintln!("warning: cannot load plugin `{}`: {err:#}", plugin.name);
                pending.push((
                    plugin.name.clone(),
                    Pending::Skipped(format!("load failed: {err:#}")),
                ));
            }
        }
    }

    let result = walker::scan(&cli.path, &matcher, &mut slots);

    let run: Vec<PluginRunStatus> = pending
        .into_iter()
        .map(|(name, pending)| match pending {
            Pending::Skipped(reason) => PluginRunStatus::disabled(name, reason),
            Pending::Ran(index) => PluginRunStatus::ran(name, &slots[index].detector.report()),
        })
        .collect();

    match cli.format {
        Format::Console => printer::print_console(&result, cli.all, &run),
        Format::Json => printer::print_json(&result)?,
    }
    Ok(())
}

/// The plugin names the run explicitly asked for (flags first, then config),
/// used to warn about names that were not discovered.
fn requested_names<'a>(cli: &'a Cli, config: &'a config::Config) -> Vec<&'a String> {
    if !cli.plugins.is_empty() {
        cli.plugins.iter().collect()
    } else {
        config.plugins.enabled.iter().collect()
    }
}

/// Resolves the enabled detector names: `--no-plugins` beats `--plugins`, which
/// beats `[plugins] enabled`, which (when empty) means "everything discovered".
fn select_enabled(
    cli: &Cli,
    config: &config::Config,
    outcome: &DiscoveryOutcome,
) -> BTreeSet<String> {
    if cli.no_plugins {
        return BTreeSet::new();
    }
    if !cli.plugins.is_empty() {
        return cli.plugins.iter().cloned().collect();
    }
    if !config.plugins.enabled.is_empty() {
        return config.plugins.enabled.iter().cloned().collect();
    }
    outcome
        .plugins
        .iter()
        .map(|plugin| plugin.name.clone())
        .collect()
}

/// Orders the selected plugins: `[plugins] order` (when set) pins the listed
/// plugins first, in the given sequence; the rest follow by ascending
/// `hooks.priority` and then name, so the order is always deterministic.
fn ordered_plugins(
    outcome: &DiscoveryOutcome,
    enabled: &BTreeSet<String>,
    order: &[String],
) -> Vec<DiscoveredPlugin> {
    let position = |name: &str| order.iter().position(|item| item == name);
    let mut selected: Vec<DiscoveredPlugin> = outcome
        .plugins
        .iter()
        .filter(|plugin| enabled.contains(&plugin.name))
        .cloned()
        .collect();

    selected.sort_by(
        |left, right| match (position(&left.name), position(&right.name)) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left
                .manifest
                .hooks
                .priority
                .cmp(&right.manifest.hooks.priority)
                .then_with(|| left.name.cmp(&right.name)),
        },
    );
    selected
}

/// The hooks a discovered plugin declared.
fn hooks_of(plugin: &DiscoveredPlugin) -> Hooks {
    Hooks {
        dir: plugin.manifest.hooks.dir,
        file: plugin.manifest.hooks.file,
        symlink: plugin.manifest.hooks.symlink,
    }
}

/// Flattens a `[plugins.<name>]` table into the string pairs handed to a
/// detector's `init`. Strings are passed verbatim; other TOML values use their
/// canonical textual form.
fn plugin_config(config: &config::Config, name: &str) -> Vec<(String, String)> {
    config
        .plugins
        .extra
        .get(name)
        .map(|table| {
            table
                .iter()
                .map(|(key, value)| (key.clone(), toml_value(value)))
                .collect()
        })
        .unwrap_or_default()
}

fn toml_value(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn run_plugins(args: &PluginsArgs) -> Result<()> {
    match &args.command {
        PluginsCommand::List(list) => plugins_list(list),
        PluginsCommand::Info(info) => plugins_info(info),
        PluginsCommand::Verify(verify) => plugins_verify(verify),
    }
}

fn plugins_list(args: &PluginDirArgs) -> Result<()> {
    let context = trust_context(args.config.as_deref())?;
    let outcome = discover_plugins(&args.plugin_dir);
    let trust = apply_trust(
        &context,
        &outcome.plugins,
        &outcome,
        &args.trust_plugin,
        true,
    )?;
    printer::print_plugins_list(&outcome, &context.config.plugins, &trust);
    Ok(())
}

fn plugins_info(args: &PluginInfoArgs) -> Result<()> {
    let context = trust_context(args.dirs.config.as_deref())?;
    let outcome = discover_plugins(&args.dirs.plugin_dir);

    let Some(plugin) = outcome.get(&args.name) else {
        for error in &outcome.errors {
            eprintln!("warning: {}", error.display());
        }
        bail!("plugin `{}` not found among discovered plugins", args.name);
    };

    let trust = apply_trust(
        &context,
        std::slice::from_ref(plugin),
        &outcome,
        &args.dirs.trust_plugin,
        true,
    )?;
    printer::print_plugins_info(plugin, &trust[&args.name]);
    Ok(())
}

/// Offline trust check: reports every discovered plugin's state and exits with
/// code 2 when any third-party plugin is not trusted. Read-only (no prompt, no
/// config write).
fn plugins_verify(args: &PluginDirArgs) -> Result<()> {
    let context = trust_context(args.config.as_deref())?;
    let outcome = discover_plugins(&args.plugin_dir);
    // A `--trust-plugin` name only answers the question "would this be trusted";
    // verify never persists.
    let trust = apply_trust(
        &context,
        &outcome.plugins,
        &outcome,
        &args.trust_plugin,
        false,
    )?;

    printer::print_plugins_verify(&outcome, &trust);

    let untrusted: Vec<&str> = trust
        .values()
        .filter(|evaluation| !evaluation.trusted)
        .map(|evaluation| evaluation.name.as_str())
        .collect();
    if !untrusted.is_empty() {
        bail!(
            "{} plugin(s) are not trusted: {}",
            untrusted.len(),
            untrusted.join(", ")
        );
    }
    Ok(())
}

/// Config plus the path it was (or would be) loaded from.
struct TrustContext {
    config: config::Config,
    /// Where newly confirmed hashes are written; `None` when no config home is
    /// known (persistence is then skipped with a warning).
    config_path: Option<PathBuf>,
}

fn trust_context(config: Option<&Path>) -> Result<TrustContext> {
    let loaded = load_config(config)?;
    let config_path = config
        .map(Path::to_path_buf)
        .or_else(config::Config::default_path);
    Ok(TrustContext {
        config: loaded,
        config_path,
    })
}

/// Evaluates the trust of `plugins` and, when `persist` is set, writes newly
/// confirmed hashes back to the config.
///
/// `discovered` is the full discovery outcome, used only to warn about
/// `--trust-plugin` names that match nothing (important for `info`, which
/// evaluates a single plugin). Interactive prompting only happens on a
/// terminal; otherwise the only way to confirm is `--trust-plugin=NAME`. Either
/// way the answer never blocks a pipeline.
fn apply_trust(
    context: &TrustContext,
    plugins: &[DiscoveredPlugin],
    discovered: &DiscoveryOutcome,
    trust_plugin: &[String],
    persist: bool,
) -> Result<BTreeMap<String, PluginTrust>> {
    let mut prompt = CliPrompt::new(trust_plugin, io::stdin().is_terminal());
    let evaluations = trust::evaluate_all(plugins, &context.config.plugins.trust, &mut prompt);

    for name in trust_plugin {
        if discovered.get(name).is_none() {
            eprintln!("warning: --trust-plugin `{name}` matches no discovered plugin");
        }
    }

    if persist {
        for evaluation in evaluations.values() {
            if !evaluation.persist {
                continue;
            }
            match &context.config_path {
                Some(path) => trust::save_trust_hash(path, &evaluation.name, &evaluation.hash)
                    .with_context(|| {
                        format!("cannot persist trust for plugin `{}`", evaluation.name)
                    })?,
                None => eprintln!(
                    "warning: cannot persist trust for `{}`: no config directory found",
                    evaluation.name
                ),
            }
        }
    }
    Ok(evaluations)
}

/// The trust prompt used by the CLI.
///
/// `--trust-plugin=NAME` confirms the named plugin without asking. Otherwise an
/// interactive terminal renders the capability request and asks `[y/N]`; a pipe
/// or redirected stdin declines (the [`trust::DenyPrompt`] behavior), so
/// non-interactive runs never hang.
struct CliPrompt {
    allow: std::collections::BTreeSet<String>,
    interactive: bool,
}

impl CliPrompt {
    fn new(names: &[String], interactive: bool) -> Self {
        Self {
            allow: names.iter().cloned().collect(),
            interactive,
        }
    }
}

impl TrustPrompt for CliPrompt {
    fn confirm(&mut self, request: &TrustRequest<'_>) -> bool {
        if self.allow.contains(request.name()) {
            return true;
        }
        if !self.interactive {
            return false;
        }

        println!("{}", request.render());
        print!("Trust this plugin? [y/N] ");
        let _ = io::stdout().flush();
        let mut answer = String::new();
        if io::stdin().read_line(&mut answer).is_err() {
            return false;
        }
        matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
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
