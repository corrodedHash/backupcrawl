//! REST API access and status mapping for syncthing folders (§7.3).
//!
//! One `GET /rest/db/status?folder=<id>` call per configured folder per scan,
//! through the host `http` import (plain HTTP, allowlisted origin,
//! `X-API-Key` header). The response is JSON; only the fields needed for the
//! verdict are read:
//!
//! * `state` (`idle`, `scanning`, `syncing`, `error`, ...),
//! * `needFiles` (remote data not here yet),
//! * `pullErrors` (failed transfers),
//! * `receiveOnlyChanged*` / `receiveOnlyTotalItems` (local divergence in a
//!   receive-only folder).
//!
//! The mapping never turns an API failure into a silent `clean`; that decision
//! belongs to `assume-marker-only`/`require-daemon` in the caller.

use backupcrawl_plugin_sdk::{Host, HttpRequest, Status};

use crate::config::ReceiveOnlyChanges;

/// Default REST endpoint when neither config nor `config.xml` names one.
pub const DEFAULT_ADDRESS: &str = "http://127.0.0.1:8384";

/// The relevant fields of `/rest/db/status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderState {
    /// Raw `state` value.
    pub state: String,
    /// `needFiles` (data needed from the cluster).
    pub need_files: u64,
    /// `pullErrors` (failed pulls).
    pub pull_errors: u64,
    /// Whether any receive-only divergence counter is non-zero.
    pub receive_only_changed: bool,
}

/// A folder's verdict after applying the configured mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// Resulting status.
    pub status: Status,
    /// The `state` attribute (raw state, or `marker-only`/`invalid`).
    pub state: String,
    /// Explanation for non-clean verdicts.
    pub reason: Option<String>,
    /// Whether receive-only divergence was observed.
    pub receive_only_changed: bool,
}

/// Builds the REST base URL from the override or the GUI address.
///
/// A missing scheme becomes `http://` (the host only speaks plain HTTP), and
/// the wildcard bind addresses syncthing often writes (`0.0.0.0`, `localhost`)
/// are canonicalized to loopback so the manifest's `127.0.0.1` allowlist
/// matches.
pub fn canonical_address(address: Option<&str>) -> String {
    let raw = address.map(str::trim).filter(|value| !value.is_empty());
    let raw = raw.unwrap_or(DEFAULT_ADDRESS);
    let with_scheme = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    };
    let with_scheme = with_scheme.trim_end_matches('/').to_owned();

    let Some((scheme, rest)) = with_scheme.split_once("://") else {
        return with_scheme;
    };
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, Some(path)),
        None => (rest, None),
    };
    let authority = canonical_authority(authority);
    match path {
        Some(path) if !path.is_empty() => format!("{scheme}://{authority}/{path}"),
        _ => format!("{scheme}://{authority}"),
    }
}

fn canonical_authority(authority: &str) -> String {
    if authority == "localhost" {
        return "127.0.0.1".to_owned();
    }
    if let Some(port) = authority.strip_prefix("localhost:") {
        return format!("127.0.0.1:{port}");
    }
    if authority == "0.0.0.0" {
        return "127.0.0.1".to_owned();
    }
    if let Some(port) = authority.strip_prefix("0.0.0.0:") {
        return format!("127.0.0.1:{port}");
    }
    if authority == "[::]" {
        return "[::1]".to_owned();
    }
    if let Some(port) = authority.strip_prefix("[::]:") {
        return format!("[::1]:{port}");
    }
    authority.to_owned()
}

/// Queries `/rest/db/status` for `folder_id`.
///
/// The error string is a human-readable reason suitable for the `reason`
/// attribute; the caller decides whether it becomes `unknown` or a marker-only
/// `clean`.
pub fn query(
    base: &str,
    api_key: &str,
    folder_id: &str,
    timeout_ms: Option<u64>,
    host: &mut dyn Host,
) -> Result<FolderState, String> {
    if timeout_ms.is_some() {
        // The WIT http request has no per-request timeout; the host enforces
        // its global limit. The key is parsed for config parity.
        host.log(
            backupcrawl_plugin_sdk::LogLevel::Debug,
            "syncthing: `timeout-ms` is reserved; the host http limit applies",
        );
    }
    let url = format!("{base}/rest/db/status?folder={}", percent_encode(folder_id));
    let request = HttpRequest::new("GET", url).header("X-API-Key", api_key);
    let response = host
        .http(&request)
        .map_err(|err| format!("syncthing daemon unreachable: {}", err.message()))?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "syncthing API returned HTTP {} for folder `{folder_id}`",
            response.status
        ));
    }
    parse_status(&response.body).map_err(|err| format!("invalid /rest/db/status response: {err}"))
}

