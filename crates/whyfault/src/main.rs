mod index;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::VecDeque;
use whyfault_trace::{reg, Event, Reader};

#[derive(Parser)]
#[command(
    name = "whyfault",
    version,
    about = "Explains why your program crashed, not just where."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a program under the tracer and write a .wft trace.
    Record {
        /// Output trace path.
        #[arg(short, long, default_value = "crash.wft")]
        output: String,
        /// Stop after N steps (0 = unlimited).
        #[arg(long, default_value_t = 0)]
        max_steps: u64,
        /// Print progress every N steps.
        #[arg(long, default_value_t = 0)]
        progress: u64,
        /// Program and arguments.
        #[arg(required = true, trailing_var_arg = true)]
        argv: Vec<String>,
    },
    /// Print the events in a trace (default: the last 20 steps).
    Dump {
        trace: String,
        /// Show only the last N steps before the end/fault.
        #[arg(long, default_value_t = 20)]
        last: usize,
        /// Show everything.
        #[arg(long)]
        all: bool,
    },
    /// Summarize a trace: program, step count, outcome.
    Info { trace: String },
    /// Ask the last-writer index who wrote a location before a step.
    Query {
        trace: String,
        /// Memory address (hex) or register name.
        #[arg(long)]
        loc: String,
        /// Look for writes strictly before this step index (default: end).
        #[arg(long)]
        before: Option<u64>,
    },
    /// Explain a recorded crash.
    Explain {
        trace: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        verbose: bool,
    },
}

fn fmt_step(s: &whyfault_trace::Step) -> String {
    let regs = [0u64; reg::COUNT];
    let text = whyfault_decode::decode(&s.bytes, s.pc, &regs)
        .map(|d| d.text)
        .unwrap_or_else(|_| "??".into());
    let mut out = format!("#{:<8} {:#014x}  {:<40}", s.idx, s.pc, text);
    for (r, v) in &s.reg_writes {
        if *r == reg::RIP {
            continue;
        }
        out.push_str(&format!(" {}={:#x}", reg::name(*r), v));
    }
    for m in &s.mem {
        let val: String = m.value.iter().rev().map(|b| format!("{b:02x}")).collect();
        out.push_str(&format!(
            " {}[{:#x}]{}={}",
            if m.is_write { "W" } else { "R" },
            m.addr,
            m.size,
            if val.is_empty() {
                "<unreadable>".into()
            } else {
                format!("0x{val}")
            }
        ));
    }
    out
}

fn fmt_event(ev: &Event) -> String {
    match ev {
        Event::Header(h) => format!("HEADER  {:?} build-id={:?}", h.argv, h.build_id),
        Event::ThreadStart { tid, .. } => format!("THREAD  tid={tid}"),
        Event::Step(s) => fmt_step(s),
        Event::Syscall {
            idx, nr, args, ret, ..
        } => {
            format!("SYSCALL #{idx} nr={nr} args={args:x?} ret={ret:#x}")
        }
        Event::Mappings { idx, maps } => format!("MAPS    #{idx} {} regions", maps.len()),
        Event::Fault {
            idx,
            signal,
            pc,
            fault_addr,
            ..
        } => format!(
            "FAULT   #{idx} signal={signal} pc={pc:#x} addr={}",
            fault_addr
                .map(|a| format!("{a:#x}"))
                .unwrap_or_else(|| "-".into())
        ),
        Event::Exit { idx, code } => format!("EXIT    #{idx} code={code}"),
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Record {
            output,
            max_steps,
            progress,
            argv,
        } => {
            let opts = whyfault_record::RecordOptions {
                max_steps,
                progress_every: progress,
            };
            let r = whyfault_record::record(&argv, output.as_ref(), &opts)?;
            eprintln!(
                "whyfault: recorded {} steps to {output}: {:?}",
                r.steps, r.outcome
            );
            Ok(())
        }
        Cmd::Info { trace } => {
            let r = Reader::open(&trace).with_context(|| format!("opening {trace}"))?;
            println!("program:   {}", r.header.argv.join(" "));
            println!(
                "build-id:  {}",
                r.header.build_id.as_deref().unwrap_or("(none)")
            );
            let mut steps = 0u64;
            let mut syscalls = 0u64;
            let mut end = String::from("(truncated)");
            for ev in r.events() {
                match ev? {
                    Event::Step(_) => steps += 1,
                    Event::Syscall { .. } => syscalls += 1,
                    e @ (Event::Fault { .. } | Event::Exit { .. }) => end = fmt_event(&e),
                    _ => {}
                }
            }
            println!("steps:     {steps}");
            println!("syscalls:  {syscalls}");
            println!("end:       {end}");
            Ok(())
        }
        Cmd::Dump { trace, last, all } => {
            let r = Reader::open(&trace)?;
            if all {
                for ev in r.events() {
                    println!("{}", fmt_event(&ev?));
                }
            } else {
                let mut tail: VecDeque<Event> = VecDeque::with_capacity(last + 1);
                for ev in r.events() {
                    let ev = ev?;
                    match &ev {
                        Event::Step(_) | Event::Syscall { .. } => {
                            if tail.len() > last {
                                tail.pop_front();
                            }
                            tail.push_back(ev);
                        }
                        Event::Fault { .. } | Event::Exit { .. } => tail.push_back(ev),
                        _ => {}
                    }
                }
                for ev in tail {
                    println!("{}", fmt_event(&ev));
                }
            }
            Ok(())
        }
        Cmd::Query { trace, loc, before } => {
            let r = Reader::open(&trace)?;
            let mut lw = index::LastWriter::new();
            for ev in r.events() {
                lw.ingest(&ev?);
            }
            let before = before.unwrap_or(u64::MAX - 1);
            if let Some(id) = reg::NAMES.iter().position(|n| *n == loc) {
                match lw.reg_before(id as u8, before) {
                    Some((Some(idx), v)) => println!("{loc} = {v:#x}, written by step #{idx}"),
                    Some((None, v)) => println!("{loc} = {v:#x}, unchanged since trace start"),
                    None => println!("{loc}: no information"),
                }
            } else {
                let addr = u64::from_str_radix(loc.trim_start_matches("0x"), 16)
                    .with_context(|| format!("{loc} is neither a register nor a hex address"))?;
                match lw.mem_before(addr, before) {
                    Some(w) => {
                        println!("[{addr:#x}] = {:#04x}, written by step #{}", w.value, w.idx)
                    }
                    None => println!("[{addr:#x}]: no recorded write before step {before}"),
                }
            }
            Ok(())
        }
        Cmd::Explain { trace, .. } => {
            anyhow::bail!("explain not implemented yet (would read {trace}); milestone 2")
        }
    }
}
