//! The `.wft` trace format.
//!
//! A trace is an append-only sequence of [`Event`]s produced by the recorder and
//! consumed by the analyzer. It carries enough per-instruction state (program
//! counter, changed registers, memory accesses with values) that the analyzer can
//! rebuild a complete last-writer index without re-executing anything.
//!
//! On disk: an 8-byte magic `WFTRACE0`, then a stream of postcard-encoded
//! [`Event`]s, each prefixed by a little-endian u32 byte length. The format is
//! not stable before v0.1; [`FORMAT_VERSION`] in the header guards mismatches.

use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;

pub const FORMAT_VERSION: u32 = 1;
pub const MAGIC: &[u8; 8] = b"WFTRACE0";

/// Register indices used by [`Step::reg_writes`] and [`Event::ThreadStart`].
/// Fixed order shared by recorder and analyzer.
pub mod reg {
    pub const RAX: u8 = 0;
    pub const RBX: u8 = 1;
    pub const RCX: u8 = 2;
    pub const RDX: u8 = 3;
    pub const RSI: u8 = 4;
    pub const RDI: u8 = 5;
    pub const RBP: u8 = 6;
    pub const RSP: u8 = 7;
    pub const R8: u8 = 8;
    pub const R9: u8 = 9;
    pub const R10: u8 = 10;
    pub const R11: u8 = 11;
    pub const R12: u8 = 12;
    pub const R13: u8 = 13;
    pub const R14: u8 = 14;
    pub const R15: u8 = 15;
    pub const RIP: u8 = 16;
    pub const EFLAGS: u8 = 17;
    pub const FS_BASE: u8 = 18;
    pub const GS_BASE: u8 = 19;
    pub const COUNT: usize = 20;

    pub const NAMES: [&str; COUNT] = [
        "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp", "r8", "r9", "r10", "r11", "r12",
        "r13", "r14", "r15", "rip", "eflags", "fs_base", "gs_base",
    ];

    pub fn name(id: u8) -> &'static str {
        NAMES.get(id as usize).copied().unwrap_or("?")
    }
}

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

/// A memory access performed by one instruction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemAccess {
    pub addr: u64,
    pub size: u8,
    pub is_write: bool,
    /// The bytes read or written, little-endian, `size` long. Empty if the
    /// recorder could not read them (the access itself faulted).
    pub value: Vec<u8>,
}

/// Everything the analyzer needs about one executed instruction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Step {
    /// Monotonic instruction index within the trace.
    pub idx: u64,
    pub tid: u32,
    pub pc: u64,
    /// Raw instruction bytes (up to 15).
    pub bytes: Vec<u8>,
    /// Registers whose value changed as a result of this instruction, with the
    /// new value. Registers read but unchanged are not listed; the decoder
    /// recovers read sets from the instruction bytes.
    pub reg_writes: Vec<(u8, u64)>,
    pub mem: Vec<MemAccess>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mapping {
    pub start: u64,
    pub end: u64,
    pub offset: u64,
    pub perms: String,
    pub path: Option<String>,
}

/// A trace event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Event {
    Header(Header),
    /// Initial register file for a thread, before its first step, in
    /// [`reg`] order.
    ThreadStart {
        tid: u32,
        regs: Vec<u64>,
    },
    Step(Step),
    /// A syscall boundary, emitted after the `syscall` instruction's step.
    /// Syscall results are analysis roots.
    Syscall {
        idx: u64,
        tid: u32,
        nr: u64,
        args: [u64; 6],
        ret: u64,
    },
    /// Full snapshot of the address space layout at `idx`.
    Mappings {
        idx: u64,
        maps: Vec<Mapping>,
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
    #[error("not a whyfault trace (bad magic)")]
    Magic,
    #[error("unsupported trace format version {0} (this build reads {FORMAT_VERSION})")]
    Version(u32),
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("encoding: {0}")]
    Encoding(#[from] postcard::Error),
}

pub struct Writer<W: Write> {
    inner: BufWriter<W>,
}

