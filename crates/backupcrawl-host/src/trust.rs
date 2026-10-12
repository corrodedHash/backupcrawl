//! Trust-on-first-use for detector plugins (M1.5).
//!
//! The host hashes each plugin's raw `plugin.toml` bytes with SHA-256 and
//! compares the result against the `[plugins.trust]` table of the user config
//! (§10 and §11 of `docs/rust-rewrite-plan.md`). A plugin whose name maps to
//! its current hash is trusted; anything else needs an explicit decision.
//!
//! # Design
//!
//! * **Hash input** is exactly [`Manifest::raw`](crate::manifest::Manifest::raw)
//!   — the `plugin.toml` bytes as read from disk, before parsing — in canonical
//!   `sha256:<lowercase-hex>` form. Hashing the raw bytes means a comment or
//!   whitespace change is a new identity and re-prompts, which is the intent.
//! * **Bundled** plugins ([`PluginSource::Bundled`]) are auto-trusted: they are
//!   compiled into the binary and covered by its integrity, so a prompt would
//!   only train users to click through it. They are still hashed (for display
//!   and for a future audit log) but never prompt.
//! * A **third-party** plugin is trusted when the config maps its name to the
//!   current hash. A missing entry is *trust on first use*; an entry with a
//!   different hash is a *hash change* (for example after an update) and
//!   re-prompts. The stale hash is carried in [`TrustState::Changed`].
//! * **Prompting** is behind the [`TrustPrompt`] trait so the caller decides
//!   how to ask: the CLI reads stdin, `--trust-plugin=NAME` answers without a
//!   prompt, and tests inject a scripted answer. The non-interactive default is
//!   [`DenyPrompt`] (no), so a pipeline never blocks.
//! * **Persistence** ([`save_trust_hash`]) uses [`toml_edit`] so writing one
//!   hash keeps every other table, key and comment of the config intact. One
//!   `[plugins.trust]` slot per plugin is updated in place.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use toml_edit::{DocumentMut, Item, Table};

use crate::discovery::{DiscoveredPlugin, PluginSource};

/// Canonical prefix of every stored manifest hash.
pub const HASH_PREFIX: &str = "sha256:";

/// Hashes raw manifest bytes into the canonical `sha256:<hex>` form.
///
/// This is the only place the trust identity of a plugin is derived; the input
/// is always [`DiscoveredPlugin::raw_manifest`] so callers cannot accidentally
/// hash a re-serialized manifest.
pub fn hash_manifest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{HASH_PREFIX}{}", hex::encode(digest))
}

/// Relative trust state of a discovered plugin.
///
/// The state is independent of any user decision: it only describes how the
/// current hash relates to the trust list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustState {
    /// Bundled plugin: shipped inside the binary, always trusted, never prompts.
    Bundled,
    /// The current manifest hash matches the stored one.
    Trusted,
    /// No hash is stored: trust on first use.
    Unknown,
    /// A hash is stored but differs from the current manifest (hash change);
    /// the previously stored value is kept for the prompt.
    Changed {
        /// The hash from the trust list that no longer matches.
        previous: String,
    },
}

impl TrustState {
    /// Short, stable label for tables and diagnostics.
    pub fn label(&self) -> &'static str {
        match self {
            TrustState::Bundled => "bundled",
            TrustState::Trusted => "trusted",
            TrustState::Unknown => "new",
            TrustState::Changed { .. } => "changed",
        }
    }

    /// Whether the plugin is trusted in this state (no decision required).
    pub fn is_trusted(&self) -> bool {
        matches!(self, TrustState::Bundled | TrustState::Trusted)
    }

    /// Whether a fresh decision (prompt or `--trust-plugin`) is required.
    pub fn needs_confirmation(&self) -> bool {
        matches!(self, TrustState::Unknown | TrustState::Changed { .. })
    }
}

