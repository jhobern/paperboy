use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

#[derive(Default)]
struct Api {
    sessions: HashMap<String, (String, bool)>,
    creates: usize,
    uploads: usize,
    results: usize,
    fail: bool,
}

struct Fixture {
    root: PathBuf,
    api: Arc<Mutex<Api>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn serve(mut socket: TcpStream, api: &Mutex<Api>) {
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut data = Vec::new();
    let end = loop {
        let mut buf = [0u8; 4096];
        let n = socket.read(&mut buf).unwrap();
        assert_ne!(n, 0);
        data.extend_from_slice(&buf[..n]);
        if let Some(end) = data.windows(4).position(|b| b == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&data[..end]).into_owned();
    if headers
        .to_ascii_lowercase()
        .contains("expect: 100-continue")
    {
        socket.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").unwrap();
    }
    let length = headers
        .lines()
        .filter_map(|s| s.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .map(|(_, v)| v.trim().parse::<usize>().unwrap())
        .unwrap_or(0);
    while data.len() < end + length {
        let mut buf = [0u8; 4096];
        let n = socket.read(&mut buf).unwrap();
        assert_ne!(n, 0);
        data.extend_from_slice(&buf[..n]);
    }
    let path = headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap();
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    let session = headers
        .lines()
        .filter_map(|s| s.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case("session_id"))
        .map(|(_, v)| v.trim())
        .unwrap_or("");
    let mut api = api.lock().unwrap();
    let (status, body) = match parts[1] {
        "create" => {
            api.creates += 1;
            let id = format!("s{}", api.creates);
            api.sessions.insert(id.clone(), (parts[0].into(), false));
            (200, format!(r#"{{"session_id":"{id}"}}"#))
        }
        "upload" => {
            assert!(data[end..].windows(8).any(|w| w == b"document"));
            let (target, uploaded) = api.sessions.get_mut(session).unwrap();
            assert_eq!(target, parts[0]);
            *uploaded = true;
            api.uploads += 1;
            (200, "{}".into())
        }
        "result" => {
            api.results += 1;
            if api.fail {
                (503, r#"{"message":"temporary outage"}"#.into())
            } else if api
                .sessions
                .get(session)
                .is_some_and(|(target, uploaded)| target == parts[0] && *uploaded)
            {
                (
                    200,
                    r#"{"dfa_overall_result":"Real","dfa_breakdown":{"ok":true}}"#.into(),
                )
            } else {
                (
                    400,
                    r#"{"status":"error","message":"Can't find document"}"#.into(),
                )
            }
        }
        _ => panic!("unexpected path {path}"),
    };
    drop(api);
    write!(socket, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("paperboy-cache-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("inputs")).unwrap();
        std::fs::write(root.join("inputs/one.jpg"), b"document one").unwrap();
        std::fs::write(root.join("inputs/two.jpg"), b"document two").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let api = Arc::new(Mutex::new(Api::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (worker_api, worker_stop) = (api.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((socket, _)) => serve(socket, &worker_api),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        for env in ["dev", "staging"] {
            std::fs::write(
                root.join(format!("{env}.vars")),
                format!("url=http://127.0.0.1:{port}/{env}\n"),
            )
            .unwrap();
        }
        std::fs::write(
            root.join("api.hurl"),
            r#"# create_session
POST {{url}}/create
HTTP 200
[Captures]
session_id: jsonpath "$.session_id"

# upload_document
POST {{url}}/upload
session_id: {{session_id}}
[Multipart]
document_id: 1
front_side_file: file,{{DFA_FILE}}; image/jpeg
HTTP 200

# dfa_result
POST {{url}}/result
session_id: {{session_id}}
HTTP 200
"#,
        )
        .unwrap();
        std::fs::write(root.join("test.trail"), r#"# collection: ./api.hurl
# output: json
PARALLEL(2) FOR TARGET IN ENVS BASELINE("dev"), COMPARISON("staging")
    PARALLEL(2) FOR DFA_FILE IN FILES "./inputs" MATCH "*.jpg"
        CACHE document_session
            FILES ("{{DFA_FILE}}")
            PERSIST (session_id)
            REQUEST create_session
            REQUEST upload_document
        END
        REPORT DFA_FILE AS Document
        TRY
            REPORT REQUEST dfa_result AS dfa WITH
                Overall: jsonpath "$.dfa_overall_result" TRUTH "Real"
                Breakdown: jsonpath "$.dfa_breakdown"
            END
        RECOVER RETRY(1) failure
            IF "{{failure.http_status}}" == "400" AND "{{failure.body.message}}" == "Can't find document"
                CACHE REFRESH document_session
            ELSE
                RETHROW
            END
        END
    END
END
"#).unwrap();
        Self {
            root,
            api,
            stop,
            thread: Some(thread),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_paperboy"));
        command
            .current_dir(&self.root)
            .env("PAPERBOY_CACHE_DIR", self.root.join("cache"))
            .env("PAPERBOY_STATE_DIR", self.root.join("state-config"));
        command
    }

    fn run(&self, name: &str) -> std::process::Output {
        self.command()
            .args([
                "-r",
                "test.trail",
                "-e",
                "dev.vars",
                "-e",
                "staging.vars",
                "--run-state",
                &format!("state-{name}"),
                "--case-key",
                "DFA_FILE",
                "-o",
                &format!("{name}.json"),
            ])
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn cache_cli_comparison_cold_warm_deleted_and_unrelated_failure() {
    let fixture = Fixture::new();
    for label in ["cold", "warm"] {
        let output = fixture.run(label);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let api = fixture.api.lock().unwrap();
        assert_eq!((api.creates, api.uploads), (4, 4));
    }
    fixture.api.lock().unwrap().sessions.clear();
    let output = fixture.run("repaired");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    {
        let api = fixture.api.lock().unwrap();
        assert_eq!((api.creates, api.uploads, api.results), (8, 8, 16));
    }
    let output = fixture
        .command()
        .args(["--run-state", "state-repaired", "--state-action", "inspect"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let inspection: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(inspection["errors_count"], 0);
    fixture.api.lock().unwrap().fail = true;
    let output = fixture.run("outage");
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fixture.api.lock().unwrap().creates,
        8,
        "an outage must not reupload documents"
    );
}
