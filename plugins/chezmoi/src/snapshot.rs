//! `chezmoi managed` / `chezmoi status` snapshot: execution, parsing, caching.
//!
//! `init` runs both commands exactly once and stores the result in a
//! [`Snapshot`]. The snapshot is serialized into the host cache, keyed by the
//! source/destination directory mtimes, the include-types setting and the
//! program, so a cache hit skips **both** `exec` calls (§7.4, §9). The Python
//! tool ran `chezmoi` per path; this detector never does.
//!
//! Managed paths are the target paths printed by `chezmoi managed --format
//! json --path-style absolute`. Status lines are `<source><target> <absolute
//! path>`; the columns are mapped as documented in `docs/rust-rewrite-plan.md`
//! §7.4 and the plugin README:
//!
//! * target column changed (` M`, `MM`, ...) → `dirty` (the destination holds
//!   content the dotfiles source does not capture);
//! * only the source column changed (`M `, ...) → `ahead` (the source state is
//!   not on disk yet);
//! * both columns clean (the path does not appear in `chezmoi status`) →
//!   `clean`.
//!
//! A failed/unparsable `chezmoi status` does not silently become `clean`: every
//! managed path is reported `unknown` with a `reason`. A failed `chezmoi
//! managed` (including a missing binary) is an `init` error, so the host
//! disables the plugin with a visible note and nothing is claimed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use backupcrawl_plugin_sdk::{ExecRequest, ExecResult, Host, LogLevel, Status};
use serde::{Deserialize, Serialize};

use crate::config::{ChezmoiConfig, IncludeTypes};
use crate::dirs::Dirs;

/// Version of the serialized snapshot format. Bump when [`Snapshot`] changes.
pub const CACHE_FORMAT: u32 = 1;

/// One `init`-time view of chezmoi's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Cache format version.
    pub format: u32,
    /// Managed target paths, normalized (no trailing slash).
    pub managed: BTreeSet<String>,
    /// Raw two-column status code per target path.
    #[serde(default)]
    pub statuses: BTreeMap<String, String>,
    /// Why the status could not be determined; makes every managed path
    /// `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_error: Option<String>,
}

/// The verdict for one path, before it becomes a `Claim`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Managed and in sync.
    Clean,
    /// Managed, destination changed.
    Dirty,
    /// Managed, source changed but not applied.
    Ahead,
    /// Managed but the state is unknown; carries the diagnostic.
    Unknown(String),
}

impl Snapshot {
    /// An empty snapshot (no managed paths).
    pub fn empty() -> Self {
        Self {
            format: CACHE_FORMAT,
            managed: BTreeSet::new(),
            statuses: BTreeMap::new(),
            status_error: None,
        }
    }

    /// Number of managed paths.
    pub fn len(&self) -> usize {
        self.managed.len()
    }

    /// The verdict for `path`, or `None` when chezmoi does not manage it.
    pub fn verdict(&self, path: &Path) -> Option<Verdict> {
        let key = normalize(path);
        if !self.managed.contains(&key) {
            return None;
        }
        if let Some(reason) = &self.status_error {
            return Some(Verdict::Unknown(reason.clone()));
        }
        Some(match self.statuses.get(&key) {
            Some(code) => match status_from_code(code) {
                Status::Dirty => Verdict::Dirty,
                Status::Ahead => Verdict::Ahead,
                Status::Unknown => {
                    Verdict::Unknown(format!("unrecognized chezmoi status code `{code}`"))
                }
                // Clean/ignored cannot come out of `status_from_code`.
                _ => Verdict::Clean,
            },
            None => Verdict::Clean,
        })
    }
}

impl Default for Snapshot {
    fn default() -> Self {
        Self::empty()
    }
}

/// Loads a snapshot, running `managed` and `status` once each.
pub fn load(
    config: &ChezmoiConfig,
    dirs: &Dirs,
    host: &mut dyn Host,
) -> Result<Snapshot, backupcrawl_plugin_sdk::DetectorError> {
    let managed = load_managed(config, dirs, host)?;
    let (statuses, status_error) = load_status(config, dirs, host);
    Ok(Snapshot {
        format: CACHE_FORMAT,
        managed,
        statuses,
        status_error,
    })
}