/// Computes the trust state of `plugin` against a `[plugins.trust]` table
/// (plugin name -> `"sha256:..."`).
pub fn trust_state(plugin: &DiscoveredPlugin, trusted: &BTreeMap<String, String>) -> TrustState {
    if plugin.source == PluginSource::Bundled {
        return TrustState::Bundled;
    }
    match trusted.get(&plugin.name) {
        None => TrustState::Unknown,
        Some(stored) => {
            if stored == &hash_manifest(plugin.raw_manifest()) {
                TrustState::Trusted
            } else {
                TrustState::Changed {
                    previous: stored.clone(),
                }
            }
        }
    }
}

/// Everything a user needs to decide whether to trust a plugin.
///
/// Rendered by [`TrustRequest::render`] for the interactive prompt; the CLI
/// also uses it to attribute a `--trust-plugin` confirmation to the right name.
pub struct TrustRequest<'a> {
    /// The plugin whose trust is being decided.
    pub plugin: &'a DiscoveredPlugin,
    /// The current canonical manifest hash.
    pub hash: &'a str,
    /// Why the decision is needed ([`TrustState::Unknown`] or
    /// [`TrustState::Changed`]).
    pub state: &'a TrustState,
}

impl TrustRequest<'_> {
    /// The plugin name, used to match `--trust-plugin`.
    pub fn name(&self) -> &str {
        &self.plugin.name
    }

    /// Renders the request for the interactive prompt.
    ///
    /// Capabilities are printed with `exec` **first** and marked as unsandboxed,
    /// because a spawned program runs with the user's own rights (§10). The
    /// blanket read preopen is printed plainly (`fs: read /`; v1 grants every
    /// plugin all of `/`) followed by the informational `fs-read` intent, the
    /// http/cache grants and the manifest hash.
    pub fn render(&self) -> String {
        let manifest = &self.plugin.manifest;
        let mut out = String::new();

        match self.state {
            TrustState::Changed { previous } => out.push_str(&format!(
                "Plugin `{}` {} changed since it was trusted.\n  previously: {previous}\n",
                self.plugin.name,
                self.plugin.version(),
            )),
            _ => out.push_str(&format!(
                "Plugin `{}` {} from {} requests trust.\n",
                self.plugin.name,
                self.plugin.version(),
                self.plugin.source.label(),
            )),
        }

        // `exec` first and loud: it is the strongest grant.
        if manifest.capabilities.exec.is_empty() {
            out.push_str("  exec:  (none)\n");
        } else {
            out.push_str(&format!(
                "  exec:  {}  <-- runs with your user rights, unsandboxed\n",
                manifest.capabilities.exec.join(", ")
            ));
        }

        out.push_str("  fs:    read / (read-only preopen; v1 grants every plugin all of /)\n");
        if !manifest.capabilities.fs_read.is_empty() {
            out.push_str(&format!(
                "  fs-read: {} (documented intent)\n",
                manifest.capabilities.fs_read.join(", ")
            ));
        }
        if manifest.capabilities.http.is_empty() {
            out.push_str("  http:  (none)\n");
        } else {
            out.push_str(&format!(
                "  http:  {}\n",
                manifest.capabilities.http.join(", ")
            ));
        }
        out.push_str(&format!(
            "  cache: {}\n",
            if manifest.capabilities.cache {
                "yes"
            } else {
                "no"
            }
        ));
        out.push_str(&format!("  manifest hash: {}\n", self.hash));
        out
    }
}

/// Decides whether an unknown or changed plugin is trusted.
///
/// Implementations are allowed to do I/O (the CLI reads stdin); tests inject
/// scripted answers. It is only called for states that
/// [`TrustState::needs_confirmation`].
pub trait TrustPrompt {
    /// Returns `true` to trust the plugin and persist its hash.
    fn confirm(&mut self, request: &TrustRequest<'_>) -> bool;
}

/// The non-interactive default: never confirms, so a pipeline never blocks.
#[derive(Debug, Default, Clone, Copy)]
pub struct DenyPrompt;

impl TrustPrompt for DenyPrompt {
    fn confirm(&mut self, _request: &TrustRequest<'_>) -> bool {
        false
    }
}

/// The trust evaluation of one discovered plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginTrust {
    /// Plugin name.
    pub name: String,
    /// Current canonical manifest hash.
    pub hash: String,
    /// How the current hash relates to the trust list.
    pub state: TrustState,
    /// Whether the plugin is trusted after this evaluation.
    pub trusted: bool,
    /// Whether [`PluginTrust::hash`] should be persisted to the config now.
    pub persist: bool,
}

