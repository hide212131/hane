//! The connection against a real child process: the stand-in engine started
//! with an inherited pipe, connecting over the dedicated channel. Covers the
//! normal script, refused hellos, a broken event sequence, a connection cut in
//! the middle, a silent engine, cancel, and reclaiming the process tree.
#![cfg(unix)]

use hane_editor::{
    Editor, EndStatus, PolishMode, PolishResult, RawStatus, Selection,
    VoiceApply, VoiceEnvelope, VoiceError, VoiceEvent,
};
use hane_voice::{
    EngineLocation, Failure, Inbound, Timeouts, VoiceControl, VoiceEngine,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SESSION: &str = "a162d4cb5747474d9cf7fcaf1833dabb";
const ENGINE: &str = env!("CARGO_BIN_EXE_hane-voice-test-engine");

fn quick() -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(5),
        frame: Duration::from_secs(2),
        begin: Duration::from_secs(10),
        drain: Duration::from_secs(10),
        cancel: Duration::from_secs(10),
        exit_grace: Duration::from_millis(500),
    }
}

/// For the tests that wait for a silent engine to be noticed.
fn impatient() -> Timeouts {
    Timeouts {
        begin: Duration::from_millis(700),
        ..quick()
    }
}

struct Run {
    engine: VoiceEngine,
    inbound: async_channel::Receiver<Inbound>,
    dir: PathBuf,
}

impl Run {
    fn start(mode: &str) -> Self {
        Self::with(mode, Path::new(ENGINE), quick())
    }

    fn with(mode: &str, exe: &Path, timeouts: Timeouts) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "hane-voice-proc-{}",
            hane_link::auth::random_hex(8).unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let args = ["--mode", mode, "--pidfile", "pids", "--log", "log"];
        let extra_args = args
            .iter()
            .map(|arg| match *arg {
                "pids" | "log" => dir.join(arg).into_os_string(),
                other => other.into(),
            })
            .collect();
        let (engine, inbound) = VoiceEngine::with_platform(
            std::sync::Arc::from(hane_voice::transport::system_platform()),
            EngineLocation {
                registration: None,
                explicit: Some(exe.to_owned()),
                extra_args,
            },
            timeouts,
        );
        Self { engine, inbound, dir }
    }

    fn next(&self) -> Inbound {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Ok(inbound) = self.inbound.try_recv() {
                return inbound;
            }
            assert!(Instant::now() < deadline, "no message from the engine");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn pids(&self) -> Vec<i32> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(text) = std::fs::read_to_string(self.dir.join("pids"))
                && let pids @ [_, ..] = text
                    .split_whitespace()
                    .filter_map(|part| part.parse().ok())
                    .collect::<Vec<i32>>()
                    .as_slice()
            {
                return pids.to_vec();
            }
            assert!(Instant::now() < deadline, "the engine never started");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.engine.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

use std::path::Path;

fn gone(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) != 0 }
}

