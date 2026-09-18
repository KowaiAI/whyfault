//! End-to-end: build the corpus program, record it, check the trace has the
//! fault and the memory values we know the program wrote.

use std::path::PathBuf;
use std::process::Command;
use whyfault_trace::{Event, Reader};

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus")
        .join(name)
}

fn gcc_available() -> bool {
    Command::new("gcc").arg("--version").output().is_ok()
}

#[test]
fn records_null_deref_with_correct_values() {
    if !gcc_available() {
        eprintln!("skipping: gcc not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("prog");
    let status = Command::new("gcc")
        .args(["-g", "-O0", "-static", "-o"])
        .arg(&bin)
        .arg(corpus("null-after-early-return/main.c"))
        .status()
        .unwrap();
    assert!(status.success(), "gcc failed");

    let out = dir.path().join("t.wft");
    let r = whyfault_record::record(
        &[bin.to_string_lossy().into_owned()],
        &out,
        &whyfault_record::RecordOptions::default(),
    )
    .unwrap();
    assert!(
        matches!(
            r.outcome,
            whyfault_record::Outcome::Faulted {
                signal: 11,
                fault_addr: Some(0),
                ..
            }
        ),
        "unexpected outcome {:?}",
        r.outcome
    );
    assert!(r.steps > 10_000, "suspiciously short trace: {}", r.steps);

    // Find the address of g_marker via nm, then confirm the trace recorded
    // the 0xdeadbeef store to it.
    let nm = Command::new("nm").arg(&bin).output().unwrap();
    let nm = String::from_utf8_lossy(&nm.stdout);
    let marker = nm
        .lines()
        .find(|l| l.ends_with(" g_marker"))
        .and_then(|l| u64::from_str_radix(l.split_whitespace().next()?, 16).ok())
        .expect("g_marker symbol");

    let rd = Reader::open(&out).unwrap();
    assert!(rd.header.build_id.is_some(), "build-id should be captured");
    let mut saw_marker_store = false;
    let mut saw_fault = false;
    let mut last_idx = 0;
    for ev in rd.events() {
        match ev.unwrap() {
            Event::Step(s) => {
                last_idx = s.idx;
                for m in &s.mem {
                    if m.is_write && m.addr == marker && m.value == [0xef, 0xbe, 0xad, 0xde] {
                        saw_marker_store = true;
                    }
                }
            }
            Event::Fault {
                idx,
                signal,
                fault_addr,
                ..
            } => {
                saw_fault = true;
                assert_eq!(signal, 11);
                assert_eq!(fault_addr, Some(0));
                assert_eq!(idx, last_idx + 1);
            }
            _ => {}
        }
    }
    assert!(
        saw_marker_store,
        "0xdeadbeef store to g_marker not in trace"
    );
    assert!(saw_fault);
}
