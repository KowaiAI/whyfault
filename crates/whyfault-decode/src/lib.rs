//! Instruction semantics for x86-64: given instruction bytes and the register
//! file before execution, which memory locations the instruction will read and
//! write, and which registers it reads and writes. Built on iced-x86's
//! `InstructionInfo`, which reports explicit and implicit operand access.

use iced_x86::{
    Decoder, DecoderOptions, Instruction, InstructionInfoFactory, MemorySize, Mnemonic, OpAccess,
    Register,
};
use whyfault_trace::reg;

/// Register file in [`whyfault_trace::reg`] order.
pub type Regs = [u64; reg::COUNT];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemOp {
    pub addr: u64,
    pub size: u8,
    pub read: bool,
    pub write: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub len: usize,
    pub mnemonic: Mnemonic,
    pub text: String,
    pub mem: Vec<MemOp>,
    /// Registers read, as trace register ids (only the 16 GPRs + rip/eflags
    /// are tracked; sub-registers map to their full register).
    pub reg_reads: Vec<u8>,
    pub reg_writes: Vec<u8>,
    pub is_syscall: bool,
    /// For `rep`-prefixed string instructions the reported memory operand is
    /// one element; the real footprint depends on rcx and is handled by the
    /// recorder.
    pub is_rep_string: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("invalid or truncated instruction at {0:#x}")]
    Invalid(u64),
}

/// Map an iced register to a trace register id, if it's one we track.
pub fn reg_id(r: Register) -> Option<u8> {
    let full = r.full_register();
    Some(match full {
        Register::RAX => reg::RAX,
        Register::RBX => reg::RBX,
        Register::RCX => reg::RCX,
        Register::RDX => reg::RDX,
        Register::RSI => reg::RSI,
        Register::RDI => reg::RDI,
        Register::RBP => reg::RBP,
        Register::RSP => reg::RSP,
        Register::R8 => reg::R8,
        Register::R9 => reg::R9,
        Register::R10 => reg::R10,
        Register::R11 => reg::R11,
        Register::R12 => reg::R12,
        Register::R13 => reg::R13,
        Register::R14 => reg::R14,
        Register::R15 => reg::R15,
        Register::RIP => reg::RIP,
        _ => return Option::None,
    })
}

fn reg_value(r: Register, regs: &Regs, next_ip: u64) -> u64 {
    if r == Register::RIP {
        return next_ip;
    }
    match reg_id(r) {
        Some(id) => regs[id as usize],
        None => 0,
    }
}

fn mem_size_bytes(ms: MemorySize) -> u8 {
    let s = ms.size();
    if s == 0 {
        // Unknown size (e.g. fxsave area). Cap so the recorder reads something.
        8
    } else {
        s.min(64) as u8
    }
}

