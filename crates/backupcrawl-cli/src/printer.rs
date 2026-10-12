//! Console and JSON output.
//!
//! Console rendering is hand-rolled box drawing on top of the `console` crate
//! (styling is disabled automatically when stdout is not a terminal or
//! `NO_COLOR` is set). The JSON shape is versioned via `"schema": 1`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use console::{measure_text_width, style};
use serde::Serialize;

use backupcrawl_core::detector::DetectorReport;
use backupcrawl_core::{CrawlResult, PluginReport, Status};
use backupcrawl_host::discovery::{DiscoveredPlugin, DiscoveryOutcome, PluginSource};
use backupcrawl_host::manifest::{Manifest, SUPPORTED_CONTRACT};
use backupcrawl_host::trust::{PluginTrust, TrustState};

use crate::config::PluginsConfig;

/// Version of the JSON output schema.
pub const SCHEMA_VERSION: u32 = 1;

/// Run-time status of one detector, rendered in the `plugins:` report line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRunStatus {
    /// Detector name.
    pub name: String,
    /// Final state for this run.
    pub state: PluginState,
}

/// The three states shown in the run report line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    /// Ran and reported no errors.
    Ok,
    /// Disabled (untrusted, load failure, init failure, circuit breaker).
    Disabled(String),
    /// Ran but recorded failed calls.
    Failed(u32),
}

impl PluginRunStatus {
    /// Derives the status of a detector that ran from its lifecycle report.
    pub fn ran(name: impl Into<String>, report: &DetectorReport) -> Self {
        let state = if let Some(reason) = &report.disabled {
            PluginState::Disabled(reason.clone())
        } else if report.errors > 0 {
            PluginState::Failed(report.errors)
        } else {
            PluginState::Ok
        };
        Self {
            name: name.into(),
            state,
        }
    }

    /// A detector that never ran (untrusted, load failure).
    pub fn disabled(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: PluginState::Disabled(reason.into()),
        }
    }
}

/// Renders the `plugins: git ok, ...` run report line.
pub fn run_line(run: &[PluginRunStatus]) -> String {
    if run.is_empty() {
        return "plugins: (none)".to_owned();
    }
    let parts = run.iter().map(|status| match &status.state {
        PluginState::Ok => format!("{} ok", status.name),
        PluginState::Disabled(reason) => format!("{} disabled ({reason})", status.name),
        PluginState::Failed(errors) => {
            let suffix = if *errors == 1 { "" } else { "s" };
            format!("{} failed ({errors} error{suffix})", status.name)
        }
    });
    format!("plugins: {}", parts.collect::<Vec<_>>().join(", "))
}

/// Prints the crawl result as human-readable panels plus the run report line.
pub fn print_console(result: &CrawlResult, show_all: bool, run: &[PluginRunStatus]) {
    let mut panels = Vec::new();

    if !result.loose_paths.is_empty() {
        panels.push(render_panel(
            "Not backed up",
            &display_paths(&result.loose_paths),
        ));
    }
    if !result.denied_paths.is_empty() {
        panels.push(render_panel(
            "Permission denied",
            &display_paths(&result.denied_paths),
        ));
    }
    for report in &result.plugins {
        let lines = plugin_lines(report, show_all);
        if !lines.is_empty() {
            panels.push(render_panel(&report.plugin, &lines));
        }
    }

    if panels.is_empty() {
        println!("Nothing to report: everything is backed up.");
    } else {
        println!("{}", panels.join("\n"));
    }
    println!();
    println!("{}", run_line(run));
}