/// The cache key for a snapshot: format, program, include-types and the
/// source/destination directory mtimes.
pub fn cache_key(dirs: &Dirs, config: &ChezmoiConfig, host: &mut dyn Host) -> String {
    let source_mtime = host
        .stat(&dirs.source)
        .map(|info| info.mtime_ms)
        .unwrap_or(0);
    let dest_mtime = host.stat(&dirs.dest).map(|info| info.mtime_ms).unwrap_or(0);
    format!(
        "chezmoi-snapshot-v{CACHE_FORMAT}:{}:{}:{source_mtime}:{dest_mtime}:{}:{}",
        config.program,
        config.include_types.cache_key(),
        dirs.source.display(),
        dirs.dest.display()
    )
}

/// Arguments for `chezmoi managed`, with the optional include filter.
pub fn managed_args(include: IncludeTypes) -> Vec<String> {
    let mut args = vec![
        "managed".to_owned(),
        "--format".to_owned(),
        "json".to_owned(),
        "--path-style".to_owned(),
        "absolute".to_owned(),
    ];
    if let Some(types) = include.chezmoi_include() {
        args.push(format!("--include={types}"));
    }
    args
}

/// Arguments for `chezmoi status`.
pub fn status_args() -> Vec<String> {
    vec![
        "status".to_owned(),
        "--path-style".to_owned(),
        "absolute".to_owned(),
    ]
}

fn load_managed(
    config: &ChezmoiConfig,
    dirs: &Dirs,
    host: &mut dyn Host,
) -> Result<BTreeSet<String>, backupcrawl_plugin_sdk::DetectorError> {
    let args = add_explicit_dirs(config, dirs, managed_args(config.include_types));
    let result = exec(config, dirs, args, host).map_err(|err| {
        backupcrawl_plugin_sdk::DetectorError::new(format!(
            "chezmoi: cannot run `{}`: {}",
            config.program,
            err.message()
        ))
    })?;

    if result.timed_out {
        return Err(backupcrawl_plugin_sdk::DetectorError::new(format!(
            "chezmoi managed timed out after {} ms",
            config.timeout_ms.unwrap_or(0)
        )));
    }
    if result.code != 0 {
        return Err(backupcrawl_plugin_sdk::DetectorError::new(format!(
            "chezmoi managed failed (exit {}): {}",
            result.code,
            snippet(&result.stderr)
        )));
    }
    parse_managed(&result.stdout, &dirs.source).map_err(|reason| {
        backupcrawl_plugin_sdk::DetectorError::new(format!("chezmoi managed output: {reason}"))
    })
}

fn load_status(
    config: &ChezmoiConfig,
    dirs: &Dirs,
    host: &mut dyn Host,
) -> (BTreeMap<String, String>, Option<String>) {
    let args = add_explicit_dirs(config, dirs, status_args());
    let result = match exec(config, dirs, args, host) {
        Ok(result) => result,
        Err(err) => {
            return (
                BTreeMap::new(),
                Some(format!("chezmoi status failed: {}", err.message())),
            );
        }
    };
    if result.timed_out {
        return (BTreeMap::new(), Some("chezmoi status timed out".to_owned()));
    }
    if result.code != 0 {
        return (
            BTreeMap::new(),
            Some(format!(
                "chezmoi status failed (exit {}): {}",
                result.code,
                snippet(&result.stderr)
            )),
        );
    }
    parse_status(&result.stdout)
}

/// Adds `--source`/`--destination` only for explicit plugin overrides; the
/// default/config-file values are what chezmoi itself already uses.
fn add_explicit_dirs(config: &ChezmoiConfig, dirs: &Dirs, mut args: Vec<String>) -> Vec<String> {
    if config.source_dir.is_some() {
        args.push("--source".to_owned());
        args.push(dirs.source.display().to_string());
    }
    if config.dest_dir.is_some() {
        args.push("--destination".to_owned());
        args.push(dirs.dest.display().to_string());
    }
    args
}