fn all_gone(pids: &[i32]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pids.iter().all(|pid| gone(*pid)) {
        assert!(Instant::now() < deadline, "processes left behind: {pids:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn editor() -> Editor {
    let mut editor = Editor::new("前文。");
    editor
        .set_selection(Selection::caret(hane_document::SourceOffset(
            "前文。".len(),
        )))
        .unwrap();
    editor.voice_begin(SESSION).unwrap();
    editor
}

/// The fixed script the debug command (F8) feeds, as events.
fn script() -> Vec<VoiceEvent> {
    let raw = |seq: u64, text: &str, polish: PolishMode| VoiceEvent::SegmentRaw {
        seq,
        status: RawStatus::Ok { text: text.to_owned(), polish },
    };
    vec![
        VoiceEvent::SessionStart,
        raw(1, "えーと、今日はいい天気ですね。", PolishMode::Pending),
        raw(2, "つぎのしょうです", PolishMode::Pending),
        VoiceEvent::SegmentPolished { seq: 2, result: PolishResult::Ok("次の章です。".to_owned()) },
        VoiceEvent::SegmentPolished { seq: 1, result: PolishResult::Ok("今日はいい天気ですね。".to_owned()) },
        raw(3, "Hello, voice composition.", PolishMode::Off),
        VoiceEvent::SegmentRaw { seq: 4, status: RawStatus::Silent },
        VoiceEvent::SessionEnd { status: EndStatus::Completed, last_seq: 4 },
    ]
}

fn shown(editor: &mut Editor) -> Vec<(u64, usize, String)> {
    editor.voice_sync();
    editor
        .voice()
        .map(|voice| {
            voice
                .segments()
                .iter()
                .map(|segment| (segment.seq, segment.offset(), segment.display_text()))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn the_script_through_a_real_engine_matches_the_debug_script() {
    // Directly, as the debug command does.
    let mut direct = editor();
    for (index, event) in script().into_iter().enumerate() {
        direct
            .voice_apply(VoiceEnvelope {
                session_id: SESSION.to_owned(),
                event_no: index as u64 + 1,
                event,
            })
            .unwrap();
    }
    let direct_shown = shown(&mut direct);
    direct.voice_commit_all().unwrap();
    let direct_text = direct.document().full_text();

    // Through the engine process.
    let run = Run::start("normal");
    let mut viaengine = editor();
    run.engine.begin(SESSION, None).unwrap();
    let mut received = 0;
    // session.start + 3 raw + 2 polished + raw + silent = 7, then stop.
    while received < 7 {
        match run.next() {
            Inbound::Event(envelope) => {
                assert_eq!(envelope.event_no, received + 1);
                assert_eq!(viaengine.voice_apply(envelope), Ok(VoiceApply::Applied));
                received += 1;
            }
            other => panic!("{other:?}"),
        }
    }
    run.engine.stop(SESSION);
    match run.next() {
        Inbound::Event(envelope) => {
            assert!(matches!(
                envelope.event,
                VoiceEvent::SessionEnd { status: EndStatus::Completed, last_seq: 4 }
            ));
            viaengine.voice_apply(envelope).unwrap();
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(shown(&mut viaengine), direct_shown);
    viaengine.voice_commit_all().unwrap();
    assert_eq!(viaengine.document().full_text(), direct_text);
    assert_eq!(run.log(), ["capture.begin", "capture.stop"]);
}

#[test]
fn a_refused_hello_ends_the_engine_without_recording() {
    for (mode, expected) in [
        ("bad-token", Failure::Auth("token")),
        ("bad-pid", Failure::Auth("process id")),
        ("bad-protocol", Failure::Auth("protocol mismatch")),
    ] {
        let run = Run::start(mode);
        run.engine.begin(SESSION, None).unwrap();
        assert_eq!(run.next(), Inbound::Lost(expected), "{mode}");
        all_gone(&run.pids());
        assert!(run.log().is_empty(), "{mode}: no recording control was delivered");
    }
}

#[test]
fn an_event_number_jump_cancels_the_session_and_ends_the_engine() {
    let run = Run::start("gap");
    let mut editor = editor();
    run.engine.begin(SESSION, None).unwrap();
    let Inbound::Event(start) = run.next() else { panic!() };
    editor.voice_apply(start).unwrap();
    assert_eq!(run.next(), Inbound::Lost(Failure::Violation("event number gap")));
    // The UI treats the loss as a cancel: confirmed text is untouched and the
    // recording is released.
    editor.voice_cancel();
    assert!(editor.voice().is_none());
    assert_eq!(editor.document().full_text(), "前文。");
    all_gone(&run.pids());
}

#[test]
fn an_editor_side_contract_violation_is_refused_too() {
    // The editor checks what the link cannot (here a repeated number with other
    // content): the session is dropped.
    let mut editor = editor();
    let start = VoiceEnvelope { session_id: SESSION.into(), event_no: 1, event: VoiceEvent::SessionStart };
    editor.voice_apply(start).unwrap();
    let again = VoiceEnvelope {
        session_id: SESSION.into(),
        event_no: 1,
        event: VoiceEvent::SegmentRaw { seq: 1, status: RawStatus::Silent },
    };
    assert!(matches!(editor.voice_apply(again), Err(VoiceError::Contract(_))));
    assert!(editor.voice().is_none());
}

#[test]
fn a_cut_in_the_middle_keeps_what_was_confirmed_and_drops_the_rest() {
    let run = Run::start("die");
    let mut editor = editor();
    run.engine.begin(SESSION, None).unwrap();
    for _ in 0..2 {
        let Inbound::Event(envelope) = run.next() else { panic!() };
        editor.voice_apply(envelope).unwrap();
    }
    editor.voice_commit(1).unwrap();
    assert_eq!(run.next(), Inbound::Lost(Failure::Disconnected(Some(1))));
    editor.voice_cancel();
    assert_eq!(editor.document().full_text(), "前文。一つ目");
    all_gone(&run.pids());
}

#[test]
fn a_silent_engine_is_ended_with_its_workers() {
    let run = Run::with("worker", Path::new(ENGINE), impatient());
    run.engine.begin(SESSION, None).unwrap();
    let pids = run.pids();
    assert_eq!(pids.len(), 2, "engine and worker");
    assert_eq!(run.next(), Inbound::Lost(Failure::Unresponsive));
    all_gone(&pids);
}

#[test]
fn shutdown_and_drop_leave_no_process_behind() {
    let run = Run::start("worker");
    run.engine.begin(SESSION, None).unwrap();
    let pids = run.pids();
    run.engine.shutdown();
    all_gone(&pids);

    let run = Run::start("worker");
    run.engine.begin(SESSION, None).unwrap();
    let pids = run.pids();
    // Dropping the engine alone (Hane exiting without a clean shutdown call).
    let Run { engine, dir, .. } = &run;
    let _ = dir;
    let _ = engine;
    drop(run);
    all_gone(&pids);
}

#[test]
fn cancel_reaches_the_engine_and_its_late_end_is_not_taken_in() {
    let run = Run::start("normal");
    run.engine.begin(SESSION, None).unwrap();
    let Inbound::Event(_) = run.next() else { panic!() };
    run.engine.cancel(SESSION);
    run.engine.cancel(SESSION);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !run.log().contains(&"capture.cancel".to_owned()) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(200));
    while let Ok(inbound) = run.inbound.try_recv() {
        // Anything already in flight was queued before the cancel; nothing
        // after it may be a session.end.
        if let Inbound::Event(envelope) = &inbound {
            assert!(!matches!(envelope.event, VoiceEvent::SessionEnd { .. }));
        }
    }
}

#[test]
fn a_program_that_is_not_an_engine_fails_to_connect_with_its_exit_code() {
    let run = Run::with("normal", Path::new("/usr/bin/false"), quick());
    run.engine.begin(SESSION, None).unwrap();
    assert!(matches!(run.next(), Inbound::Lost(Failure::ExitedEarly(code)) if code != 0));
}

#[test]
fn an_oversized_frame_is_refused() {
    let run = Run::start("big-frame");
    run.engine.begin(SESSION, None).unwrap();
    assert_eq!(run.next(), Inbound::Lost(Failure::Violation("frame length out of range")));
    all_gone(&run.pids());
}

#[test]
fn the_engine_restarts_after_it_was_lost() {
    let run = Run::start("die");
    run.engine.begin(SESSION, None).unwrap();
    loop {
        if matches!(run.next(), Inbound::Lost(_)) {
            break;
        }
    }
    let first = run.pids();
    let _ = std::fs::remove_file(run.dir.join("pids"));
    run.engine.begin(SESSION, None).unwrap();
    let second = run.pids();
    assert_ne!(first, second, "a new process was launched");
}
