//! Console and JSON output.
//!
//! Console rendering is hand-rolled box drawing on top of the `console` crate
//! (styling is disabled automatically when stdout is not a terminal or
//! `NO_COLOR` is set). The JSON shape is versioned via `"schema": 1`.

use std::path::PathBuf;

use anyhow::Result;
use console::{measure_text_width, style};
use serde::Serialize;

use backupcrawl_core::{CrawlResult, PluginReport, Status};

/// Version of the JSON output schema.
pub const SCHEMA_VERSION: u32 = 1;

/// Prints the crawl result as human-readable panels.
pub fn print_console(result: &CrawlResult, show_all: bool) {
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
        return;
    }
    println!("{}", panels.join("\n"));
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