fn exec(
    config: &ChezmoiConfig,
    dirs: &Dirs,
    args: Vec<String>,
    host: &mut dyn Host,
) -> Result<ExecResult, backupcrawl_plugin_sdk::HostError> {
    let mut request = ExecRequest::new(&config.program).args(args);
    if let Some(timeout_ms) = config.timeout_ms {
        request = request.timeout_ms(timeout_ms);
    }
    // The host runs children with an empty environment. che-zmoi needs `$HOME`
    // to find its config file, so pass the detected home through explicitly.
    if let Some(home) = &dirs.home {
        request = request.env("HOME", home.display().to_string());
    }
    host.exec(&request)
}

/// Parses the `chezmoi managed --format json` array.
pub fn parse_managed(bytes: &[u8], source: &Path) -> Result<BTreeSet<String>, String> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(BTreeSet::new());
    }
    let paths: Vec<String> =
        serde_json::from_slice(bytes).map_err(|err| format!("invalid JSON: {err}"))?;

    let mut managed = BTreeSet::new();
    for path in paths {
        if !path.starts_with('/') {
            return Err(format!("path `{path}` is not absolute"));
        }
        let key = normalize(Path::new(&path));
        // Scripts (`run_`, `run_once_`, `run_onchange_`, ...) have no target
        // file; they are stateful side effects and never claimed.
        if is_run_script(&key, source) {
            continue;
        }
        managed.insert(key);
    }
    Ok(managed)
}

/// Parses `chezmoi status --path-style absolute`.
///
/// Returns the status codes plus an optional fatal reason. A malformed line
/// makes the whole status unknown (never a wrong `clean`).
pub fn parse_status(bytes: &[u8]) -> (BTreeMap<String, String>, Option<String>) {
    let text = String::from_utf8_lossy(bytes);
    let mut statuses = BTreeMap::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let bytes = line.as_bytes();
        if bytes.len() < 4 || bytes[2] != b' ' {
            return (
                statuses,
                Some(format!("unparsable chezmoi status line: {line:?}")),
            );
        }
        let code = &line[..2];
        let path = &line[3..];
        if !path.starts_with('/') {
            return (
                statuses,
                Some(format!("chezmoi status path `{path}` is not absolute")),
            );
        }
        statuses.insert(normalize(Path::new(path)), code.to_owned());
    }
    (statuses, None)
}

/// Maps a two-column chezmoi status code to the detector status.
///
/// The target column wins: a destination edit is `dirty` even when the source
/// changed too (`MM`), because the user data under `$HOME` is not captured.
pub fn status_from_code(code: &str) -> Status {
    let mut chars = code.chars();
    let source = chars.next().unwrap_or(' ');
    let target = chars.next().unwrap_or(' ');
    let changed = |ch: char| !matches!(ch, ' ' | '.' | '_' | '\0');
    if changed(target) {
        Status::Dirty
    } else if changed(source) {
        Status::Ahead
    } else {
        Status::Clean
    }
}

/// Normalizes a target path for map lookups: strips a trailing slash.
pub fn normalize(path: &Path) -> String {
    let mut out = path.to_string_lossy().into_owned();
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

fn is_run_script(key: &str, source: &Path) -> bool {
    let path = Path::new(key);
    path.starts_with(source)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("run_"))
}

fn snippet(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "no stderr".to_owned();
    }
    let mut out: String = trimmed.chars().take(400).collect();
    if trimmed.chars().count() > 400 {
        out.push('…');
    }
    out
}

