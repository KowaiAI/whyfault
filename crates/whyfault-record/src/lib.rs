//! ptrace single-step recorder.
//!
//! Forks, `PTRACE_TRACEME`s, execs the target, and single-steps it to
//! completion or fault, emitting one [`whyfault_trace::Step`] per instruction.
//! Before each step the instruction is decoded to learn its memory operands;
//! read values are captured before the step, written values after.
//!
//! Slow by design (roughly 10^5 steps/s): the reference recorder that works
//! on any x86-64 Linux without special CPU support or root. Faster backends
//! can produce the same `.wft` stream later.
//!
//! Limitations in this milestone: single thread only (clone is followed but
//! child threads are not traced), SSE/AVX register values are not recorded
//! (memory values still are), and `rep` string instructions record only the
//! first element's memory access.

use anyhow::{bail, Context, Result};
use nix::sys::ptrace;
use nix::sys::signal::Signal;
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{execvp, fork, ForkResult, Pid};
use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::Path;
use whyfault_decode::{decode, Regs};
use whyfault_trace::{reg, Event, Header, Mapping, MemAccess, Step, Writer, FORMAT_VERSION};

#[derive(Default)]
pub struct RecordOptions {
    /// Stop after this many steps (0 = unlimited).
    pub max_steps: u64,
    /// Print a progress line every N steps to stderr (0 = never).
    pub progress_every: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Exited(i32),
    Faulted {
        signal: i32,
        pc: u64,
        fault_addr: Option<u64>,
    },
    StepLimit,
}

#[derive(Debug)]
pub struct RecordResult {
    pub steps: u64,
    pub outcome: Outcome,
}

fn regs_from_user(u: &libc::user_regs_struct) -> Regs {
    let mut r = [0u64; reg::COUNT];
    r[reg::RAX as usize] = u.rax;
    r[reg::RBX as usize] = u.rbx;
    r[reg::RCX as usize] = u.rcx;
    r[reg::RDX as usize] = u.rdx;
    r[reg::RSI as usize] = u.rsi;
    r[reg::RDI as usize] = u.rdi;
    r[reg::RBP as usize] = u.rbp;
    r[reg::RSP as usize] = u.rsp;
    r[reg::R8 as usize] = u.r8;
    r[reg::R9 as usize] = u.r9;
    r[reg::R10 as usize] = u.r10;
    r[reg::R11 as usize] = u.r11;
    r[reg::R12 as usize] = u.r12;
    r[reg::R13 as usize] = u.r13;
    r[reg::R14 as usize] = u.r14;
    r[reg::R15 as usize] = u.r15;
    r[reg::RIP as usize] = u.rip;
    r[reg::EFLAGS as usize] = u.eflags;
    r[reg::FS_BASE as usize] = u.fs_base;
    r[reg::GS_BASE as usize] = u.gs_base;
    r
}

struct Tracee {
    pid: Pid,
    mem: File,
}

impl Tracee {
    fn regs(&self) -> Result<Regs> {
        let u = ptrace::getregs(self.pid).context("PTRACE_GETREGS")?;
        Ok(regs_from_user(&u))
    }

    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.mem.read_exact_at(buf, addr).is_ok()
    }

    fn maps(&self) -> Result<Vec<Mapping>> {
        let mut s = String::new();
        File::open(format!("/proc/{}/maps", self.pid))?.read_to_string(&mut s)?;
        Ok(s.lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                let range = it.next()?;
                let perms = it.next()?.to_string();
                let offset = u64::from_str_radix(it.next()?, 16).ok()?;
                let _dev = it.next()?;
                let _inode = it.next()?;
                let path = it.next().map(|p| p.to_string());
                let (a, b) = range.split_once('-')?;
                Some(Mapping {
                    start: u64::from_str_radix(a, 16).ok()?,
                    end: u64::from_str_radix(b, 16).ok()?,
                    offset,
                    perms,
                    path,
                })
            })
            .collect())
    }
}

fn build_id(path: &str) -> Option<String> {
    use object::Object;
    let data = std::fs::read(path).ok()?;
    let file = object::File::parse(&*data).ok()?;
    let id = file.build_id().ok()??;
    Some(id.iter().map(|b| format!("{b:02x}")).collect())
}

