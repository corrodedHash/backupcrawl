//! M1.5 acceptance: manifest hashing, the trust decision (including bundled
//! auto-trust and hash-change re-prompt) and the `[plugins.trust]` round-trip.

use std::collections::{BTreeMap, VecDeque};
use std::fs;

use backupcrawl_host::discovery::{DiscoveredPlugin, PluginSource};
use backupcrawl_host::manifest::Manifest;
use backupcrawl_host::trust::{
    self, TrustPrompt, TrustRequest, TrustState, hash_manifest, load_trust_table, save_trust_hash,
    trust_state,
};

/// A discovered plugin built from raw manifest text.
fn plugin(name: &str, manifest: &str, source: PluginSource) -> DiscoveredPlugin {
    let manifest = Manifest::parse(manifest.as_bytes()).expect("fixture manifest parses");
    DiscoveredPlugin {
        name: name.to_owned(),
        manifest,
        manifest_path: None,
        wasm_path: None,
        wasm_bytes: Vec::new(),
        source,
        source_dir: None,
        warnings: Vec::new(),
    }
}

const DEMO: &str = r#"
[plugin]
name = "demo"
version = "0.1.0"
api = "1"

[capabilities]
exec = ["demo-tool"]
fs-read = ["~/.config/demo"]
http = ["http://127.0.0.1:9000"]
cache = true
"#;

/// A prompt that answers a fixed script and records the names it was asked
/// about, so tests can assert prompt order and that no prompt happened.
#[derive(Default)]
struct ScriptedPrompt {
    answers: VecDeque<bool>,
    seen: Vec<String>,
}

impl ScriptedPrompt {
    fn new(answers: impl IntoIterator<Item = bool>) -> Self {
        Self {
            answers: answers.into_iter().collect(),
            seen: Vec::new(),
        }
    }
}

impl TrustPrompt for ScriptedPrompt {
    fn confirm(&mut self, request: &TrustRequest<'_>) -> bool {
        self.seen.push(request.name().to_owned());
        self.answers.pop_front().unwrap_or(false)
    }
}

/// A prompt that must never be called; proves a plugin is auto-trusted.
struct PanicPrompt;

impl TrustPrompt for PanicPrompt {
    fn confirm(&mut self, _request: &TrustRequest<'_>) -> bool {
        panic!("plugin must not be prompted");
    }
}

fn trusted_with(name: &str, hash: &str) -> BTreeMap<String, String> {
    BTreeMap::from([(name.to_owned(), hash.to_owned())])
}

// --- hashing ---------------------------------------------------------------

