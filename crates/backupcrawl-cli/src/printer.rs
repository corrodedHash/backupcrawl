//! Console and JSON output.
//!
//! Console rendering is hand-rolled box drawing on top of the `console` crate
//! (styling is disabled automatically when stdout is not a terminal or
//! `NO_COLOR` is set). The JSON shape is versioned via `"schema": 1`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use console::{measure_text_width, style};
use serde::Serialize;

use backupcrawl_core::detector::DetectorReport;
use backupcrawl_core::{CrawlResult, PluginReport, Status, SymlinkEntry};
use backupcrawl_host::discovery::{DiscoveredPlugin, DiscoveryOutcome, PluginSource};
use backupcrawl_host::fs::FsReadScope;
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
    let symlinks = symlink_lines(result, show_all);
    if !symlinks.is_empty() {
        panels.push(render_panel("Symlinks", &symlinks));
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

/// Renders the symlink panel lines.
///
/// Unmanaged links are always shown (`[Unmanaged] path -> target`, with a
/// `(dangling)` marker); links that are clean because their target lives in a
/// clean-managed tree (`via`) appear only with `--all`.
///
/// Links sharing a directory that holds nothing but links are collapsed into
/// one row (`[Unmanaged] dir (N links, D dangling)`), because link farms (Steam
/// prefixes, version managers) otherwise drown every other finding. A group
/// with a single link, and a directory that also holds unmanaged non-link
/// paths, stay per-link rows. With `--all` the unmanaged groups are expanded
/// again; clean (`via`) groups stay summarized so they do not flood the panel
/// once detectors learn to claim their targets.
fn symlink_lines(result: &CrawlResult, show_all: bool) -> Vec<String> {
    let mut lines = Vec::new();
    for group in group_symlinks(result, show_all) {
        let expanded = !group.link_only || group.links <= 1 || (show_all && group.via == 0);
        if expanded {
            lines.extend(group.members.iter().map(link_line));
        } else {
            lines.push(group_line(&group));
        }
    }
    lines
}

fn link_line(link: &SymlinkEntry) -> String {
    let label = if link.via.is_some() {
        "Clean"
    } else {
        "Unmanaged"
    };
    let dangling = if link.dangling { " (dangling)" } else { "" };
    format!(
        "[{label}] {} -> {}{dangling}",
        link.path.display(),
        link.target.display()
    )
}

fn group_line(group: &SymlinkGroup) -> String {
    let label = if group.via > 0 { "Clean" } else { "Unmanaged" };
    let links = if group.links == 1 { "link" } else { "links" };
    format!(
        "[{label}] {} ({} {links}, {} dangling)",
        group.path.display(),
        thousands(group.links),
        thousands(group.dangling)
    )
}

/// One collapsed row of the symlink panel: the links that live in one
/// directory. Also the source of the JSON `symlink_groups` entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymlinkGroup {
    /// Directory the reported links were grouped under.
    pub path: PathBuf,
    /// Number of links represented by this row.
    pub links: usize,
    /// How many of those links have a target that does not exist.
    pub dangling: usize,
    /// How many of those links are clean because their target lives in a
    /// managed tree. Groups are category-homogeneous, so this is either `0`
    /// or equal to `links`.
    pub via: usize,
    /// Whether [`path`](Self::path) holds only symlinks and may therefore
    /// stand for all members. When false the members stay individual rows.
    pub link_only: bool,
    /// The grouped links, in crawl (path) order.
    members: Vec<SymlinkEntry>,
}

impl SymlinkGroup {
    fn new(path: PathBuf, link_only: bool) -> Self {
        Self {
            path,
            links: 0,
            dangling: 0,
            via: 0,
            link_only,
            members: Vec::new(),
        }
    }

    fn push(&mut self, link: &SymlinkEntry) {
        self.links += 1;
        if link.dangling {
            self.dangling += 1;
        }
        if link.via.is_some() {
            self.via += 1;
        }
        self.members.push(link.clone());
    }
}

