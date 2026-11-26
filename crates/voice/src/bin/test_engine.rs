//! A stand-in for the whisparrot engine, for tests of Hane's connection. It
//! takes the same command line (`--voice-engine --voice-handoff <handle>`),
//! reads the handoff from the inherited pipe, authenticates and then speaks
//! `voice/1` as scripted by `--mode`. Not part of the product.
//!
//! Modes: `normal`, `bad-token`, `bad-pid`, `bad-protocol`, `gap`, `die`,
//! `hang`, `worker`, `big-frame`. `--pidfile <path>` records the process id
//! (and, in `worker` mode, the id of a worker it starts); `--log <path>`
//! appends each control message received.

#[cfg(unix)]
fn main() {
    use hane_link::frame;
    use hane_voice::unix;
    use serde_json::{Value, json};
    use std::io::Write;
    use std::os::fd::FromRawFd;

    let args: Vec<String> = std::env::args().collect();
    let option = |name: &str| {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let mode = option("--mode").unwrap_or_else(|| "normal".into());
    let Some(handle) = option("--voice-handoff").and_then(|text| text.parse::<i32>().ok()) else {
        std::process::exit(2);
    };
    // SAFETY: the parent passed this inherited descriptor for us to read.
    let mut input = unsafe { std::fs::File::from_raw_fd(handle) };
    let Ok(body) = frame::read_frame(&mut input, frame::MAX_FRAME_BYTES) else {
        std::process::exit(2);
    };
    drop(input);
    let handoff: Value = serde_json::from_slice(&body).expect("handoff json");
    let pipe_name = handoff["pipeName"].as_str().expect("pipeName").to_owned();
    let token = handoff["token"].as_str().expect("token").to_owned();
    let parent = handoff["parentPid"].as_u64().expect("parentPid") as u32;

    if let Some(path) = option("--pidfile") {
        let mut text = std::process::id().to_string();
        if mode == "worker" {
            let worker = std::process::Command::new("sleep").arg("300").spawn().expect("worker");
            text.push_str(&format!(" {}", worker.id()));
            std::mem::forget(worker);
        }
        std::fs::write(path, text).expect("pidfile");
    }
    let log = option("--log");
    let note = |line: &str| {
        if let Some(path) = &log {
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
            writeln!(file, "{line}").unwrap();
        }
    };

    let mut conn = unix::connect(&pipe_name).unwrap_or_else(|_| std::process::exit(2));
    use hane_link::transport::Connection;
    use hane_voice::transport::VoiceConnection;
    if conn.peer_pid() != Some(parent) {
        std::process::exit(2);
    }
    let wait = std::time::Duration::from_secs(5);
    let write = |conn: &mut unix::UnixConnection, value: Value| {
        let bytes = value.to_string().into_bytes();
        let mut wire = (bytes.len() as u32).to_le_bytes().to_vec();
        wire.extend_from_slice(&bytes);
        let mut sent = 0;
        while sent < wire.len() {
            match conn.write_timeout(&wire[sent..], wait) {
                Ok(count) => sent += count,
                Err(_) => std::process::exit(0),
            }
        }
    };
    let read = |conn: &mut unix::UnixConnection| -> Option<Value> {
        let mut header = [0u8; 4];
        let mut filled = 0;
        while filled < 4 {
            match conn.read_timeout(&mut header[filled..], wait) {
                Ok(0) => return None,
                Ok(count) => filled += count,
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(_) => return None,
            }
        }
        let mut body = vec![0u8; u32::from_le_bytes(header) as usize];
        let mut filled = 0;
        while filled < body.len() {
            match conn.read_timeout(&mut body[filled..], wait) {
                Ok(0) => return None,
                Ok(count) => filled += count,
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(_) => return None,
            }
        }
        serde_json::from_slice(&body).ok()
    };

    let (hello_token, hello_pid, hello_protocol) = match mode.as_str() {
        "bad-token" => ("0".repeat(64), std::process::id(), "voice/1"),
        "bad-pid" => (token.clone(), std::process::id() + 1, "voice/1"),
        "bad-protocol" => (token.clone(), std::process::id(), "voice/2"),
        _ => (token.clone(), std::process::id(), "voice/1"),
    };
    write(
        &mut conn,
        json!({"type":"hello","protocol":hello_protocol,"token":hello_token,"pid":hello_pid}),
    );
    match read(&mut conn) {
        Some(reply) if reply == json!({"type":"hello.ok","protocol":"voice/1"}) => {}
        _ => std::process::exit(2),
    }

    let mut session = String::new();
    let event_no = std::cell::Cell::new(0u64);
    let emit = |conn: &mut unix::UnixConnection, session: &str, mut event: Value| {
        event_no.set(event_no.get() + 1);
        event["sessionId"] = json!(session);
        event["eventNo"] = json!(event_no.get());
        write(conn, event);
    };
    while let Some(message) = read(&mut conn) {
        let kind = message["type"].as_str().unwrap_or("").to_owned();
        note(&kind);
        match kind.as_str() {
            "capture.begin" => {
                session = message["sessionId"].as_str().unwrap().to_owned();
                if message.get("context").is_some() {
                    note("context");
                }
                event_no.set(0);
                match mode.as_str() {
                    "hang" | "worker" => {}
                    "die" => {
                        emit(&mut conn, &session, json!({"type":"session.start"}));
                        emit(&mut conn, &session, json!({"type":"segment.raw","seq":1,"status":"ok","text":"一つ目","polish":"pending"}));
                        std::process::exit(1);
                    }
                    "gap" => {
                        emit(&mut conn, &session, json!({"type":"session.start"}));
                        event_no.set(event_no.get() + 1); // skip a number
                        emit(&mut conn, &session, json!({"type":"segment.raw","seq":1,"status":"ok","text":"飛ぶ","polish":"off"}));
                    }
                    "big-frame" => {
                        conn.write_timeout(&(300_000u32).to_le_bytes(), wait).ok();
                    }
                    _ => {
                        emit(&mut conn, &session, json!({"type":"session.start"}));
                        emit(&mut conn, &session, json!({"type":"segment.raw","seq":1,"status":"ok","text":"えーと、今日はいい天気ですね。","polish":"pending"}));
                        emit(&mut conn, &session, json!({"type":"segment.raw","seq":2,"status":"ok","text":"つぎのしょうです","polish":"pending"}));
                        emit(&mut conn, &session, json!({"type":"segment.polished","seq":2,"status":"ok","text":"次の章です。"}));
                        emit(&mut conn, &session, json!({"type":"segment.polished","seq":1,"status":"ok","text":"今日はいい天気ですね。"}));
                        emit(&mut conn, &session, json!({"type":"segment.raw","seq":3,"status":"ok","text":"Hello, voice composition.","polish":"off"}));
                        emit(&mut conn, &session, json!({"type":"segment.raw","seq":4,"status":"silent"}));
                    }
                }
            }
            "capture.stop" => {
                emit(&mut conn, &session, json!({"type":"session.end","status":"completed","lastSeq":4}));
            }
            "capture.cancel" => {
                emit(&mut conn, &session, json!({"type":"session.end","status":"cancelled","lastSeq":0}));
            }
            _ => std::process::exit(0),
        }
    }
    note("closed");
}

#[cfg(not(unix))]
fn main() {
    eprintln!("the test engine runs on Unix only");
    std::process::exit(2);
}
