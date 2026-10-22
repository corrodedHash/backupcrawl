//! Bundled chezmoi detector plugin.
//!
//! Answers the three questions of §3 of `docs/rust-rewrite-plan.md` for
//! [chezmoi](https://www.chezmoi.io) dotfiles. Chezmoi is the one bundled
//! detector that keeps `exec` (§7.4): a faithful native implementation would
//! have to reimplement source-state filename encoding and template evaluation,
//! while `chezmoi managed` and `chezmoi status` already answer the three
//! questions.
//!
//! * **Managed?** `chezmoi managed --format json --path-style absolute` lists
//!   the managed destination paths. `include-types` narrows the set
//!   (`files`, `dirs`, `symlinks`; default all).
//! * **Current?** `chezmoi status --path-style absolute` prints a two-column
//!   status per changed path. The target column changed (` M`, `MM`, ...) is
//!   `dirty`: the destination holds content the dotfiles source does not
//!   capture. Only the source column changed (`M `, ...) is `ahead`: the source
//!   state is not on disk yet. Both columns clean (the path is absent from
//!   `status`) is `clean`.
//! * **Propagated?** Chezmoi has no remote. Whether the source directory is
//!   pushed is the **git plugin's** job (the source tree is usually a git
//!   repository). If both detectors run, the file under `$HOME` is claimed
//!   `clean` by chezmoi and the source repository is reported in the git
//!   panel — "in sync" here means "chezmoi has captured it", not "archived".
//!   See `plugins/chezmoi/README.md`.
//!
//! Both commands run **once** in `init` and their result is cached through the
//! host cache, keyed by the source/destination directory mtimes, the
//! include-types setting and the program; a cache hit skips both execs. A
//! missing/failed `chezmoi managed` is an `init` error, so the host disables
//! the plugin with a visible note and nothing is claimed. A failed or
//! unparsable `chezmoi status` is not fatal: every managed path becomes
//! `unknown` with a `reason` instead of a silent `clean`. `run_` scripts have
//! no target file and are ignored.
//!
//! # Config
//!
//! `source-dir`, `dest-dir` (defaults come from `~/.config/chezmoi/chezmoi.toml`
//! or `~/.local/share/chezmoi` / `$HOME`), `include-types = "files,dirs,symlinks"`
//! (default all), `chezmoi-path` (test hook, default `chezmoi`) and
//! `timeout-ms` (forwarded to the host `exec` import).

mod config;
mod dirs;
mod snapshot;

use std::path::{Path, PathBuf};

use backupcrawl_plugin_sdk::{
    Claim, Detector, DetectorError, Host, LinkInfo, LogLevel, ScanInfo, bind_detector,
};

use config::ChezmoiConfig;
use snapshot::{CACHE_FORMAT, Snapshot, Verdict};

/// The chezmoi detector's mutable state (one instance per wasm store).
#[derive(Debug, Default)]
pub struct Chezmoi {
    /// Validated `[plugins.chezmoi]` config.
    config: ChezmoiConfig,
    /// Resolved source state directory, used for the `source` attribute.
    source: PathBuf,
    /// Managed/status snapshot for this scan.
    snapshot: Snapshot,
}

impl Chezmoi {
    fn claim(&self, path: &Path) -> Option<Claim> {
        let verdict = self.snapshot.verdict(path)?;
        let claim = match verdict {
            Verdict::Clean => Claim::clean(),
            Verdict::Dirty => Claim::dirty().attr("state", "target-modified"),
            Verdict::Ahead => Claim::ahead().attr("state", "source-modified"),
            Verdict::Unknown(reason) => Claim::unknown().attr("reason", reason),
        };
        Some(claim.attr("source", self.source.display().to_string()))
    }
}

