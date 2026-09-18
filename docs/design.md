# whyfault: design

*v1, 2026-09-16. Canonical copy also in Notion (Project HQ / Document Vault).*

## 1. The problem

A stack trace answers "where did it die." Almost every real debugging session is spent answering "why was that value wrong," and that question has no tool. With rr you can run backwards, but it's still a human doing a graph search by hand, one watchpoint at a time. The information needed to answer "why" is fully present in a deterministic recording. whyfault extracts it and hands you a short causal chain.

## 2. Output

Input: a trace of a crash (or sanitizer abort). Output: a causal chain from the faulting operand back to a root cause, as source-level steps (see README for an example). The numbered steps are computed. The ROOT paragraph is a rendering of the same chain, labelled as a summary.

## 3. Core idea: backward dynamic slicing with control dependencies

1. **Identify the culprit operand.** For SIGSEGV, the memory operand's base register or computed address. For a sanitizer report, the pointer passed to the check. For an assert, the value that made the condition false.
2. **Walk data dependencies backward.** Worklist of (location, time). Find the most recent write to that location before that time; decode that instruction's read set; push each read location. The trace makes "most recent write" a lookup.
3. **Add control dependencies.** For each instruction in the slice, add the branch that decided whether it executed (immediate control dependency from the post-dominator tree) and slice the branch condition. This is what explains "the assignment never ran."
4. **Terminate at roots:** syscall results, immediate constants, values from outside the recorded region, uninitialized reads, configurable depth/window.
5. **Compress:** collapse moves and spill/reload pairs; fold address arithmetic into the load/store it feeds; one node per source line; inlined frames via DWARF.
6. **Rank multiple parents** by "surprise" (zero, null, out of range, sanitizer-flagged). Heuristic; labelled; alternatives listed.

## 4. Recorder

MVP recorder is a ptrace single-stepper (`whyfault-record`): works on any x86-64 Linux, no special CPU, no root. Per instruction it records pc, changed registers, and memory accesses with values. Slow (roughly 10^5 to 10^6 instructions/s) but sufficient for anything that crashes within seconds. Faster backends (rr) come later behind the same `.wft` format.

Windowing: analyze the last N million instructions before the fault; extend backward if the slice escapes. Last-writer index is sparse, keyed by page. Threads are serialized by the recorder, so cross-thread dataflow uses the same index.

## 5. Symbolization

DWARF: line table (prefer is_stmt), location lists for named variables, inline info, type info for struct fields. Missing symbols degrade to address-level steps with `??`.

## 6. Narrative layer

Structured chain (JSON) is the truth. ROOT summary: template tier by default (deterministic pattern rules), optional model tier later, never introducing facts not in the chain.

## 7. Degraded mode: core dumps

Static backward slicing within the on-stack functions to list candidate stores; heap reasoning on the bad pointer. Labelled "candidates, not causes." Post-MVP.

## 8. Architecture

```
whyfault record   -> ptrace tracer -> .wft
whyfault explain  -> locate fault -> window -> decode (iced-x86) -> last-writer index + CFG
                     -> slice (data + control) -> compress -> symbolize (gimli) -> render
whyfault core     -> degraded mode (later)
```

Crates: `whyfault-trace` (format), `whyfault-record`, `whyfault-decode`, `whyfault` (CLI and analyzer).

## 9. MVP scope

Linux x86-64; C, C++, Rust with DWARF 4/5; SIGSEGV, SIGABRT from assert, ASan UAF and overflow, UBSan overflow; text and JSON output. Success: on a 30-program corpus, reach the true root in at least 25 and never print a confident chain that is wrong.

## 10. Milestones

1. Trace extraction spike (recorder + decoder + last-writer index).
2. Data slice only, instruction-level output.
3. Symbolization.
4. Control dependencies.
5. Compression and rendering.
6. Sanitizer integration.
7. Corpus and hardening.

## 11. Hard parts

Optimized code (location list gaps); trace size (windowing); control dependency explosion (immediate deps only, short branch slices); JIT (out of scope); syscall boundary as root; recorder speed.

## 12. After the MVP

Differential mode (passing vs failing run); "why is this value X" interactive query; mixed-language stacks; CI integration.

## 13. Prior art

Dynamic slicing: Korel and Laski (1988). Reverse debugging: rr, UndoDB, WinDbg TTD. Failure explanation: Zeller's cause-effect chains (2002). Undo LiveRecorder has "find where written" but not chain synthesis.
