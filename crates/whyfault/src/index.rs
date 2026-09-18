//! Last-writer index: for any register or memory byte, which step most
//! recently wrote it before a given time. This is the lookup that makes
//! backward slicing a graph walk instead of a re-execution.

use std::collections::HashMap;
use whyfault_trace::{reg, Event, Step};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Write {
    pub idx: u64,
    pub value: u8,
}

#[derive(Default)]
pub struct LastWriter {
    /// Per register: (step idx, new value), ascending by idx.
    regs: Vec<Vec<(u64, u64)>>,
    /// Per memory byte address: writes, ascending by idx.
    mem: HashMap<u64, Vec<Write>>,
    pub steps: u64,
}

impl LastWriter {
    pub fn new() -> Self {
        Self {
            regs: vec![Vec::new(); reg::COUNT],
            mem: HashMap::new(),
            steps: 0,
        }
    }

    pub fn ingest(&mut self, ev: &Event) {
        match ev {
            Event::ThreadStart { regs, .. } => {
                for (i, v) in regs.iter().enumerate().take(reg::COUNT) {
                    // Initial values are "written" before step 0; use u64::MAX
                    // sentinel meaning "entered the trace with this value".
                    self.regs[i].push((u64::MAX, *v));
                }
            }
            Event::Step(s) => self.ingest_step(s),
            _ => {}
        }
    }

    fn ingest_step(&mut self, s: &Step) {
        self.steps = self.steps.max(s.idx + 1);
        for (r, v) in &s.reg_writes {
            self.regs[*r as usize].push((s.idx, *v));
        }
        for m in &s.mem {
            if !m.is_write {
                continue;
            }
            for (i, b) in m.value.iter().enumerate() {
                self.mem.entry(m.addr + i as u64).or_default().push(Write {
                    idx: s.idx,
                    value: *b,
                });
            }
        }
    }

    /// Step index that most recently wrote register `r` strictly before
    /// `before`, and the value it wrote. `None` if the register still holds
    /// its trace-entry value (returned as `Some((None, value))`).
    pub fn reg_before(&self, r: u8, before: u64) -> Option<(Option<u64>, u64)> {
        let v = &self.regs[r as usize];
        // entries: sentinel first (u64::MAX) then ascending idx.
        let mut best: Option<(Option<u64>, u64)> = None;
        for (idx, val) in v {
            if *idx == u64::MAX {
                best = Some((None, *val));
            } else if *idx < before {
                best = Some((Some(*idx), *val));
            } else {
                break;
            }
        }
        best
    }

    /// Step index that most recently wrote memory byte `addr` strictly before
    /// `before`. `None` if no recorded write.
    pub fn mem_before(&self, addr: u64, before: u64) -> Option<Write> {
        let v = self.mem.get(&addr)?;
        let pos = v.partition_point(|w| w.idx < before);
        if pos == 0 {
            None
        } else {
            Some(v[pos - 1])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whyfault_trace::MemAccess;

    fn step(idx: u64, rw: Vec<(u8, u64)>, mem: Vec<MemAccess>) -> Event {
        Event::Step(Step {
            idx,
            tid: 1,
            pc: 0,
            bytes: vec![],
            reg_writes: rw,
            mem,
        })
    }

    #[test]
    fn register_and_memory_lookup() {
        let mut lw = LastWriter::new();
        lw.ingest(&Event::ThreadStart {
            tid: 1,
            regs: vec![7; reg::COUNT],
        });
        lw.ingest(&step(0, vec![(reg::RAX, 1)], vec![]));
        lw.ingest(&step(
            1,
            vec![],
            vec![MemAccess {
                addr: 0x1000,
                size: 2,
                is_write: true,
                value: vec![0xaa, 0xbb],
            }],
        ));
        lw.ingest(&step(2, vec![(reg::RAX, 2)], vec![]));

        assert_eq!(lw.reg_before(reg::RAX, 0), Some((None, 7)));
        assert_eq!(lw.reg_before(reg::RAX, 1), Some((Some(0), 1)));
        assert_eq!(lw.reg_before(reg::RAX, 5), Some((Some(2), 2)));
        assert_eq!(lw.reg_before(reg::RBX, 5), Some((None, 7)));
        assert_eq!(lw.mem_before(0x1001, 1), None);
        assert_eq!(
            lw.mem_before(0x1001, 2),
            Some(Write {
                idx: 1,
                value: 0xbb
            })
        );
        assert_eq!(lw.mem_before(0x1002, 9), None);
    }
}