/// Prints the crawl result as versioned JSON.
pub fn print_json(result: &CrawlResult) -> Result<()> {
    let output = JsonOutput {
        schema: SCHEMA_VERSION,
        root: &result.root,
        not_backed_up: display_paths(&result.loose_paths),
        permission_denied: display_paths(&result.denied_paths),
        plugins: &result.plugins,
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Prints discovered plugins as an aligned table inside the console panel style.
pub fn print_plugins_list(
    outcome: &DiscoveryOutcome,
    plugins: &PluginsConfig,
    trust: &BTreeMap<String, PluginTrust>,
) {
    let mut rows: Vec<PluginRow> = outcome
        .plugins
        .iter()
        .map(|plugin| PluginRow {
            name: plugin.name.clone(),
            version: plugin.manifest.version().to_owned(),
            source: plugin.source.label().to_owned(),
            enabled: is_enabled(plugins, &plugin.name),
            trust: trust_label(trust, &plugin.name),
        })
        .collect();
    rows.sort_by(|left, right| left.name.cmp(&right.name));

    let mut lines = Vec::new();
    lines.push(format!(
        "{:<16} {:<10} {:<8} {:<4} {:<8} {:<8}",
        "NAME", "VERSION", "SOURCE", "API", "ENABLED", "TRUST"
    ));
    for row in &rows {
        lines.push(format!(
            "{:<16} {:<10} {:<8} {:<4} {:<8} {:<8}",
            row.name,
            row.version,
            row.source,
            "ok",
            yes_no(row.enabled),
            row.trust,
        ));
    }
    if rows.is_empty() {
        lines.push("(no plugins discovered)".to_owned());
    }

    println!("{}", render_panel("Plugins", &lines));

    // Warnings and load errors go below the table so a long diagnostic does not
    // stretch the table's box.
    for plugin in &outcome.plugins {
        for warning in &plugin.warnings {
            println!("warning: {}: {warning}", plugin.name);
        }
    }
    for warning in &outcome.warnings {
        println!("warning: {warning}");
    }
    for error in &outcome.errors {
        println!("error: {}", error.display());
    }
}

/// Prints the trust state of every discovered plugin (offline check).
pub fn print_plugins_verify(outcome: &DiscoveryOutcome, trust: &BTreeMap<String, PluginTrust>) {
    let mut rows: Vec<PluginRow> = outcome
        .plugins
        .iter()
        .map(|plugin| PluginRow {
            name: plugin.name.clone(),
            version: plugin.manifest.version().to_owned(),
            source: plugin.source.label().to_owned(),
            enabled: true,
            trust: trust_label(trust, &plugin.name),
        })
        .collect();
    rows.sort_by(|left, right| left.name.cmp(&right.name));

    let mut lines = Vec::new();
    lines.push(format!(
        "{:<16} {:<10} {:<8} {:<8} HASH",
        "NAME", "VERSION", "SOURCE", "TRUST"
    ));
    for row in &rows {
        let hash = trust
            .get(&row.name)
            .map(|evaluation| evaluation.hash.as_str())
            .unwrap_or("-");
        lines.push(format!(
            "{:<16} {:<10} {:<8} {:<8} {hash}",
            row.name, row.version, row.source, row.trust
        ));
    }
    if rows.is_empty() {
        lines.push("(no plugins discovered)".to_owned());
    }

    println!("{}", render_panel("Plugin trust", &lines));

    for error in &outcome.errors {
        println!("error: {}", error.display());
    }
}

/// Prints the resolved manifest and capabilities of one plugin.
pub fn print_plugins_info(plugin: &DiscoveredPlugin, trust: &PluginTrust) {
    let manifest = &plugin.manifest;
    let mut lines = Vec::new();

    lines.push(format!("name:        {}", manifest.name()));
    lines.push(format!("version:     {}", manifest.version()));
    lines.push(format!(
        "api:         {} ({SUPPORTED_CONTRACT})",
        plugin.api()
    ));
    lines.push(format!(
        "description: {}",
        manifest.plugin.description.as_deref().unwrap_or("-")
    ));
    lines.push(format!(
        "license:     {}",
        manifest.plugin.license.as_deref().unwrap_or("-")
    ));
    lines.push(format!("source:      {}", source_display(plugin)));
    match effective_label(trust) {
        "bundled" => {
            lines.push("trust:       bundled (auto-trusted, covered by the binary)".to_owned())
        }
        "changed" => {
            let previous = match &trust.state {
                TrustState::Changed { previous } => previous.as_str(),
                _ => "-",
            };
            lines.push(format!(
                "trust:       changed (stored {previous} no longer matches)"
            ));
        }
        label => lines.push(format!("trust:       {label}")),
    }
    lines.push(format!("hash:        {}", trust.hash));
    if let Some(path) = &plugin.manifest_path {
        lines.push(format!("manifest:    {}", path.display()));
    }
    if let Some(path) = &plugin.wasm_path {
        lines.push(format!("wasm:        {}", path.display()));
    }

    lines.push(String::new());
    lines.push(format!("hooks:       {}", hooks_display(manifest)));
    lines.push(format!("priority:    {}", manifest.hooks.priority));

    lines.push(String::new());
    lines.push("capabilities:".to_owned());
    lines.push(format!(
        "  exec: {}",
        list_or_none(&manifest.capabilities.exec)
    ));
    // v1 always grants a read-only `/` preopen; `fs-read` is documented intent.
    lines.push("  fs: read / (v1 read-only preopen)".to_owned());
    lines.push(format!(
        "  fs-read: {} (documented intent)",
        list_or_none(&manifest.capabilities.fs_read)
    ));
    lines.push(format!(
        "  http: {}",
        list_or_none(&manifest.capabilities.http)
    ));
    lines.push(format!("  cache: {}", yes_no(manifest.capabilities.cache)));

    for warning in &plugin.warnings {
        lines.push(format!("warning: {warning}"));
    }

    println!("{}", render_panel(manifest.name(), &lines));
}

/// One row of `plugins list`.
struct PluginRow {
    name: String,
    version: String,
    source: String,
    enabled: bool,
    trust: &'static str,
}

fn trust_label(trust: &BTreeMap<String, PluginTrust>, name: &str) -> &'static str {
    trust.get(name).map(effective_label).unwrap_or("?")
}

/// The user-facing trust label: bundled stays bundled, a confirmed plugin is
/// `trusted` regardless of whether it was new or changed, and an unanswered one
/// keeps its relative state (`new`/`changed`).
fn effective_label(evaluation: &PluginTrust) -> &'static str {
    match evaluation.state {
        TrustState::Bundled => "bundled",
        _ if evaluation.trusted => "trusted",
        _ => evaluation.state.label(),
    }
}