/// Summarizes the reported links by directory, optionally including the
/// clean (`via`) links.
///
/// A link is grouped under its highest ancestor directory (bounded by the
/// crawl root) that contains **no unmanaged non-symlink path at or below it**.
/// "Unmanaged non-symlink" is exactly what lands in `loose_paths` (files no
/// detector claimed, collapsed bottom-up) or `denied_paths` (unreadable
/// entries), so a directory holding links next to unmanaged regular files
/// disqualifies itself and its links stay individual rows. Claimed paths do
/// not disqualify a directory — they are managed, so a link-only row next to
/// them is not misleading, and their detail lives in the plugin panels.
///
/// Caveat: `loose_paths` is already collapsed, so a loose path can be an
/// ancestor of a link directory while the actual unmanaged file sits in a
/// deeper branch. The file is still reported (by that ancestor row); only the
/// link group's directory may contain something besides links. Sealing that
/// hole would require the pre-collapse loose entries in [`CrawlResult`].
pub fn group_symlinks(result: &CrawlResult, show_all: bool) -> Vec<SymlinkGroup> {
    let dirty = dirty_dirs(result);
    let mut unmanaged: BTreeMap<PathBuf, SymlinkGroup> = BTreeMap::new();
    let mut clean: BTreeMap<PathBuf, SymlinkGroup> = BTreeMap::new();

    for link in &result.symlinks {
        if link.via.is_some() && !show_all {
            continue;
        }
        let path = group_path(&link.path, &result.root, &dirty);
        let link_only = !dirty.contains(&path);
        let groups = if link.via.is_some() {
            &mut clean
        } else {
            &mut unmanaged
        };
        groups
            .entry(path.clone())
            .or_insert_with(|| SymlinkGroup::new(path, link_only))
            .push(link);
    }

    let mut groups: Vec<SymlinkGroup> =
        unmanaged.into_values().chain(clean.into_values()).collect();
    groups.sort_by(|left, right| left.path.cmp(&right.path).then(left.via.cmp(&right.via)));
    groups
}

/// Every unmanaged non-symlink path plus all of its ancestors.
///
/// A directory is link-only exactly when it is absent from this set: the set
/// contains a directory if, and only if, a loose or denied path is at or below
/// that directory.
fn dirty_dirs(result: &CrawlResult) -> BTreeSet<PathBuf> {
    result
        .loose_paths
        .iter()
        .chain(result.denied_paths.iter())
        .flat_map(|path| path.ancestors())
        .map(Path::to_path_buf)
        .collect()
}

/// The directory a link is grouped under: its parent, raised through the
/// chain of link-only ancestors, stopping at the crawl root or below the
/// first directory that holds an unmanaged non-link path.
fn group_path(link: &Path, root: &Path, dirty: &BTreeSet<PathBuf>) -> PathBuf {
    let mut key = link
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.to_path_buf());
    while !dirty.contains(&key) {
        let Some(parent) = key.parent() else { break };
        if dirty.contains(parent) || !parent.starts_with(root) {
            break;
        }
        key = parent.to_path_buf();
    }
    key
}