impl Writer<std::fs::File> {
    pub fn create(path: impl AsRef<Path>, header: Header) -> Result<Self, TraceError> {
        let f = std::fs::File::create(path)?;
        Self::new(f, header)
    }
}

impl<W: Write> Writer<W> {
    pub fn new(w: W, header: Header) -> Result<Self, TraceError> {
        let mut inner = BufWriter::with_capacity(1 << 20, w);
        inner.write_all(MAGIC)?;
        let mut me = Writer { inner };
        me.write(&Event::Header(header))?;
        Ok(me)
    }

    pub fn write(&mut self, ev: &Event) -> Result<(), TraceError> {
        let buf = postcard::to_stdvec(ev)?;
        self.inner.write_all(&(buf.len() as u32).to_le_bytes())?;
        self.inner.write_all(&buf)?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), TraceError> {
        self.inner.flush()?;
        Ok(())
    }
}

pub struct Reader<R: Read> {
    inner: BufReader<R>,
    pub header: Header,
}

impl Reader<std::fs::File> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TraceError> {
        Self::new(std::fs::File::open(path)?)
    }
}

impl<R: Read> Reader<R> {
    pub fn new(r: R) -> Result<Self, TraceError> {
        let mut inner = BufReader::with_capacity(1 << 20, r);
        let mut magic = [0u8; 8];
        inner.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(TraceError::Magic);
        }
        let mut me = Reader {
            inner,
            header: Header {
                format_version: 0,
                arch: String::new(),
                argv: vec![],
                build_id: None,
            },
        };
        match me.next_event()? {
            Some(Event::Header(h)) if h.format_version == FORMAT_VERSION => me.header = h,
            Some(Event::Header(h)) => return Err(TraceError::Version(h.format_version)),
            _ => return Err(TraceError::Magic),
        }
        Ok(me)
    }

    pub fn next_event(&mut self) -> Result<Option<Event>, TraceError> {
        if self.inner.fill_buf()?.is_empty() {
            return Ok(None);
        }
        let mut len = [0u8; 4];
        self.inner.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len) as usize;
        let mut buf = vec![0u8; len];
        self.inner.read_exact(&mut buf)?;
        Ok(Some(postcard::from_bytes(&buf)?))
    }

    /// Iterate over remaining events. Stops at the first error.
    pub fn events(self) -> Events<R> {
        Events { r: self }
    }
}

pub struct Events<R: Read> {
    r: Reader<R>,
}

impl<R: Read> Iterator for Events<R> {
    type Item = Result<Event, TraceError>;
    fn next(&mut self) -> Option<Self::Item> {
        self.r.next_event().transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> Header {
        Header {
            format_version: FORMAT_VERSION,
            arch: "x86_64".into(),
            argv: vec!["./t".into()],
            build_id: None,
        }
    }

    #[test]
    fn roundtrip_stream() {
        let mut buf = Vec::new();
        {
            let mut w = Writer::new(&mut buf, header()).unwrap();
            w.write(&Event::ThreadStart {
                tid: 1,
                regs: vec![0; reg::COUNT],
            })
            .unwrap();
            w.write(&Event::Step(Step {
                idx: 0,
                tid: 1,
                pc: 0x401000,
                bytes: vec![0x48, 0x89, 0xc7],
                reg_writes: vec![(reg::RDI, 42)],
                mem: vec![MemAccess {
                    addr: 0x1000,
                    size: 8,
                    is_write: true,
                    value: vec![1, 0, 0, 0, 0, 0, 0, 0],
                }],
            }))
            .unwrap();
            w.write(&Event::Exit { idx: 1, code: 0 }).unwrap();
            w.finish().unwrap();
        }
        let r = Reader::new(&buf[..]).unwrap();
        assert_eq!(r.header, header());
        let evs: Vec<_> = r.events().map(|e| e.unwrap()).collect();
        assert_eq!(evs.len(), 3);
        assert!(matches!(evs[2], Event::Exit { code: 0, .. }));
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(matches!(
            Reader::new(&b"NOTATRACE"[..]).err(),
            Some(TraceError::Magic)
        ));
    }
}
