//! Canonical WIT contract of the plugin ABI.
//!
//! `wit/` is the single source of truth for the `backupcrawl:detector@1.0.0`
//! package: the host generates its bindings from it, the plugin SDK wraps it
//! and every detector component targets its `plugin` world. Do not copy the
//! contract elsewhere; plugin crates symlink their `wit` directory here.

/// The canonical WIT package name.
pub const PACKAGE_NAME: &str = "backupcrawl:detector@1.0.0";

#[cfg(test)]
mod tests {
    use crate::PACKAGE_NAME;
    use wit_parser::Resolve;

    /// The WIT package parses and exposes the agreed interfaces and world.
    #[test]
    fn wit_contract_resolves() {
        let wit_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("wit");
        let mut resolve = Resolve::default();
        let (package_id, _) = resolve
            .push_path(&wit_dir)
            .expect("wit/ must parse as a WIT package");

        let package = &resolve.packages[package_id];
        assert_eq!(package.name.to_string(), PACKAGE_NAME);
        assert!(
            package.interfaces.contains_key("types"),
            "missing `types` interface"
        );
        assert!(
            package.interfaces.contains_key("host"),
            "missing `host` interface"
        );
        assert!(
            package.interfaces.contains_key("detector"),
            "missing `detector` interface"
        );
        assert!(
            package.worlds.contains_key("plugin"),
            "missing `plugin` world"
        );

        let world_id = resolve
            .select_world(&[package_id], Some("plugin"))
            .expect("world `plugin` must be selectable");
        assert_eq!(resolve.worlds[world_id].name, "plugin");
    }
}
