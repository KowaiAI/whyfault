# whyfault

**Explains why your program crashed, not just where.**

A stack trace tells you where the program died. `whyfault` runs your program, catches the crash, and prints the chain of events that caused it, line by line back to the input or bug that started it.

```
$ whyfault ./myprog --input bad.bin

CRASH  SIGSEGV in parser.c:203  render_field()
       read of 0x0 via  field->name

WHY
 1. field is NULL at parser.c:203
    <- assigned at parser.c:197  from lookup_field(hdr, idx)
 2. lookup_field returned NULL at parser.c:161
    <- early return because hdr->count == 0
 3. hdr->count was 0 at parser.c:161
    <- last written at parser.c:88 in parse_header():  hdr->count = 0
       (initialization; the assignment at line 95 that sets the real count never ran)
 4. parse_header returned early at parser.c:91
    <- branch taken because remaining < HEADER_MIN (3 < 8)
 5. remaining computed at parser.c:86  = buf_len - offset
    <- buf_len = 3, from read() syscall at io.c:44

ROOT  Input file supplied a 3-byte header; parse_header bails without
      signalling failure, leaving hdr->count at its zero initialization.
```

The numbered steps are computed from a deterministic execution trace by backward dynamic slicing (data and control dependencies), not guessed. See [docs/design.md](docs/design.md).

## Status

Pre-alpha. Linux x86-64 only.

Milestone 1 (recorder) is done: `whyfault record` single-steps a program under ptrace and writes a `.wft` trace with per-instruction register changes and memory accesses (addresses and values). `whyfault dump`, `info`, and `query` inspect a trace; `query` answers "which step last wrote this register or byte?", the primitive the analyzer is built on. `whyfault explain` is not implemented yet (milestone 2).

```
$ whyfault record -o t.wft ./prog
whyfault: recorded 145075 steps to t.wft: Faulted { signal: 11, pc: 0x40193f, fault_addr: Some(0) }
$ whyfault dump t.wft --last 3
#145073   0x000000401937  mov [rbp-28h],rax     W[0x7ffd0bb55868]8=0x0000000000000000
#145074   0x00000040193b  mov rax,[rbp-28h]     R[0x7ffd0bb55868]8=0x0000000000000000
FAULT   #145075 signal=11 pc=0x40193f addr=0x0
$ whyfault query t.wft --loc 0x7ffd0bb55870
[0x7ffd0bb55870] = 0x00, written by step #145051
```

Recorder speed is roughly 25k to 50k instructions per second; fine for programs that crash within seconds. Known limitations: single-threaded targets only, SSE/AVX register values not recorded, `rep` string ops record one element.

## Building

```
cargo build --release
```

## License

Dual-licensed under MIT or Apache-2.0, at your option.