/// Formats a count with thousands separators, so large link farms stay
/// readable (`26,320` instead of `26320`).
fn thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Prints the crawl result as versioned JSON.
///
/// The default output is compact: links are summarized per directory in
/// `symlink_groups`, and the exact per-link `symlinks` array is only emitted
/// with `--all`.
pub fn print_json(result: &CrawlResult, show_all: bool) -> Result<()> {
    let symlink_groups = group_symlinks(result, show_all)
        .into_iter()
        .map(|group| JsonSymlinkGroup {
            path: group.path.display().to_string(),
            links: group.links,
            dangling: group.dangling,
            via: group.via,
        })
        .collect();
    let symlinks = show_all.then(|| {
        result
            .symlinks
            .iter()
            .map(|link| JsonSymlink {
                path: link.path.display().to_string(),
                target: link.target.display().to_string(),
                dangling: link.dangling,
                via: link.via.as_ref().map(|via| via.display().to_string()),
            })
            .collect()
    });
    let output = JsonOutput {
        schema: SCHEMA_VERSION,
        root: &result.root,
        not_backed_up: display_paths(&result.loose_paths),
        permission_denied: display_paths(&result.denied_paths),
        symlink_groups,
        symlinks,
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
    // The real, resolved content scope (M2.0): the crawl root is added when a
    // detector is actually instantiated, so `info` shows the declared scope.
    let scope = FsReadScope::resolve(&manifest.capabilities.fs_read);
    lines.push(format!("  fs: read {}", scope.display()));
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
    /// Compact per-directory summary of the reported links, always present
    /// (empty when there are none).
    symlink_groups: Vec<JsonSymlinkGroup>,
    /// Exact per-link array, only with `--all` (the default report is compact).
    #[serde(skip_serializing_if = "Option::is_none")]
    symlinks: Option<Vec<JsonSymlink>>,
    plugins: &'a [PluginReport],
}

/// One entry of the JSON `symlink_groups` array.
#[derive(Serialize)]
struct JsonSymlinkGroup {
    /// Directory the links were grouped under.
    path: String,
    /// Number of links in the group.
    links: usize,
    /// Number of those links whose target does not exist.
    dangling: usize,
    /// Number of those links that are clean via a managed tree.
    via: usize,
}

/// One entry of the JSON `symlinks` array (`--all` only).
#[derive(Serialize)]
struct JsonSymlink {
    path: String,
    target: String,
    dangling: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    via: Option<String>,
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

    #[test]
    fn thousands_groups_digits() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(26_320), "26,320");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    fn link(root: &Path, relative: &str) -> SymlinkEntry {
        SymlinkEntry {
            path: root.join(relative),
            target: root.join("target"),
            dangling: false,
            via: None,
        }
    }

    fn crawl(root: &Path, loose: &[&str], links: Vec<SymlinkEntry>) -> CrawlResult {
        CrawlResult {
            root: root.to_path_buf(),
            loose_paths: loose.iter().map(|path| root.join(path)).collect(),
            denied_paths: Vec::new(),
            symlinks: links,
            plugins: Vec::new(),
        }
    }

    #[test]
    fn groups_climb_to_the_highest_link_only_directory() {
        let root = Path::new("/scan");
        let mut first = link(root, "a/b/c/link-1");
        first.dangling = true;
        let result = crawl(root, &["keep.txt"], vec![first, link(root, "a/b/link-2")]);

        let groups = group_symlinks(&result, false);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].path, root.join("a"));
        assert_eq!(groups[0].links, 2);
        assert_eq!(groups[0].dangling, 1);
        assert!(groups[0].link_only);
    }

    #[test]
    fn groups_stop_at_mixed_directories() {
        let root = Path::new("/scan");
        let result = crawl(
            root,
            &["mixed/regular.txt"],
            vec![link(root, "mixed/link-1"), link(root, "mixed/link-2")],
        );

        let groups = group_symlinks(&result, false);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].path, root.join("mixed"));
        assert_eq!(groups[0].links, 2);
        assert!(!groups[0].link_only);
    }

    #[test]
    fn clean_links_are_grouped_only_with_all() {
        let root = Path::new("/scan");
        let mut clean = link(root, "farm/link-1");
        clean.via = Some(root.join("target"));
        let result = crawl(
            root,
            &[],
            vec![clean.clone(), {
                let mut other = clean;
                other.path = root.join("farm/link-2");
                other
            }],
        );

        assert!(group_symlinks(&result, false).is_empty());
        let groups = group_symlinks(&result, true);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].links, 2);
        assert_eq!(groups[0].via, 2);
        assert!(groups[0].link_only);
    }
}
