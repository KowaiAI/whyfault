//! ptrace single-step recorder.
//!
//! Milestone 1. The recorder forks, `PTRACE_TRACEME`s, execs the target, and
//! single-steps it to completion or fault, emitting one [`whyfault_trace::Step`]
//! per instruction. Memory access values are recovered by decoding each
//! instruction before it executes (to learn addresses and sizes) and reading the
//! bytes afterward.
//!
//! Slow by design: this is the reference recorder that works on any x86-64
//! Linux. Faster backends (rr) come later behind the same trace format.

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