/// Logs a one-line summary of the snapshot (kept out of the hot path).
pub fn log_summary(snapshot: &Snapshot, host: &mut dyn Host) {
    host.log(
        LogLevel::Debug,
        &format!(
            "chezmoi: {} managed paths, {} status entries{}",
            snapshot.len(),
            snapshot.statuses.len(),
            snapshot
                .status_error
                .as_ref()
                .map(|reason| format!(", status unknown: {reason}"))
                .unwrap_or_default()
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_columns_map_to_clean_dirty_and_ahead() {
        assert_eq!(status_from_code("  "), Status::Clean);
        assert_eq!(status_from_code(". "), Status::Clean);
        assert_eq!(status_from_code(" M"), Status::Dirty);
        assert_eq!(status_from_code("MM"), Status::Dirty);
        assert_eq!(status_from_code(" D"), Status::Dirty);
        assert_eq!(status_from_code("M "), Status::Ahead);
        assert_eq!(status_from_code("A "), Status::Ahead);
        assert_eq!(status_from_code("R "), Status::Ahead);
    }

    #[test]
    fn managed_json_is_parsed_and_normalized() {
        let managed = parse_managed(
            br#"["/home/u/.bashrc","/home/u/.config/","/home/u/.config/app"]"#,
            Path::new("/home/u/.local/share/chezmoi"),
        )
        .unwrap();
        assert!(managed.contains("/home/u/.bashrc"));
        assert!(managed.contains("/home/u/.config"));
        assert!(managed.contains("/home/u/.config/app"));
        assert_eq!(managed.len(), 3);
    }

    #[test]
    fn empty_managed_output_is_an_empty_set() {
        assert!(parse_managed(b"", Path::new("/src")).unwrap().is_empty());
        assert!(
            parse_managed(b"[]\n", Path::new("/src"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn managed_json_errors_are_reported() {
        let err = parse_managed(b"not json", Path::new("/src")).unwrap_err();
        assert!(err.contains("invalid JSON"), "{err}");
        let err = parse_managed(br#"["relative/path"]"#, Path::new("/src")).unwrap_err();
        assert!(err.contains("not absolute"), "{err}");
    }

    #[test]
    fn run_scripts_under_the_source_are_ignored() {
        let managed = parse_managed(
            br#"["/src/run_once_setup.sh","/src/run_onchange_thing.sh","/home/u/.bashrc"]"#,
            Path::new("/src"),
        )
        .unwrap();
        assert!(!managed.iter().any(|path| path.contains("run_")));
        assert!(managed.contains("/home/u/.bashrc"));
    }

    #[test]
    fn status_lines_are_parsed_with_absolute_paths() {
        let (statuses, error) =
            parse_status(b" M /home/u/.bashrc\nM  /home/u/.vimrc\nMM /home/u/.zshrc\n");
        assert_eq!(error, None);
        assert_eq!(statuses.get("/home/u/.bashrc").unwrap(), " M");
        assert_eq!(statuses.get("/home/u/.vimrc").unwrap(), "M ");
        assert_eq!(statuses.get("/home/u/.zshrc").unwrap(), "MM");
    }

    #[test]
    fn malformed_status_output_is_an_error_not_clean() {
        let (_, error) = parse_status(b"garbage\n");
        assert!(error.as_ref().unwrap().contains("unparsable"), "{error:?}");

        let (_, error) = parse_status(b" M relative\n");
        assert!(
            error.as_ref().unwrap().contains("not absolute"),
            "{error:?}"
        );
    }

    #[test]
    fn verdict_requires_a_managed_path() {
        let snapshot = Snapshot {
            managed: ["/home/u/.bashrc".to_owned()].into(),
            statuses: [("/home/u/.bashrc".to_owned(), " M".to_owned())].into(),
            ..Snapshot::empty()
        };
        assert_eq!(
            snapshot.verdict(Path::new("/home/u/.bashrc")),
            Some(Verdict::Dirty)
        );
        assert_eq!(snapshot.verdict(Path::new("/home/u/other")), None);
    }

    #[test]
    fn status_error_makes_every_managed_path_unknown() {
        let snapshot = Snapshot {
            managed: ["/home/u/.bashrc".to_owned()].into(),
            status_error: Some("template failed".to_owned()),
            ..Snapshot::empty()
        };
        assert_eq!(
            snapshot.verdict(Path::new("/home/u/.bashrc")),
            Some(Verdict::Unknown("template failed".to_owned()))
        );
    }

    #[test]
    fn managed_and_status_arguments_match_the_plan() {
        assert_eq!(
            managed_args(IncludeTypes::default()),
            vec!["managed", "--format", "json", "--path-style", "absolute"]
        );
        assert_eq!(
            managed_args(IncludeTypes {
                files: true,
                dirs: false,
                symlinks: true
            })
            .last()
            .unwrap(),
            "--include=files,symlinks"
        );
        assert_eq!(status_args(), vec!["status", "--path-style", "absolute"]);
    }
}