/// Evaluates one plugin against the trust list, prompting only when needed.
///
/// Bundled and already-trusted plugins never call `prompt`. A confirmed
/// unknown/changed plugin sets [`PluginTrust::persist`] so the caller can write
/// the hash back; a missing or declined decision leaves `trusted = false`.
pub fn evaluate(
    plugin: &DiscoveredPlugin,
    trusted: &BTreeMap<String, String>,
    prompt: &mut dyn TrustPrompt,
) -> PluginTrust {
    let hash = hash_manifest(plugin.raw_manifest());
    let state = trust_state(plugin, trusted);

    let (trusted_now, persist) = if state.needs_confirmation() {
        let request = TrustRequest {
            plugin,
            hash: &hash,
            state: &state,
        };
        let confirmed = prompt.confirm(&request);
        (confirmed, confirmed)
    } else {
        (true, false)
    };

    PluginTrust {
        name: plugin.name.clone(),
        hash,
        state,
        trusted: trusted_now,
        persist,
    }
}

/// Evaluates every discovered plugin, keyed by name.
///
/// Prompt order matches the discovery order (explicit/user > system > bundled),
/// so the output is deterministic. `prompt` is called sequentially.
pub fn evaluate_all(
    plugins: &[DiscoveredPlugin],
    trusted: &BTreeMap<String, String>,
    prompt: &mut dyn TrustPrompt,
) -> BTreeMap<String, PluginTrust> {
    plugins
        .iter()
        .map(|plugin| {
            let evaluation = evaluate(plugin, trusted, prompt);
            (evaluation.name.clone(), evaluation)
        })
        .collect()
}

/// Reads the `[plugins.trust]` table from a config file.
///
/// A missing file or table yields an empty map. Non-string values are skipped;
/// only `name = "sha256:..."` entries are meaningful.
pub fn load_trust_table(path: &Path) -> Result<BTreeMap<String, String>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(err) => {
            return Err(err).with_context(|| format!("cannot read config file {}", path.display()));
        }
    };
    let doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("invalid config file {}", path.display()))?;

    let mut table = BTreeMap::new();
    if let Some(trust) = doc
        .get("plugins")
        .and_then(Item::as_table)
        .and_then(|plugins| plugins.get("trust"))
        .and_then(Item::as_table)
    {
        for (name, item) in trust.iter() {
            if let Some(value) = item.as_str() {
                table.insert(name.to_owned(), value.to_owned());
            }
        }
    }
    Ok(table)
}

/// Writes `name -> hash` into `[plugins.trust]`, preserving the rest of the
/// config (other tables, keys and comments) via [`toml_edit`].
///
/// The file and its parent directory are created when missing. The `hash` is
/// stored verbatim; callers pass the canonical [`hash_manifest`] string.
pub fn save_trust_hash(path: &Path, name: &str, hash: &str) -> Result<()> {
    let mut doc = match fs::read_to_string(path) {
        Ok(text) => text
            .parse::<DocumentMut>()
            .with_context(|| format!("invalid config file {}", path.display()))?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => DocumentMut::new(),
        Err(err) => {
            return Err(err).with_context(|| format!("cannot read config file {}", path.display()));
        }
    };

    let plugins = doc.entry("plugins").or_insert(Item::Table(Table::new()));
    let plugins = plugins
        .as_table_mut()
        .context("config `plugins` must be a table")?;
    let trust = plugins.entry("trust").or_insert(Item::Table(Table::new()));
    let trust = trust
        .as_table_mut()
        .context("config `plugins.trust` must be a table")?;
    trust.insert(name, Item::Value(hash.into()));

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create config directory {}", parent.display()))?;
    }
    fs::write(path, doc.to_string())
        .with_context(|| format!("cannot write config file {}", path.display()))?;
    Ok(())
}
