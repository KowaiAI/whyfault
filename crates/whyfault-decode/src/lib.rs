//! Instruction semantics: for one x86-64 instruction, which registers and
//! memory locations it reads and which it writes. Built on iced-x86's
//! `InstructionInfo`, which reports explicit and implicit operand access.

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
