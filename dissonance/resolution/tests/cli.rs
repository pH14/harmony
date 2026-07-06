// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `resolution` binary end-to-end: a live scripted session logged to a
//! transcript, then re-rendered from that transcript — asserting the live
//! rendering and the replay are **byte-identical** (the one-renderer principle,
//! through the real process, clap, stdin loop, and file I/O).

use std::io::Write;
use std::process::{Command, Stdio};

use environment::{EnvCodec, FaultPolicy};
use resolution::MomentRef;

/// Drive the bin with a script on stdin; return its stdout.
fn run_bin(args: &[&str], stdin: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_resolution"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn resolution");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "bin exited with {:?}", out.status);
    String::from_utf8(out.stdout).expect("utf8 stdout")
}

#[test]
fn live_session_and_replay_render_identically() {
    let seed = 0xC0FFEE;
    // A MomentRef into the mock's genesis env under the same seed the bin boots.
    let mref = MomentRef::new(EnvCodec::seeded(seed, FaultPolicy::none()), 4_000);

    // A script that exercises the recorded verbs (no `transcript` view, so the
    // live stdout is exactly the recorded render stream).
    let script = format!(
        "# a scripted investigation\n\
         open {mref}\n\
         regs\n\
         read 0x1000 16\n\
         hash\n\
         exec ls /\n\
         vary set 3000 corrupt 0x2000 0xff\n\
         run 5000\n"
    );

    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("session.jsonl");
    let transcript_arg = transcript.to_str().unwrap();

    // Live: run the script, logging the JSONL transcript.
    let seed_arg = seed.to_string();
    let live = run_bin(
        &["--seed", &seed_arg, "--transcript", transcript_arg],
        &script,
    );

    // The transcript file was written and is valid JSONL.
    let jsonl = std::fs::read_to_string(&transcript).unwrap();
    assert!(
        jsonl.lines().count() >= 6,
        "one record per recorded command"
    );

    // Replay: re-render the transcript.
    let replay = run_bin(&["--replay", transcript_arg], "");

    assert_eq!(live, replay, "replay must render byte-identically to live");
    // Sanity: the investigation actually happened.
    assert!(live.contains("opened"));
    assert!(live.contains("TAINTED"));
}
