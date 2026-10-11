//! CLI for the M1.7 spike: `gix-spike-run <component.wasm> <repo-root>`.
//!
//! Prints the JSON summary on stdout and timings on stderr.

use anyhow::{Context, Result};
use gix_feasibility_runner::{default_component_path, SpikeHost};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let first = args.next().context(
        "usage: gix-spike-run [<component.wasm>] <repo-root> [more roots...]\n\
         (the component defaults to $CARGO_TARGET_DIR/wasm32-wasip2/debug/gix_feasibility.wasm)",
    )?;
    let (component, roots) = if first.ends_with(".wasm") {
        (std::path::PathBuf::from(&first), args.collect::<Vec<_>>())
    } else {
        (default_component_path()?, std::iter::once(first).chain(args).collect())
    };
    anyhow::ensure!(!roots.is_empty(), "no repository root given");

    let host = SpikeHost::new(&component)?;
    for root in roots {
        let call = host.run_cold(&root)?;
        println!("{root}: {}", call.json);
        eprintln!(
            "  instantiate {:.1} ms, run {:.1} ms",
            call.instantiate.unwrap_or_default().as_secs_f64() * 1e3,
            call.call.as_secs_f64() * 1e3
        );
    }
    Ok(())
}