impl Detector for Chezmoi {
    fn init(&mut self, info: &ScanInfo, host: &mut dyn Host) -> Result<(), DetectorError> {
        self.config = ChezmoiConfig::from_config(&info.config)?;
        let dirs = dirs::resolve(&self.config, &info.root, &info.user)?;
        self.source = dirs.source.clone();

        let key = snapshot::cache_key(&dirs, &self.config, host);
        if let Some(bytes) = host.cache_get(&key) {
            match serde_json::from_slice::<Snapshot>(&bytes) {
                Ok(snapshot) if snapshot.format == CACHE_FORMAT => {
                    host.log(
                        LogLevel::Debug,
                        &format!(
                            "chezmoi: snapshot cache hit ({} managed paths)",
                            snapshot.len()
                        ),
                    );
                    snapshot::log_summary(&snapshot, host);
                    self.snapshot = snapshot;
                    return Ok(());
                }
                Ok(snapshot) => host.log(
                    LogLevel::Warn,
                    &format!(
                        "chezmoi: ignoring cached snapshot in unknown format {}; rebuilding",
                        snapshot.format
                    ),
                ),
                Err(err) => host.log(
                    LogLevel::Warn,
                    &format!("chezmoi: ignoring corrupt snapshot cache ({err}); rebuilding"),
                ),
            }
        }

        let snapshot = snapshot::load(&self.config, &dirs, host)?;
        if snapshot.status_error.is_none() {
            match serde_json::to_vec(&snapshot) {
                Ok(bytes) => {
                    if let Err(err) = host.cache_set(&key, &bytes) {
                        host.log(
                            LogLevel::Debug,
                            &format!("chezmoi: cannot write snapshot cache: {err}"),
                        );
                    }
                }
                Err(err) => host.log(
                    LogLevel::Warn,
                    &format!("chezmoi: cannot serialize snapshot cache: {err}"),
                ),
            }
        } else {
            // A transient template/config failure must not be pinned in the
            // cache, otherwise a fixed setup would keep reporting `unknown`.
            host.log(
                LogLevel::Debug,
                "chezmoi: not caching a snapshot with a failed status",
            );
        }

        host.log(
            LogLevel::Debug,
            &format!(
                "chezmoi: source={} dest={} program={} include={}",
                dirs.source.display(),
                dirs.dest.display(),
                self.config.program,
                self.config.include_types.cache_key()
            ),
        );
        snapshot::log_summary(&snapshot, host);
        self.snapshot = snapshot;
        Ok(())
    }

    fn check_dir(
        &mut self,
        path: &Path,
        _host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        Ok(self.claim(path))
    }

    fn check_file(
        &mut self,
        path: &Path,
        _host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        Ok(self.claim(path))
    }

    fn check_symlink(
        &mut self,
        link: &LinkInfo,
        _host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        Ok(self.claim(&link.path))
    }
}

