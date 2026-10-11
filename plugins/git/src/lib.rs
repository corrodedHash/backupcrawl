//! Bundled git detector plugin.
//!
//! Stub as of M1.1: the component world is already wired up
//! (`[package.metadata.component]` plus the `wit` symlink to the canonical
//! contract), but no detector is implemented yet. M1.8 fills this crate (gix
//! backend; M1.9 adds the `backend = "exec"` fallback). Until then a
//! `cargo component build -p backupcrawl-plugin-git` output has no detector
//! exports and cannot be loaded by the host.