#[test]
fn hashes_manifest_bytes_canonically() {
    // Known SHA-256 of the empty input, in the canonical `sha256:<hex>` form.
    assert_eq!(
        hash_manifest(b""),
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_ne!(hash_manifest(b"a"), hash_manifest(b"b"));
}

#[test]
fn hash_covers_raw_bytes_including_comments_and_whitespace() {
    let base = plugin("demo", DEMO, PluginSource::Explicit);
    let commented = plugin(
        "demo",
        &format!("# a comment\n{DEMO}"),
        PluginSource::Explicit,
    );

    // The comment is part of the hashed identity, so it is a different hash.
    assert_ne!(
        hash_manifest(base.raw_manifest()),
        hash_manifest(commented.raw_manifest())
    );
}

// --- trust state -----------------------------------------------------------

#[test]
fn bundled_plugins_are_auto_trusted_and_never_prompt() {
    let bundled = plugin("demo", DEMO, PluginSource::Bundled);

    assert_eq!(trust_state(&bundled, &BTreeMap::new()), TrustState::Bundled);

    let evaluation = trust::evaluate(&bundled, &BTreeMap::new(), &mut PanicPrompt);
    assert_eq!(evaluation.state, TrustState::Bundled);
    assert!(evaluation.trusted);
    assert!(
        !evaluation.persist,
        "bundled trust is implicit, not persisted"
    );
}

#[test]
fn unknown_hash_is_trust_on_first_use() {
    let demo = plugin("demo", DEMO, PluginSource::Explicit);

    assert_eq!(trust_state(&demo, &BTreeMap::new()), TrustState::Unknown);

    let mut prompt = ScriptedPrompt::new([true]);
    let evaluation = trust::evaluate(&demo, &BTreeMap::new(), &mut prompt);

    assert_eq!(evaluation.state, TrustState::Unknown);
    assert!(evaluation.trusted);
    assert!(evaluation.persist, "a confirmed new hash must be persisted");
    assert_eq!(evaluation.hash, hash_manifest(DEMO.as_bytes()));
    assert_eq!(prompt.seen, ["demo"]);
}

#[test]
fn unknown_hash_declined_is_not_trusted() {
    let demo = plugin("demo", DEMO, PluginSource::Explicit);
    let mut prompt = ScriptedPrompt::new([false]);

    let evaluation = trust::evaluate(&demo, &BTreeMap::new(), &mut prompt);

    assert!(!evaluation.trusted);
    assert!(!evaluation.persist);
    assert_eq!(prompt.seen, ["demo"]);
}

#[test]
fn matching_hash_is_trusted_without_prompting() {
    let demo = plugin("demo", DEMO, PluginSource::Explicit);
    let trusted = trusted_with("demo", &hash_manifest(DEMO.as_bytes()));

    assert_eq!(trust_state(&demo, &trusted), TrustState::Trusted);

    let evaluation = trust::evaluate(&demo, &trusted, &mut PanicPrompt);
    assert!(evaluation.trusted);
    assert!(!evaluation.persist);
}

#[test]
fn hash_change_re_prompts_and_carries_the_old_hash() {
    let demo = plugin("demo", DEMO, PluginSource::Explicit);
    let old = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    let trusted = trusted_with("demo", old);

    let state = trust_state(&demo, &trusted);
    assert_eq!(
        state,
        TrustState::Changed {
            previous: old.to_owned()
        }
    );
    assert!(state.needs_confirmation());

    // Confirm: the changed hash is trusted and must be re-persisted.
    let mut confirm = ScriptedPrompt::new([true]);
    let evaluation = trust::evaluate(&demo, &trusted, &mut confirm);
    assert!(evaluation.trusted);
    assert!(evaluation.persist);
    assert_eq!(evaluation.hash, hash_manifest(DEMO.as_bytes()));
    assert_ne!(evaluation.hash, old);
    assert_eq!(confirm.seen, ["demo"]);

    // Decline: the plugin stays untrusted and the old hash is untouched.
    let mut decline = ScriptedPrompt::new([false]);
    let evaluation = trust::evaluate(&demo, &trusted, &mut decline);
    assert!(!evaluation.trusted);
    assert!(!evaluation.persist);
}

#[test]
fn evaluate_all_keys_by_name_and_prompts_in_discovery_order() {
    let alpha = plugin(
        "alpha",
        DEMO.replace("demo", "alpha").as_str(),
        PluginSource::Explicit,
    );
    let demo = plugin("demo", DEMO, PluginSource::Explicit);
    let bundled = plugin(
        "git",
        DEMO.replace("demo", "git").as_str(),
        PluginSource::Bundled,
    );

    let mut prompt = ScriptedPrompt::new([true, false]);
    let evaluations = trust::evaluate_all(&[alpha, demo, bundled], &BTreeMap::new(), &mut prompt);

    assert_eq!(evaluations.len(), 3);
    assert!(evaluations["alpha"].trusted);
    assert!(!evaluations["demo"].trusted);
    assert!(evaluations["git"].trusted);
    assert_eq!(evaluations["git"].state, TrustState::Bundled);
    // Bundled `git` was not prompted; only the two third-party names were.
    assert_eq!(prompt.seen, ["alpha", "demo"]);
}

// --- prompt rendering ------------------------------------------------------

#[test]
fn prompt_renders_exec_first_then_fs_http_and_hash() {
    let demo = plugin("demo", DEMO, PluginSource::Explicit);
    let hash = hash_manifest(DEMO.as_bytes());
    let state = TrustState::Unknown;
    let request = TrustRequest {
        plugin: &demo,
        hash: &hash,
        state: &state,
    };
    let rendered = request.render();

    let exec = rendered.find("exec:").expect("exec line");
    let fs = rendered.find("fs:").expect("fs line");
    let http = rendered.find("http:").expect("http line");
    let cache = rendered.find("cache:").expect("cache line");
    let digest = rendered.find(&hash).expect("hash line");

    assert!(
        exec < fs && fs < http && http < cache && cache < digest,
        "{rendered}"
    );
    assert!(rendered.contains("demo-tool"), "{rendered}");
    assert!(rendered.contains("read /"), "{rendered}");
    assert!(rendered.contains("unsandboxed"), "{rendered}");
}

#[test]
fn prompt_mentions_the_previous_hash_on_change() {
    let demo = plugin("demo", DEMO, PluginSource::Explicit);
    let hash = hash_manifest(DEMO.as_bytes());
    let old = "sha256:deadbeef";
    let state = TrustState::Changed {
        previous: old.to_owned(),
    };
    let request = TrustRequest {
        plugin: &demo,
        hash: &hash,
        state: &state,
    };

    let rendered = request.render();
    assert!(rendered.contains(old), "{rendered}");
    assert!(rendered.contains("changed"), "{rendered}");
}

// --- persistence -----------------------------------------------------------

#[test]
fn trust_table_round_trips_and_preserves_other_config() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("backupcrawl/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "# my config\n[ignore]\npaths = [\"/proc\", \"/sys\"]\n\n[plugins]\nenabled = [\"git\"]\n\n[plugins.git]\nreport-ignored = false\n",
    )
    .unwrap();

    assert!(load_trust_table(&path).unwrap().is_empty());

    save_trust_hash(&path, "demo", "sha256:aaa").unwrap();
    save_trust_hash(&path, "git", "sha256:bbb").unwrap();

    // Round-trip: both entries read back.
    let table = load_trust_table(&path).unwrap();
    assert_eq!(table.get("demo").map(String::as_str), Some("sha256:aaa"));
    assert_eq!(table.get("git").map(String::as_str), Some("sha256:bbb"));

    // Unrelated tables, keys, order and comments survive the write.
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("# my config"), "{text}");
    assert!(text.contains("[ignore]"), "{text}");
    assert!(text.contains("paths = [\"/proc\", \"/sys\"]"), "{text}");
    assert!(text.contains("enabled = [\"git\"]"), "{text}");
    assert!(text.contains("[plugins.git]"), "{text}");
    assert!(text.contains("report-ignored = false"), "{text}");
    assert!(text.contains("[plugins.trust]"), "{text}");

    // The result is still a valid config for the CLI's strict deserializer.
    let parsed: toml::Value = text.parse().unwrap();
    assert_eq!(
        parsed["plugins"]["trust"]["demo"].as_str(),
        Some("sha256:aaa")
    );
}

#[test]
fn save_creates_missing_file_and_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("nested/backupcrawl/config.toml");

    save_trust_hash(&path, "demo", "sha256:aaa").unwrap();

    assert!(path.is_file());
    assert_eq!(
        load_trust_table(&path)
            .unwrap()
            .get("demo")
            .map(String::as_str),
        Some("sha256:aaa")
    );
}

#[test]
fn save_updates_an_existing_hash_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    fs::write(&path, "[plugins.trust]\ndemo = \"sha256:old\"\n").unwrap();

    save_trust_hash(&path, "demo", "sha256:new").unwrap();

    let table = load_trust_table(&path).unwrap();
    assert_eq!(table.len(), 1);
    assert_eq!(table.get("demo").map(String::as_str), Some("sha256:new"));

    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains("sha256:old"), "{text}");
}

#[test]
fn load_rejects_an_invalid_config() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    fs::write(&path, "not valid = [ toml").unwrap();

    let err = load_trust_table(&path).expect_err("invalid config must be reported");
    assert!(
        format!("{err:#}").contains("invalid config file"),
        "{err:#}"
    );
}