// Emits the component export glue for the canonical `plugin` world.
bind_detector!(Chezmoi);

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use backupcrawl_plugin_sdk::{
        Attr, Config, ExecRequest, ExecResult, FileInfo, MockHost, Status,
    };

    use super::*;
    use crate::dirs::Dirs;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn scan_info(attrs: &[(&str, &str)]) -> ScanInfo {
        let mut config = vec![
            ("source-dir".to_owned(), "/src".to_owned()),
            ("dest-dir".to_owned(), "/dest".to_owned()),
        ];
        config.extend(
            attrs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
        );
        ScanInfo {
            root: PathBuf::from("/"),
            user: "tester".to_owned(),
            follow_symlinks: false,
            config: Config::new(
                config
                    .into_iter()
                    .map(|(key, value)| Attr::new(key, value))
                    .collect(),
            ),
        }
    }

    fn ok(stdout: &[u8]) -> ExecResult {
        ExecResult {
            code: 0,
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
            timed_out: false,
        }
    }

    fn failed(code: i32, stderr: &[u8]) -> ExecResult {
        ExecResult {
            code,
            stdout: Vec::new(),
            stderr: stderr.to_vec(),
            timed_out: false,
        }
    }

    fn init(config: &[(&str, &str)], host: &mut MockHost) -> Result<Chezmoi, DetectorError> {
        let mut detector = Chezmoi::default();
        detector.init(&scan_info(config), host)?;
        Ok(detector)
    }

    fn default_managed() -> &'static [u8] {
        br#"["/dest/.bashrc","/dest/.vimrc","/dest/.config","/dest/.zshrc","/dest/.profile"]"#
    }

    fn default_status() -> &'static [u8] {
        b" M /dest/.bashrc\nM  /dest/.vimrc\nMM /dest/.zshrc\n D /dest/.profile\n"
    }

    fn claim_status(detector: &mut Chezmoi, path: &str) -> Option<Status> {
        let mut host = MockHost::new();
        detector
            .check_file(Path::new(path), &mut host)
            .unwrap()
            .map(|claim| claim.status)
    }

    fn attr<'a>(claim: &'a Claim, key: &str) -> Option<&'a str> {
        claim
            .attrs
            .iter()
            .find(|attr| attr.key == key)
            .map(|attr| attr.value.as_str())
    }

    // -----------------------------------------------------------------------
    // Status mapping
    // -----------------------------------------------------------------------

    #[test]
    fn managed_paths_map_to_clean_dirty_and_ahead() {
        let mut host = MockHost::new()
            .with_exec_result(ok(default_managed()))
            .with_exec_result(ok(default_status()));
        let mut detector = init(&[], &mut host).unwrap();

        assert_eq!(
            claim_status(&mut detector, "/dest/.config"),
            Some(Status::Clean)
        );
        assert_eq!(
            claim_status(&mut detector, "/dest/.bashrc"),
            Some(Status::Dirty)
        );
        assert_eq!(
            claim_status(&mut detector, "/dest/.zshrc"),
            Some(Status::Dirty)
        );
        assert_eq!(
            claim_status(&mut detector, "/dest/.profile"),
            Some(Status::Dirty)
        );
        assert_eq!(
            claim_status(&mut detector, "/dest/.vimrc"),
            Some(Status::Ahead)
        );
        assert_eq!(claim_status(&mut detector, "/dest/other"), None);
    }

    #[test]
    fn claims_carry_the_source_attribute_and_a_state() {
        let mut host = MockHost::new()
            .with_exec_result(ok(default_managed()))
            .with_exec_result(ok(default_status()));
        let mut detector = init(&[], &mut host).unwrap();

        let dirty = detector
            .check_file(Path::new("/dest/.bashrc"), &mut MockHost::new())
            .unwrap()
            .unwrap();
        assert_eq!(attr(&dirty, "state"), Some("target-modified"));
        assert_eq!(attr(&dirty, "source"), Some("/src"));

        let ahead = detector
            .check_file(Path::new("/dest/.vimrc"), &mut MockHost::new())
            .unwrap()
            .unwrap();
        assert_eq!(attr(&ahead, "state"), Some("source-modified"));
    }

    #[test]
    fn managed_symlinks_are_claimed() {
        let mut host = MockHost::new()
            .with_exec_result(ok(br#"["/dest/.bashrc","/dest/.config/link"]"#))
            .with_exec_result(ok(b" M /dest/.config/link\n"));
        let mut detector = init(&[], &mut host).unwrap();

        let link = LinkInfo {
            path: PathBuf::from("/dest/.config/link"),
            target: PathBuf::from("/dest/.config/real"),
            dangling: false,
        };
        let claim = detector
            .check_symlink(&link, &mut MockHost::new())
            .unwrap()
            .expect("managed symlink claim");
        assert_eq!(claim.status, Status::Dirty);
        assert_eq!(attr(&claim, "source"), Some("/src"));

        // Unmanaged links are not claimed.
        let unmanaged = LinkInfo {
            path: PathBuf::from("/dest/.config/other"),
            target: PathBuf::from("/dest/.config/real"),
            dangling: false,
        };
        assert_eq!(
            detector
                .check_symlink(&unmanaged, &mut MockHost::new())
                .unwrap(),
            None
        );
    }

    #[test]
    fn unparsable_status_is_unknown_with_a_reason() {
        let mut host = MockHost::new()
            .with_exec_result(ok(default_managed()))
            .with_exec_result(ok(b"this is not chezmoi status\n"));
        let mut detector = init(&[], &mut host).unwrap();

        let claim = detector
            .check_file(Path::new("/dest/.bashrc"), &mut MockHost::new())
            .unwrap()
            .unwrap();
        assert_eq!(claim.status, Status::Unknown);
        assert!(
            attr(&claim, "reason").unwrap().contains("unparsable"),
            "{claim:?}"
        );
    }

    #[test]
    fn status_exec_failure_is_unknown_not_clean() {
        let mut host = MockHost::new()
            .with_exec_result(ok(default_managed()))
            .with_exec_result(failed(1, b"template: boom\n"));
        let mut detector = init(&[], &mut host).unwrap();

        let claim = detector
            .check_file(Path::new("/dest/.bashrc"), &mut MockHost::new())
            .unwrap()
            .unwrap();
        assert_eq!(claim.status, Status::Unknown);
        let reason = attr(&claim, "reason").unwrap();
        assert!(reason.contains("chezmoi status failed"), "{reason}");
        assert!(reason.contains("template: boom"), "{reason}");
    }

    #[test]
    fn status_timeout_is_unknown() {
        let mut timed_out = ok(b"");
        timed_out.timed_out = true;
        let mut host = MockHost::new()
            .with_exec_result(ok(default_managed()))
            .with_exec_result(timed_out);
        let mut detector = init(&[], &mut host).unwrap();

        let claim = detector
            .check_file(Path::new("/dest/.bashrc"), &mut MockHost::new())
            .unwrap()
            .unwrap();
        assert_eq!(claim.status, Status::Unknown);
        assert!(attr(&claim, "reason").unwrap().contains("timed out"));
    }

    // -----------------------------------------------------------------------
    // Managed parsing, run scripts and include-types
    // -----------------------------------------------------------------------

    #[test]
    fn run_scripts_have_no_claim() {
        let mut host = MockHost::new()
            .with_exec_result(ok(br#"["/src/run_once_setup.sh","/dest/.bashrc"]"#))
            .with_exec_result(ok(b""));
        let mut detector = init(&[], &mut host).unwrap();

        assert_eq!(claim_status(&mut detector, "/src/run_once_setup.sh"), None);
        assert_eq!(
            claim_status(&mut detector, "/dest/.bashrc"),
            Some(Status::Clean)
        );
    }

    #[test]
    fn include_types_are_forwarded_to_managed() {
        let mut host = MockHost::new()
            .with_exec_result(ok(default_managed()))
            .with_exec_result(ok(b""));
        let _detector = init(&[("include-types", "files,symlinks")], &mut host).unwrap();

        let requests = host.exec_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].program, "chezmoi");
        assert!(
            requests[0]
                .args
                .iter()
                .any(|arg| arg == "--include=files,symlinks"),
            "{:?}",
            requests[0].args
        );
        assert!(
            !requests[1]
                .args
                .iter()
                .any(|arg| arg.starts_with("--include")),
            "{:?}",
            requests[1].args
        );
    }

    #[test]
    fn invalid_include_types_fail_before_any_exec() {
        let mut host = MockHost::new();
        let err = init(&[("include-types", "scripts")], &mut host).unwrap_err();
        assert!(err.message().contains("include-types"), "{err}");
        assert!(host.exec_requests().is_empty());
    }

    #[test]
    fn program_and_timeout_are_forwarded() {
        let mut host = MockHost::new()
            .with_exec_result(ok(default_managed()))
            .with_exec_result(ok(b""));
        let _detector = init(
            &[("chezmoi-path", "/opt/chezmoi"), ("timeout-ms", "2500")],
            &mut host,
        )
        .unwrap();

        let requests = host.exec_requests();
        assert_eq!(requests[0].program, "/opt/chezmoi");
        assert_eq!(requests[0].timeout_ms, Some(2500));
        assert_eq!(requests[1].program, "/opt/chezmoi");
    }

    #[test]
    fn explicit_dirs_are_forwarded_as_global_flags() {
        let mut host = MockHost::new()
            .with_exec_result(ok(default_managed()))
            .with_exec_result(ok(b""));
        let _detector = init(&[], &mut host).unwrap();

        let args = &host.exec_requests()[0].args;
        assert_eq!(
            args,
            &[
                "managed",
                "--format",
                "json",
                "--path-style",
                "absolute",
                "--source",
                "/src",
                "--destination",
                "/dest"
            ]
        );
    }

    #[test]
    fn managed_output_errors_are_fatal() {
        let mut host = MockHost::new().with_exec_result(ok(b"not json"));
        let err = init(&[], &mut host).unwrap_err();
        assert!(err.message().contains("managed output"), "{err}");
        assert!(err.message().contains("invalid JSON"), "{err}");
    }

    #[test]
    fn managed_failure_disables_the_plugin() {
        let mut host = MockHost::new().with_exec_result(failed(1, b"config: boom\n"));
        let err = init(&[], &mut host).unwrap_err();
        assert!(err.message().contains("chezmoi managed failed"), "{err}");
        assert!(err.message().contains("config: boom"), "{err}");
    }

    #[test]
    fn missing_binary_is_an_init_error() {
        let mut host = MockHost::new().with_exec_error("exec: cannot find program `chezmoi`");
        let err = init(&[], &mut host).unwrap_err();
        assert!(err.message().contains("cannot run `chezmoi`"), "{err}");
        assert!(err.message().contains("cannot find program"), "{err}");
    }

    #[test]
    fn managed_timeout_is_an_init_error() {
        let mut timed_out = ok(b"");
        timed_out.timed_out = true;
        let mut host = MockHost::new().with_exec_result(timed_out);
        let err = init(&[("timeout-ms", "100")], &mut host).unwrap_err();
        assert!(err.message().contains("timed out after 100 ms"), "{err}");
    }

    // -----------------------------------------------------------------------
    // Cache
    // -----------------------------------------------------------------------

    #[test]
    fn cache_hit_skips_both_execs() {
        let mut first_host = MockHost::new()
            .with_exec_result(ok(default_managed()))
            .with_exec_result(ok(default_status()));
        let mut first = Chezmoi::default();
        first.init(&scan_info(&[]), &mut first_host).unwrap();

        let (key, value) = first_host
            .cache_sets()
            .first()
            .expect("snapshot cached")
            .clone();
        assert!(key.starts_with("chezmoi-snapshot-v1:"), "{key}");

        let mut second_host = MockHost::new().with_cache(&key, value);
        let mut second = Chezmoi::default();
        second.init(&scan_info(&[]), &mut second_host).unwrap();

        assert!(
            second_host.exec_requests().is_empty(),
            "a cache hit must not exec: {:?}",
            second_host.exec_requests()
        );
        assert_eq!(
            claim_status(&mut second, "/dest/.bashrc"),
            Some(Status::Dirty)
        );
        assert!(
            second_host
                .logs()
                .iter()
                .any(|(_, message)| message.contains("cache hit")),
            "{:?}",
            second_host.logs()
        );
    }

    #[test]
    fn corrupt_cache_is_rebuilt() {
        let mut first_host = MockHost::new()
            .with_exec_result(ok(default_managed()))
            .with_exec_result(ok(b""));
        let mut first = Chezmoi::default();
        first.init(&scan_info(&[]), &mut first_host).unwrap();
        let (key, _) = first_host.cache_sets().first().unwrap().clone();

        let mut second_host = MockHost::new()
            .with_cache(&key, b"not json".to_vec())
            .with_exec_result(ok(default_managed()))
            .with_exec_result(ok(b""));
        let mut second = Chezmoi::default();
        second.init(&scan_info(&[]), &mut second_host).unwrap();

        assert_eq!(second_host.exec_requests().len(), 2);
        assert_eq!(second_host.cache_sets().len(), 1);
        assert!(
            second_host
                .logs()
                .iter()
                .any(|(_, message)| message.contains("corrupt snapshot cache")),
            "{:?}",
            second_host.logs()
        );
    }

    #[test]
    fn cache_key_tracks_source_and_dest_mtimes_and_include_types() {
        let dirs = Dirs {
            source: PathBuf::from("/src"),
            dest: PathBuf::from("/dest"),
            home: None,
        };
        let info = |mtime_ms: u64| FileInfo {
            mtime_ms,
            ..MockHost::file_info(0)
        };
        let mut host = MockHost::new()
            .with_stat("/src", info(10))
            .with_stat("/dest", info(20));
        let config = ChezmoiConfig::default();
        let key = snapshot::cache_key(&dirs, &config, &mut host);
        assert!(key.contains(":10:20:"), "{key}");

        let mut host = MockHost::new()
            .with_stat("/src", info(11))
            .with_stat("/dest", info(20));
        let other = snapshot::cache_key(&dirs, &config, &mut host);
        assert_ne!(key, other);

        let mut host = MockHost::new()
            .with_stat("/src", info(10))
            .with_stat("/dest", info(20));
        let filtered = ChezmoiConfig {
            include_types: crate::config::IncludeTypes::parse("files").unwrap(),
            ..ChezmoiConfig::default()
        };
        let other = snapshot::cache_key(&dirs, &filtered, &mut host);
        assert_ne!(key, other);
    }

    #[test]
    fn failed_status_is_not_cached() {
        let mut host = MockHost::new()
            .with_exec_result(ok(default_managed()))
            .with_exec_result(failed(1, b"template: boom\n"));
        let mut detector = Chezmoi::default();
        detector.init(&scan_info(&[]), &mut host).unwrap();
        assert!(host.cache_sets().is_empty());
    }

    // -----------------------------------------------------------------------
    // Misc
    // -----------------------------------------------------------------------

    #[test]
    fn empty_managed_list_yields_no_claims() {
        let mut host = MockHost::new()
            .with_exec_result(ok(b"[]"))
            .with_exec_result(ok(b""));
        let mut detector = init(&[], &mut host).unwrap();
        assert_eq!(claim_status(&mut detector, "/dest/.bashrc"), None);
    }

    #[test]
    fn cache_key_starts_with_the_format_version() {
        let dirs = Dirs {
            source: PathBuf::from("/src"),
            dest: PathBuf::from("/dest"),
            home: None,
        };
        let mut host = MockHost::new();
        let key = snapshot::cache_key(&dirs, &ChezmoiConfig::default(), &mut host);
        assert!(key.contains("chezmoi-snapshot-v1"), "{key}");
    }

    #[test]
    fn exec_requests_carry_home() {
        let mut host = MockHost::new()
            .with_exec_result(ok(default_managed()))
            .with_exec_result(ok(b""));
        // `dirs::resolve` detects `$HOME` from the environment in native
        // builds; when it is available it must be passed to the child (the
        // host otherwise runs children with an empty environment).
        let _detector = init(&[], &mut host).unwrap();
        let requests: Vec<&ExecRequest> = host.exec_requests().iter().collect();
        if std::env::var_os("HOME").is_some() {
            assert!(
                requests[0].env.iter().any(|(key, _)| key == "HOME"),
                "{:?}",
                requests[0].env
            );
        }
    }

    #[test]
    fn status_map_is_kept_for_duplicate_lines() {
        // Sanity: the parser keeps the last code for a repeated path.
        let (statuses, error) = snapshot::parse_status(b"M  /dest/a\n M /dest/a\n");
        assert_eq!(error, None);
        assert_eq!(statuses["/dest/a"], " M");
        assert_eq!(
            snapshot::status_from_code(&statuses["/dest/a"]),
            Status::Dirty
        );
    }

    #[test]
    fn snapshot_status_lookup_is_exact() {
        let snapshot = Snapshot {
            managed: ["/dest/.bashrc".to_owned()].into(),
            statuses: BTreeMap::from([("/dest/.bashrc".to_owned(), "M ".to_owned())]),
            ..Snapshot::empty()
        };
        assert_eq!(
            snapshot.verdict(Path::new("/dest/.bashrc")),
            Some(Verdict::Ahead)
        );
    }
}