pub fn decode(bytes: &[u8], pc: u64, regs: &Regs) -> Result<Decoded, DecodeError> {
    let mut dec = Decoder::with_ip(64, bytes, pc, DecoderOptions::NONE);
    let ins: Instruction = dec.decode();
    if ins.is_invalid() {
        return Err(DecodeError::Invalid(pc));
    }
    let next_ip = ins.next_ip();
    let mut factory = InstructionInfoFactory::new();
    let info = factory.info(&ins);

    let mut mem = Vec::new();
    for m in info.used_memory() {
        let (read, write) = match m.access() {
            OpAccess::Read | OpAccess::CondRead => (true, false),
            OpAccess::Write | OpAccess::CondWrite => (false, true),
            OpAccess::ReadWrite | OpAccess::ReadCondWrite => (true, true),
            OpAccess::None | OpAccess::NoMemAccess => continue,
        };
        // Effective address: base + index*scale + displacement, plus segment
        // base for fs/gs.
        let mut addr = m.displacement().wrapping_add(0);
        if m.base() != Register::None {
            addr = addr.wrapping_add(reg_value(m.base(), regs, next_ip));
        }
        if m.index() != Register::None {
            addr = addr
                .wrapping_add(reg_value(m.index(), regs, next_ip).wrapping_mul(m.scale() as u64));
        }
        match m.segment() {
            Register::FS => addr = addr.wrapping_add(regs[reg::FS_BASE as usize]),
            Register::GS => addr = addr.wrapping_add(regs[reg::GS_BASE as usize]),
            _ => {}
        }
        if m.address_size() == iced_x86::CodeSize::Code32 {
            addr &= 0xffff_ffff;
        }
        mem.push(MemOp {
            addr,
            size: mem_size_bytes(m.memory_size()),
            read,
            write,
        });
    }

    let mut reg_reads = Vec::new();
    let mut reg_writes = Vec::new();
    for ur in info.used_registers() {
        let Some(id) = reg_id(ur.register()) else {
            continue;
        };
        match ur.access() {
            OpAccess::Read | OpAccess::CondRead => reg_reads.push(id),
            OpAccess::Write | OpAccess::CondWrite => reg_writes.push(id),
            OpAccess::ReadWrite | OpAccess::ReadCondWrite => {
                reg_reads.push(id);
                reg_writes.push(id);
            }
            _ => {}
        }
    }
    if ins.rflags_read() != 0 {
        reg_reads.push(reg::EFLAGS);
    }
    if ins.rflags_written() != 0 || ins.rflags_cleared() != 0 || ins.rflags_set() != 0 {
        reg_writes.push(reg::EFLAGS);
    }
    reg_reads.sort_unstable();
    reg_reads.dedup();
    reg_writes.sort_unstable();
    reg_writes.dedup();

    let is_rep_string = ins.has_rep_prefix() || ins.has_repe_prefix() || ins.has_repne_prefix();

    Ok(Decoded {
        len: ins.len(),
        mnemonic: ins.mnemonic(),
        text: format!("{ins}"),
        mem,
        reg_reads,
        reg_writes,
        is_syscall: ins.mnemonic() == Mnemonic::Syscall,
        is_rep_string,
    })
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regs() -> Regs {
        let mut r = [0u64; reg::COUNT];
        r[reg::RDI as usize] = 0x1000;
        r[reg::RSI as usize] = 0x2000;
        r[reg::RSP as usize] = 0x7fff_0000;
        r[reg::RAX as usize] = 0x10;
        r[reg::FS_BASE as usize] = 0x5000_0000;
        r
    }

    #[test]
    fn mov_load_from_rdi_plus_disp() {
        // mov eax, dword ptr [rdi+8]
        let d = decode(&[0x8b, 0x47, 0x08], 0x400000, &regs()).unwrap();
        assert_eq!(d.len, 3);
        assert_eq!(
            d.mem,
            vec![MemOp {
                addr: 0x1008,
                size: 4,
                read: true,
                write: false
            }]
        );
        assert!(d.reg_reads.contains(&reg::RDI));
        assert!(d.reg_writes.contains(&reg::RAX));
    }

    #[test]
    fn store_with_index_scale() {
        // mov qword ptr [rsi+rax*8], rdi
        let d = decode(&[0x48, 0x89, 0x3c, 0xc6], 0x400000, &regs()).unwrap();
        assert_eq!(d.mem[0].addr, 0x2000 + 0x10 * 8);
        assert_eq!(d.mem[0].size, 8);
        assert!(d.mem[0].write && !d.mem[0].read);
    }

    #[test]
    fn push_writes_below_rsp() {
        // push rbp
        let d = decode(&[0x55], 0x400000, &regs()).unwrap();
        assert_eq!(d.mem.len(), 1);
        assert_eq!(d.mem[0].addr, 0x7fff_0000 - 8);
        assert!(d.mem[0].write);
        assert!(d.reg_writes.contains(&reg::RSP));
    }

    #[test]
    fn rip_relative_uses_next_ip() {
        // mov rax, qword ptr [rip+0x100]  (7 bytes)
        let d = decode(
            &[0x48, 0x8b, 0x05, 0x00, 0x01, 0x00, 0x00],
            0x400000,
            &regs(),
        )
        .unwrap();
        assert_eq!(d.mem[0].addr, 0x400007 + 0x100);
    }

    #[test]
    fn fs_relative_tls_load() {
        // mov rax, qword ptr fs:[0x28]
        let d = decode(
            &[0x64, 0x48, 0x8b, 0x04, 0x25, 0x28, 0x00, 0x00, 0x00],
            0x400000,
            &regs(),
        )
        .unwrap();
        assert_eq!(d.mem[0].addr, 0x5000_0028);
    }

    #[test]
    fn lea_has_no_memory_access() {
        // lea rax, [rdi+8]
        let d = decode(&[0x48, 0x8d, 0x47, 0x08], 0x400000, &regs()).unwrap();
        assert!(d.mem.is_empty());
    }

    #[test]
    fn syscall_detected() {
        let d = decode(&[0x0f, 0x05], 0x400000, &regs()).unwrap();
        assert!(d.is_syscall);
    }
}
