//! Resource limits for the wasmtime host.
//!
//! Section 9 of `docs/rust-rewrite-plan.md` requires three kinds of protection:
//!
//! * a **linear-memory cap** per detector instance (default 256 MiB) so a
//!   memory bomb cannot exhaust the host;
//! * **fuel / epoch interruption** so guest code cannot spin forever: fuel is a
//!   deterministic per-call budget, while an epoch deadline checked every
//!   [`Limits::epoch_interval_ms`] gives the host a way to *cooperatively*
//!   cancel a detector mid-call (see [`crate::runtime`]);
//! * **exec / HTTP** timeout and output/body caps enforced by the host imports
//!   (see [`crate::imports`]).
//!
//! All values are collected in [`Limits`], which implements [`Default`] with the
//! values documented on each field. Tests override individual fields with struct
//! update syntax. The struct is deliberately plain data (no policy logic); the
//! runtime and the imports consult it.

/// One mebibyte.
const MIB: usize = 1024 * 1024;

/// Resource limits applied to a detector instance and its host imports.
///
/// The [`Default`] implementation chooses values from section 9 of the rewrite
/// plan (256 MiB linear memory, ~50 ms cancellation granularity, 64 MiB exec and
/// HTTP output caps).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Maximum number of bytes a single linear memory may grow to.
    ///
    /// Enforced by wasmtime's [`StoreLimits`](wasmtime::StoreLimits) per store;
    /// growing beyond this fails (a memory bomb traps instead of exhausting the
    /// host).
    pub max_memory: usize,

    /// Fuel (roughly: executed instructions) granted to each guest call.
    ///
    /// The budget is refilled before every `init`/`check_*`/`finish` call, so a
    /// detector can do real work across the scan while a single runaway call
    /// cannot run forever. Exhausting it raises `Trap::OutOfFuel`, which the
    /// runtime records as a per-plugin error (the call becomes "no claim").
    pub fuel_per_call: u64,

    /// How often the epoch watchdog advances the engine epoch, in milliseconds.
    ///
    /// This is the granularity of cooperative cancellation: a detector call
    /// checks the runtime's cancellation flag at least this often.
    pub epoch_interval_ms: u64,

    /// Default timeout for [`exec`](crate::imports::HostState) requests that do
    /// not set their own `timeout_ms`.
    pub exec_timeout_ms: u64,

    /// Maximum number of bytes captured from a child's stdout and stderr
    /// individually.
    pub exec_output_cap: usize,

    /// Default timeout for HTTP requests, in milliseconds.
    pub http_timeout_ms: u64,

    /// Maximum number of bytes captured from an HTTP response body.
    pub http_body_cap: usize,

    /// Number of consecutive failing guest calls after which a detector is
    /// disabled for the rest of the scan (the circuit breaker, section 5.4).
    pub max_consecutive_failures: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_memory: 256 * MIB,
            fuel_per_call: 10_000_000_000,
            epoch_interval_ms: 50,
            exec_timeout_ms: 30_000,
            exec_output_cap: 64 * MIB,
            http_timeout_ms: 30_000,
            http_body_cap: 64 * MIB,
            max_consecutive_failures: 5,
        }
    }
}

impl Limits {
    /// The default exec timeout as a `u32` (the WIT `timeout-ms` type).
    pub fn exec_timeout_ms_u32(&self) -> u32 {
        self.exec_timeout_ms.min(u64::from(u32::MAX)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_plan() {
        let limits = Limits::default();

        assert_eq!(limits.max_memory, 256 * MIB);
        assert_eq!(limits.epoch_interval_ms, 50);
        assert_eq!(limits.exec_output_cap, 64 * MIB);
        assert_eq!(limits.http_body_cap, 64 * MIB);
        assert!(limits.fuel_per_call > 0);
        assert!(limits.max_consecutive_failures > 0);
    }

    #[test]
    fn exec_timeout_saturates_at_u32() {
        let limits = Limits {
            exec_timeout_ms: u64::from(u32::MAX) + 10,
            ..Limits::default()
        };
        assert_eq!(limits.exec_timeout_ms_u32(), u32::MAX);
    }
}