fn is_enabled(plugins: &PluginsConfig, name: &str) -> bool {
    plugins.enabled.is_empty() || plugins.enabled.iter().any(|enabled| enabled == name)
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "(none)".to_owned()
    } else {
        items.join(", ")
    }
}

fn hooks_display(manifest: &Manifest) -> String {
    let mut hooks = Vec::new();
    if manifest.hooks.dir {
        hooks.push("dir");
    }
    if manifest.hooks.file {
        hooks.push("file");
    }
    if manifest.hooks.symlink {
        hooks.push("symlink");
    }
    if hooks.is_empty() {
        "(none)".to_owned()
    } else {
        hooks.join(", ")
    }
}

fn source_display(plugin: &DiscoveredPlugin) -> String {
    if plugin.source == PluginSource::Bundled {
        return "bundled (embedded)".to_owned();
    }
    match &plugin.source_dir {
        Some(dir) => format!("{} ({})", plugin.source.label(), dir.display()),
        None => plugin.source.label().to_owned(),
    }
}

#[derive(Serialize)]
struct JsonOutput<'a> {
    schema: u32,
    root: &'a PathBuf,
    not_backed_up: Vec<String>,
    permission_denied: Vec<String>,
    plugins: &'a [PluginReport],
}

fn plugin_lines(report: &PluginReport, show_all: bool) -> Vec<String> {
    report
        .entries
        .iter()
        .filter(|entry| show_all || entry.status != Status::Clean)
        .map(|entry| format!("[{}] {}", status_label(entry.status), entry.path.display()))
        .collect()
}

fn status_label(status: Status) -> &'static str {
    match status {
        Status::Clean => "Clean",
        Status::Dirty => "Dirty",
        Status::Ahead => "Unsynced",
        Status::Unknown => "Unknown",
        Status::Ignored => "Ignored",
    }
}

fn display_paths(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

/// Renders a simple box panel with a title in the top border.
fn render_panel(title: &str, lines: &[String]) -> String {
    let title_width = measure_text_width(title);
    let widest = lines
        .iter()
        .map(|line| measure_text_width(line))
        .max()
        .unwrap_or(0);
    let width = widest.max(title_width + 3);
    let dash_count = width - title_width - 1;
    let styled_title = if console::colors_enabled() {
        style(title).bold().to_string()
    } else {
        title.to_owned()
    };

    let mut out = String::new();
    out.push_str(&format!("╭─ {styled_title} {}╮\n", "─".repeat(dash_count)));
    for line in lines {
        let padding = width.saturating_sub(measure_text_width(line));
        out.push_str(&format!("│ {line}{} │\n", " ".repeat(padding)));
    }
    out.push_str(&format!("╰{}╯", "─".repeat(width + 2)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_have_consistent_width() {
        let panel = render_panel(
            "Title",
            &["short".to_owned(), "much longer line".to_owned()],
        );
        let widths: Vec<usize> = panel.lines().map(measure_text_width).collect();
        assert_eq!(widths.iter().min(), widths.iter().max());
    }
}