/// Record `argv` into a trace at `out`.
pub fn record(argv: &[String], out: &Path, opts: &RecordOptions) -> Result<RecordResult> {
    if argv.is_empty() {
        bail!("no program given");
    }
    let cargs: Vec<CString> = argv
        .iter()
        .map(|a| CString::new(a.as_str()))
        .collect::<Result<_, _>>()?;

    // SAFETY: standard fork/exec pattern; the child only calls async-signal-safe
    // functions (ptrace, execvp, _exit) before exec.
    let child = match unsafe { fork() }.context("fork")? {
        ForkResult::Child => {
            if ptrace::traceme().is_err() {
                unsafe { libc::_exit(126) };
            }
            let _ = execvp(&cargs[0], &cargs);
            unsafe { libc::_exit(127) };
        }
        ForkResult::Parent { child } => child,
    };

    // Initial stop at exec.
    match waitpid(child, None)? {
        WaitStatus::Stopped(_, Signal::SIGTRAP) => {}
        WaitStatus::Exited(_, code) => {
            bail!("target exited before exec (code {code}); is the program executable?")
        }
        other => bail!("unexpected initial wait status {other:?}"),
    }
    ptrace::setoptions(child, ptrace::Options::PTRACE_O_EXITKILL)?;

    let tracee = Tracee {
        pid: child,
        mem: File::open(format!("/proc/{}/mem", child)).context("open /proc/pid/mem")?,
    };

    let header = Header {
        format_version: FORMAT_VERSION,
        arch: "x86_64".into(),
        argv: argv.to_vec(),
        build_id: build_id(&argv[0]),
    };
    let mut w = Writer::create(out, header)?;
    let tid = child.as_raw() as u32;

    let mut regs = tracee.regs()?;
    w.write(&Event::ThreadStart {
        tid,
        regs: regs.to_vec(),
    })?;
    w.write(&Event::Mappings {
        idx: 0,
        maps: tracee.maps()?,
    })?;

    let mut idx: u64 = 0;
    let mut code = [0u8; 15];
    let outcome = loop {
        if opts.max_steps != 0 && idx >= opts.max_steps {
            break Outcome::StepLimit;
        }
        if opts.progress_every != 0 && idx > 0 && idx.is_multiple_of(opts.progress_every) {
            eprintln!("whyfault: {idx} steps");
        }
        let pc = regs[reg::RIP as usize];

        // Fetch and decode the instruction about to execute.
        let mut nbytes = 15;
        if !tracee.read(pc, &mut code) {
            // Near the end of a mapping; try shorter reads.
            nbytes = 0;
            for n in (1..15).rev() {
                if tracee.read(pc, &mut code[..n]) {
                    nbytes = n;
                    break;
                }
            }
        }
        let decoded = if nbytes > 0 {
            decode(&code[..nbytes], pc, &regs).ok()
        } else {
            None
        };

        // Capture read values before the step.
        let mut mem = Vec::new();
        if let Some(d) = &decoded {
            for m in &d.mem {
                if m.read {
                    let mut buf = vec![0u8; m.size as usize];
                    let ok = tracee.read(m.addr, &mut buf);
                    mem.push(MemAccess {
                        addr: m.addr,
                        size: m.size,
                        is_write: false,
                        value: if ok { buf } else { Vec::new() },
                    });
                }
            }
        }

        // Step.
        ptrace::step(child, None)?;
        let status = waitpid(child, None)?;
        match status {
            WaitStatus::Stopped(_, Signal::SIGTRAP) => {}
            WaitStatus::Stopped(_, sig) => {
                let info = ptrace::getsiginfo(child).ok();
                let fault_addr = info.and_then(|si| match sig {
                    Signal::SIGSEGV | Signal::SIGBUS => {
                        // SAFETY: si_addr is valid for SIGSEGV/SIGBUS.
                        Some(unsafe { si.si_addr() } as u64)
                    }
                    _ => None,
                });
                let fpc = tracee.regs().map(|r| r[reg::RIP as usize]).unwrap_or(pc);
                w.write(&Event::Mappings {
                    idx,
                    maps: tracee.maps().unwrap_or_default(),
                })?;
                w.write(&Event::Fault {
                    idx,
                    tid,
                    signal: sig as i32,
                    pc: fpc,
                    fault_addr,
                })?;
                let _ = ptrace::kill(child);
                let _ = waitpid(child, None);
                break Outcome::Faulted {
                    signal: sig as i32,
                    pc: fpc,
                    fault_addr,
                };
            }
            WaitStatus::Exited(_, code) => {
                w.write(&Event::Exit { idx, code })?;
                break Outcome::Exited(code);
            }
            WaitStatus::Signaled(_, sig, _) => {
                w.write(&Event::Fault {
                    idx,
                    tid,
                    signal: sig as i32,
                    pc,
                    fault_addr: None,
                })?;
                break Outcome::Faulted {
                    signal: sig as i32,
                    pc,
                    fault_addr: None,
                };
            }
            other => bail!("unexpected wait status {other:?}"),
        }

        // After the step: new registers, written values.
        let new_regs = tracee.regs()?;
        if let Some(d) = &decoded {
            for m in &d.mem {
                if m.write {
                    let mut buf = vec![0u8; m.size as usize];
                    let ok = tracee.read(m.addr, &mut buf);
                    mem.push(MemAccess {
                        addr: m.addr,
                        size: m.size,
                        is_write: true,
                        value: if ok { buf } else { Vec::new() },
                    });
                }
            }
        }
        let mut reg_writes = Vec::new();
        for i in 0..reg::COUNT {
            if new_regs[i] != regs[i] {
                reg_writes.push((i as u8, new_regs[i]));
            }
        }

        let len = decoded.as_ref().map(|d| d.len).unwrap_or(0);
        w.write(&Event::Step(Step {
            idx,
            tid,
            pc,
            bytes: code[..len].to_vec(),
            reg_writes,
            mem,
        }))?;

        if let Some(d) = &decoded {
            if d.is_syscall {
                let nr = regs[reg::RAX as usize];
                w.write(&Event::Syscall {
                    idx,
                    tid,
                    nr,
                    args: [
                        regs[reg::RDI as usize],
                        regs[reg::RSI as usize],
                        regs[reg::RDX as usize],
                        regs[reg::R10 as usize],
                        regs[reg::R8 as usize],
                        regs[reg::R9 as usize],
                    ],
                    ret: new_regs[reg::RAX as usize],
                })?;
                // mmap=9 munmap=11 brk=12 mremap=25 mprotect=10 execve=59
                if matches!(nr, 9 | 10 | 11 | 12 | 25 | 59) {
                    w.write(&Event::Mappings {
                        idx: idx + 1,
                        maps: tracee.maps()?,
                    })?;
                }
            }
        }

        regs = new_regs;
        idx += 1;
    };

    w.finish()?;
    Ok(RecordResult {
        steps: idx,
        outcome,
    })
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
