//! The `.wft` trace format.
//!
//! A trace is an append-only sequence of [`Event`]s produced by the recorder and
//! consumed by the analyzer. It carries enough per-instruction state (program
//! counter, changed registers, memory accesses with values) that the analyzer can
//! rebuild a complete last-writer index without re-executing anything.
//!
//! The on-disk encoding is not final. Until v0.1 the format may change without
//! notice; the header carries a version so mismatches fail loudly.

use serde::{Deserialize, Serialize};

pub const FORMAT_VERSION: u32 = 0;

/// Trace file header. Written once, first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Header {
    pub format_version: u32,
    /// Target architecture, e.g. "x86_64".
    pub arch: String,
    /// Command line of the traced program.
    pub argv: Vec<String>,
    /// GNU build-id of the main executable, hex, if present.
    pub build_id: Option<String>,
}

/// One x86-64 general purpose register, by index in the recorder's fixed order.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RegId(pub u8);

/// A memory access performed by one instruction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemAccess {
    pub addr: u64,
    pub size: u8,
    pub is_write: bool,
    /// The bytes read or written, little-endian, `size` long.
    pub value: Vec<u8>,
}

/// Everything the analyzer needs about one executed instruction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Step {
    /// Monotonic instruction index within the trace.
    pub idx: u64,
    pub tid: u32,
    pub pc: u64,
    /// Registers whose value changed as a result of this instruction, with the
    /// new value. Registers read but unchanged are not listed; the decoder
    /// recovers read sets from the instruction bytes.
    pub reg_writes: Vec<(RegId, u64)>,
    pub mem: Vec<MemAccess>,
}

/// A trace event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Event {
    Header(Header),
    /// Initial register file for a thread, before its first step.
    ThreadStart {
        tid: u32,
        regs: Vec<u64>,
    },
    Step(Step),
    /// A syscall boundary. Syscall results are analysis roots.
    Syscall {
        idx: u64,
        tid: u32,
        nr: u64,
        args: [u64; 6],
        ret: u64,
    },
    /// A memory mapping became visible (exec, mmap, brk). Lets the analyzer
    /// symbolize addresses in shared objects.
    Mapping {
        idx: u64,
        start: u64,
        end: u64,
        offset: u64,
        path: Option<String>,
    },
    /// The traced process received a fatal signal at `pc`.
    Fault {
        idx: u64,
        tid: u32,
        signal: i32,
        pc: u64,
        fault_addr: Option<u64>,
    },
    Exit {
        idx: u64,
        code: i32,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("unsupported trace format version {0} (this build reads {FORMAT_VERSION})")]
    Version(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = Header {
            format_version: FORMAT_VERSION,
            arch: "x86_64".into(),
            argv: vec!["./t".into()],
            build_id: None,
        };
        let s = serde_json::to_string(&Event::Header(h.clone())).unwrap();
        let back: Event = serde_json::from_str(&s).unwrap();
        assert_eq!(back, Event::Header(h));
    }
}