fn parse_status(body: &[u8]) -> Result<FolderState, String> {
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|err| err.to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "expected a JSON object".to_owned())?;
    let state = object
        .get("state")
        .and_then(serde_json::Value::as_str)
        .filter(|state| !state.is_empty())
        .ok_or_else(|| "missing string field `state`".to_owned())?
        .to_owned();
    let need_files = number(object, "needFiles");
    let pull_errors = number(object, "pullErrors");
    let mut receive_only_changed = number(object, "receiveOnlyTotalItems") > 0;
    if !receive_only_changed {
        receive_only_changed = object.iter().any(|(key, value)| {
            key.starts_with("receiveOnlyChanged") && value.as_u64().is_some_and(|count| count > 0)
        });
    }
    Ok(FolderState {
        state,
        need_files,
        pull_errors,
        receive_only_changed,
    })
}

fn number(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> u64 {
    object
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

/// Maps a daemon response to a status.
///
/// * `state = error` or `pullErrors > 0` → `dirty`;
/// * receive-only divergence → the configured `receive-only-changes` verdict;
/// * `needFiles > 0` → `dirty` (remote data is missing locally);
/// * `state = idle` with nothing needed → `clean`;
/// * a busy state (`scanning`, `syncing`, ...) → `ahead` (changes not yet
///   propagated);
/// * an unexpected state → `unknown` (never a guessed `clean`).
pub fn map(state: &FolderState, receive_only: ReceiveOnlyChanges) -> Verdict {
    let mut verdict = Verdict {
        status: Status::Clean,
        state: state.state.clone(),
        reason: None,
        receive_only_changed: state.receive_only_changed,
    };
    if state.state == "error" {
        verdict.status = Status::Dirty;
        verdict.reason = Some("folder state is `error`".to_owned());
        return verdict;
    }
    if state.pull_errors > 0 {
        verdict.status = Status::Dirty;
        verdict.reason = Some(format!("{} pull error(s)", state.pull_errors));
        return verdict;
    }
    if state.receive_only_changed {
        match receive_only {
            ReceiveOnlyChanges::Dirty => {
                verdict.status = Status::Dirty;
                verdict.reason = Some("receive-only changes are not applied globally".to_owned());
            }
            ReceiveOnlyChanges::Ahead => {
                verdict.status = Status::Ahead;
                verdict.reason = Some("receive-only changes are captured locally only".to_owned());
            }
            ReceiveOnlyChanges::Clean => {
                verdict.status = Status::Clean;
            }
        }
        return verdict;
    }
    if state.need_files > 0 {
        verdict.status = Status::Dirty;
        verdict.reason = Some(format!(
            "needs {} file(s) from the cluster",
            state.need_files
        ));
        return verdict;
    }
    match state.state.as_str() {
        "idle" => verdict.status = Status::Clean,
        "scanning" | "syncing" | "sync-preparing" => {
            verdict.status = Status::Ahead;
            verdict.reason = Some(format!(
                "folder is `{}`, local changes are not propagated yet",
                state.state
            ));
        }
        other => {
            verdict.status = Status::Unknown;
            verdict.reason = Some(format!("unexpected folder state `{other}`"));
        }
    }
    verdict
}

/// Percent-encodes a folder id for a query string.
fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(*byte));
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use backupcrawl_plugin_sdk::{HttpResponse, MockHost};

    use super::*;

    fn json(body: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: body.as_bytes().to_vec(),
        }
    }

    fn state(state: &str, need_files: u64, pull_errors: u64) -> FolderState {
        FolderState {
            state: state.to_owned(),
            need_files,
            pull_errors,
            receive_only_changed: false,
        }
    }

    #[test]
    fn canonical_address_defaults_and_rewrites_wildcards() {
        assert_eq!(canonical_address(None), "http://127.0.0.1:8384");
        assert_eq!(
            canonical_address(Some("127.0.0.1:8384")),
            "http://127.0.0.1:8384"
        );
        assert_eq!(
            canonical_address(Some("http://127.0.0.1:8384/")),
            "http://127.0.0.1:8384"
        );
        assert_eq!(
            canonical_address(Some("localhost:8384")),
            "http://127.0.0.1:8384"
        );
        assert_eq!(
            canonical_address(Some("0.0.0.0:8384")),
            "http://127.0.0.1:8384"
        );
        assert_eq!(canonical_address(Some("[::]:8384")), "http://[::1]:8384");
    }

    #[test]
    fn request_carries_the_api_key_and_encoded_folder_id() {
        let mut host = MockHost::new()
            .with_http_response(json(r#"{"state":"idle","needFiles":0,"pullErrors":0}"#));
        let parsed = query(
            "http://127.0.0.1:8384",
            "secret",
            "docs & photos",
            None,
            &mut host,
        )
        .unwrap();
        assert_eq!(parsed.state, "idle");

        let requests = host.http_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(
            requests[0].url,
            "http://127.0.0.1:8384/rest/db/status?folder=docs%20%26%20photos"
        );
        assert_eq!(
            requests[0].headers,
            vec![("X-API-Key".to_owned(), "secret".to_owned())]
        );
    }

    #[test]
    fn unreachable_daemon_is_a_reason_not_a_panic() {
        let mut host = MockHost::new().with_http_error("connection refused");
        let err = query("http://127.0.0.1:1", "secret", "docs", None, &mut host).unwrap_err();
        assert!(err.contains("unreachable"), "{err}");
        assert!(err.contains("connection refused"), "{err}");
    }

    #[test]
    fn non_2xx_is_an_error_with_the_status_code() {
        let mut host = MockHost::new().with_http_response(HttpResponse {
            status: 403,
            headers: Vec::new(),
            body: Vec::new(),
        });
        let err = query("http://127.0.0.1:8384", "wrong", "docs", None, &mut host).unwrap_err();
        assert!(err.contains("HTTP 403"), "{err}");
    }

    #[test]
    fn malformed_json_is_an_error() {
        let mut host = MockHost::new().with_http_response(json("not json"));
        let err = query("http://127.0.0.1:8384", "secret", "docs", None, &mut host).unwrap_err();
        assert!(err.contains("invalid"), "{err}");

        let mut host = MockHost::new().with_http_response(json(r#"{"needFiles":0}"#));
        let err = query("http://127.0.0.1:8384", "secret", "docs", None, &mut host).unwrap_err();
        assert!(err.contains("state"), "{err}");
    }

    #[test]
    fn maps_idle_with_nothing_needed_to_clean() {
        let verdict = map(&state("idle", 0, 0), ReceiveOnlyChanges::Dirty);
        assert_eq!(verdict.status, Status::Clean);
        assert_eq!(verdict.reason, None);
        assert_eq!(verdict.state, "idle");
    }

    #[test]
    fn maps_error_and_pull_errors_to_dirty() {
        assert_eq!(
            map(&state("error", 0, 0), ReceiveOnlyChanges::Dirty).status,
            Status::Dirty
        );
        let verdict = map(&state("idle", 0, 2), ReceiveOnlyChanges::Dirty);
        assert_eq!(verdict.status, Status::Dirty);
        assert!(verdict.reason.unwrap().contains("2 pull error"));
    }

    #[test]
    fn maps_missing_remote_data_to_dirty() {
        let verdict = map(&state("idle", 3, 0), ReceiveOnlyChanges::Dirty);
        assert_eq!(verdict.status, Status::Dirty);
        assert!(verdict.reason.unwrap().contains("3 file"));
    }

    #[test]
    fn maps_busy_states_without_missing_data_to_ahead() {
        for name in ["scanning", "syncing", "sync-preparing"] {
            let verdict = map(&state(name, 0, 0), ReceiveOnlyChanges::Dirty);
            assert_eq!(verdict.status, Status::Ahead, "{name}");
            assert_eq!(verdict.state, name);
        }
    }

    #[test]
    fn maps_receive_only_changes_per_config() {
        let mut changed = state("idle", 0, 0);
        changed.receive_only_changed = true;
        assert_eq!(
            map(&changed, ReceiveOnlyChanges::Dirty).status,
            Status::Dirty
        );
        assert_eq!(
            map(&changed, ReceiveOnlyChanges::Ahead).status,
            Status::Ahead
        );
        assert_eq!(
            map(&changed, ReceiveOnlyChanges::Clean).status,
            Status::Clean
        );
    }

    #[test]
    fn parses_receive_only_counters() {
        let mut host = MockHost::new().with_http_response(json(
            r#"{"state":"idle","needFiles":0,"pullErrors":0,"receiveOnlyChangedFiles":2}"#,
        ));
        let parsed = query("http://127.0.0.1:8384", "k", "docs", None, &mut host).unwrap();
        assert!(parsed.receive_only_changed);

        let mut host = MockHost::new().with_http_response(json(
            r#"{"state":"idle","receiveOnlyChangedFiles":0,"receiveOnlyChangedBytes":0}"#,
        ));
        let parsed = query("http://127.0.0.1:8384", "k", "docs", None, &mut host).unwrap();
        assert!(!parsed.receive_only_changed);
    }

    #[test]
    fn unexpected_state_is_unknown() {
        let verdict = map(&state("paused", 0, 0), ReceiveOnlyChanges::Dirty);
        assert_eq!(verdict.status, Status::Unknown);
        assert!(verdict.reason.unwrap().contains("paused"));
    }
}
