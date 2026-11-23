use super::*;
use std::collections::HashSet;

const TRAIL: &str = r#"
CACHE document_session
    FILES ("{{DFA_FILE}}")
    PERSIST (session_id)
    REQUEST create_session
    REQUEST upload_document
END
TRY
    REPORT REQUEST dfa_result AS result WITH
        Status: HttpStatus
        Overall: jsonpath "$.result"
        Latency: Time
    END
RECOVER RETRY(1) failure
    IF "{{failure.http_status}}" == "400" AND ("{{failure.body.message}}" == "Can't find document" OR "{{failure.headers.x-session-state}}" == "expired")
        CACHE REFRESH document_session
    ELSE
        RETHROW
    END
END
"#;

#[derive(Default)]
struct State {
    created: usize,
    uploaded: usize,
    results: usize,
    valid: HashSet<String>,
    reject_upload: bool,
    reject_result: bool,
    header_only: bool,
}

#[derive(Default)]
struct Server(Mutex<State>);

impl EntryRunner for Server {
    fn run(&self, entry: &HurlEntry, vars: &HashMap<String, String>) -> RunOutput {
        let mut state = self.0.lock().unwrap();
        let mut eo = EntryOutcome {
            ok: true,
            status: 200,
            duration_ms: 7,
            ..Default::default()
        };
        match entry.title.as_str() {
            "create_session" => {
                state.created += 1;
                eo.captures
                    .push(("session_id".into(), format!("session-{}", state.created)));
            }
            "upload_document" => {
                state.uploaded += 1;
                assert!(
                    vars.get("session_id")
                        .is_some_and(|v| v.starts_with("session-"))
                );
                if state.reject_upload {
                    eo.ok = false;
                    eo.status = 500;
                } else {
                    state.valid.insert(vars["session_id"].clone());
                }
            }
            "dfa_result" => {
                state.results += 1;
                eo.raw_body = r#"{"result":"Real"}"#.into();
                if state.reject_result || !state.valid.contains(&vars["session_id"]) {
                    eo.ok = false;
                    eo.status = 400;
                    eo.raw_body = r#"{"status":"error","message":"Can't find document"}"#.into();
                    if state.header_only {
                        eo.raw_body = "not JSON".into();
                        eo.headers
                            .push(("X-Session-State".into(), "expired".into()));
                    }
                }
            }
            _ => panic!("unexpected request {}", entry.title),
        }
        eo.body = eo.raw_body.clone();
        eo.error = (!eo.ok).then(|| format!("HTTP {}", eo.status));
        RunOutput {
            error: eo.error.clone(),
            entries: vec![eo],
            generated: HashMap::new(),
        }
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("paperboy-cache-runtime-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("document.jpg"), b"document one").unwrap();
        std::fs::write(path.join("test.trail"), TRAIL).unwrap();
        Self(path)
    }

    fn run(
        &self,
        source: &str,
        server: &dyn EntryRunner,
        environment: &str,
    ) -> (Vec<ReportRow>, Vec<String>) {
        self.run_at(source, server, environment, "test.trail")
    }

    fn run_at(
        &self,
        source: &str,
        server: &dyn EntryRunner,
        environment: &str,
        script: &str,
    ) -> (Vec<ReportRow>, Vec<String>) {
        let flow = super::super::parser::parse_flow(source).unwrap();
        let entries: Vec<_> = ["create_session", "upload_document", "dfa_result"]
            .iter()
            .map(|name| HurlEntry {
                title: name.to_string(),
                method: "POST".into(),
                url: format!("http://server/{name}"),
                captures: if *name == "create_session" {
                    vec![("session_id".into(), r#"jsonpath "$.session_id""#.into())]
                } else {
                    vec![]
                },
                ..Default::default()
            })
            .collect();
        let ctx = RunContext {
            entries: &entries,
            helpers: &[],
            base_vars: HashMap::from([("url".into(), environment.into())]),
            named_envs: HashMap::new(),
            root: Some(self.0.clone()),
            runner: server,
            strings: crate::i18n::Strings::english(),
            params: Default::default(),
            sink: None,
            shuffle: None,
            cancel: None,
        };
        let mut exec = Exec::new(&ctx);
        exec.source_path = Some(self.0.join(script));
        exec.cache_root = Some(self.0.join("cache"));
        exec.recovery_refs = Arc::new(template_refs(&flow.to_text()));
        exec.set_var(
            "DFA_FILE",
            self.0.join("document.jpg").display().to_string(),
        );
        let rows = exec.exec_block(&flow.nodes);
        (rows, exec.errors)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn cache_cold_warm_and_deleted_document_recovery() {
    let f = Fixture::new();
    let server = Server::default();
    for _ in 0..2 {
        let (rows, errors) = f.run(TRAIL, &server, "dev");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cells["result.Overall"], "Real");
    }
    {
        let mut s = server.0.lock().unwrap();
        assert_eq!((s.created, s.uploaded, s.results), (1, 1, 2));
        s.valid.clear();
    }
    let (rows, errors) = f.run(TRAIL, &server, "dev");
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].cells["result.Status"], "200");
    assert_eq!(rows[0].cells["result.Latency"], "7");
    let s = server.0.lock().unwrap();
    assert_eq!((s.created, s.uploaded, s.results), (2, 2, 4));
}

#[test]
fn cache_environment_document_and_workflow_isolation() {
    let f = Fixture::new();
    let server = Server::default();
    assert!(f.run(TRAIL, &server, "dev").1.is_empty());
    assert!(f.run(TRAIL, &server, "staging").1.is_empty());
    assert!(
        f.run(
            &TRAIL.replace("document_session", "other_session"),
            &server,
            "dev"
        )
        .1
        .is_empty()
    );
    std::fs::write(f.0.join("document.jpg"), b"document two").unwrap();
    assert!(f.run(TRAIL, &server, "dev").1.is_empty());
    assert_eq!(server.0.lock().unwrap().created, 4);
}

#[test]
fn cache_retry_is_bounded_and_failed_upload_is_not_persisted() {
    let f = Fixture::new();
    let server = Server::default();
    server.0.lock().unwrap().reject_upload = true;
    assert!(!f.run(TRAIL, &server, "dev").1.is_empty());
    assert_eq!(server.0.lock().unwrap().results, 0);
    server.0.lock().unwrap().reject_upload = false;
    assert!(f.run(TRAIL, &server, "dev").1.is_empty());
    {
        let mut s = server.0.lock().unwrap();
        s.reject_result = true;
        s.header_only = true;
    }
    let (rows, errors) = f.run(TRAIL, &server, "dev");
    assert!(!errors.is_empty());
    assert_eq!(rows.len(), 1);
    let s = server.0.lock().unwrap();
    assert_eq!((s.created, s.uploaded, s.results), (3, 3, 3));
}

#[test]
fn cache_parallel_duplicate_documents_prepare_once() {
    let f = Fixture::new();
    let server = Server::default();
    let source = format!("PARALLEL(8) FOR copy IN [\"1\", \"2\", \"3\", \"4\"]\n{TRAIL}\nEND\n");
    assert!(f.run(&source, &server, "dev").1.is_empty());
    {
        let mut s = server.0.lock().unwrap();
        assert_eq!(s.created, 1);
        s.valid.clear();
    }
    assert!(f.run(&source, &server, "dev").1.is_empty());
    assert_eq!(server.0.lock().unwrap().created, 2);
}

#[test]
fn cache_missing_optional_fields_do_not_trigger_repair() {
    let f = Fixture::new();
    let server = Server::default();
    server.0.lock().unwrap().reject_result = true;
    let source = TRAIL
        .replace("\"Can't find document\"", "\"some other condition\"")
        .replace("failure.body.message", "failure.body.missing");
    let (_, errors) = f.run(&source, &server, "dev");
    assert!(!errors.is_empty());
    assert!(!errors.iter().any(|e| e.contains("undefined")));
    assert_eq!(server.0.lock().unwrap().created, 1);
}

#[test]
fn cache_offline_does_not_create_storage_or_read_inputs() {
    let f = Fixture::new();
    std::fs::remove_file(f.0.join("document.jpg")).unwrap();
    let (_, errors) = f.run(TRAIL, &DryRunner, "dev");
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!f.0.join("cache").exists());
}

#[test]
fn cache_explicit_invalidation_does_not_recreate() {
    let f = Fixture::new();
    let server = Server::default();
    let source = format!("{TRAIL}\nCACHE INVALIDATE document_session\n");
    assert!(f.run(&source, &server, "dev").1.is_empty());
    assert_eq!(server.0.lock().unwrap().created, 1);
    assert!(f.run(TRAIL, &server, "dev").1.is_empty());
    assert_eq!(server.0.lock().unwrap().created, 2);
}

#[test]
fn cache_fileless_preparation_is_not_document_specific() {
    let f = Fixture::new();
    let server = Server::default();
    let source = TRAIL.replace("    FILES (\"{{DFA_FILE}}\")\n", "");
    assert!(f.run(&source, &server, "dev").1.is_empty());
    std::fs::write(f.0.join("document.jpg"), b"changed").unwrap();
    assert!(f.run(&source, &server, "dev").1.is_empty());
    assert_eq!(server.0.lock().unwrap().created, 1);
}

#[test]
fn cache_auth_refresh_reaches_other_iterations_without_leaking_unpersisted_values() {
    #[derive(Default)]
    struct Auth(Mutex<(usize, Vec<String>)>);
    impl EntryRunner for Auth {
        fn run(&self, request: &HurlEntry, vars: &HashMap<String, String>) -> RunOutput {
            let mut state = self.0.lock().unwrap();
            let mut eo = EntryOutcome {
                ok: true,
                status: 200,
                ..Default::default()
            };
            if request.title == "create_session" {
                state.0 += 1;
                eo.captures = vec![
                    ("session_id".into(), format!("token{}", state.0)),
                    (
                        "private_intermediate".into(),
                        "must not escape preparation".into(),
                    ),
                ];
            } else {
                assert!(!vars.contains_key("private_intermediate"));
                let token = vars["session_id"].clone();
                state.1.push(token.clone());
                if token == "token1" {
                    eo.ok = false;
                    eo.status = 401;
                    eo.error = Some("expired token".into());
                }
            }
            RunOutput {
                error: eo.error.clone(),
                entries: vec![eo],
                generated: HashMap::new(),
            }
        }
    }
    let f = Fixture::new();
    let server = Auth::default();
    let source = r#"
CACHE authentication
    PERSIST (session_id)
    REQUEST create_session
END
FOR item IN ["a", "b"]
    TRY
        REPORT REQUEST dfa_result
    RECOVER RETRY(1) failure
        IF "{{failure.http_status}}" == "401"
            CACHE REFRESH authentication
        ELSE
            RETHROW
        END
    END
END
"#;
    let (rows, errors) = f.run(source, &server, "dev");
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(rows.len(), 2);
    let state = server.0.lock().unwrap();
    assert_eq!(state.0, 2);
    assert_eq!(state.1, ["token1", "token2", "token2"]);
}

#[test]
fn cache_retry_does_not_leak_failed_attempt_captures() {
    struct FailsOnce(Mutex<usize>);
    impl EntryRunner for FailsOnce {
        fn run(&self, _: &HurlEntry, vars: &HashMap<String, String>) -> RunOutput {
            let mut attempts = self.0.lock().unwrap();
            *attempts += 1;
            let failed = *attempts == 1;
            if !failed {
                assert!(!vars.contains_key("failed_attempt"));
                assert_eq!(vars["repaired"], "yes");
            }
            let error = failed.then(|| "failure".to_string());
            RunOutput {
                error: error.clone(),
                generated: HashMap::new(),
                entries: vec![EntryOutcome {
                    ok: !failed,
                    status: if failed { 400 } else { 200 },
                    error,
                    captures: if failed {
                        vec![("failed_attempt".into(), "stale".into())]
                    } else {
                        vec![]
                    },
                    ..Default::default()
                }],
            }
        }
    }
    let f = Fixture::new();
    let source = "TRY\nREPORT REQUEST dfa_result\nRECOVER RETRY(1) failure\nrepaired=yes\nEND\n";
    let (rows, errors) = f.run(source, &FailsOnce(Mutex::new(0)), "dev");
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(rows.len(), 1);
}

#[test]
fn cache_namespace_opts_into_cross_script_sharing() {
    let f = Fixture::new();
    let server = Server::default();
    std::fs::write(f.0.join("other.trail"), TRAIL).unwrap();
    assert!(f.run(TRAIL, &server, "dev").1.is_empty());
    assert!(f.run_at(TRAIL, &server, "dev", "other.trail").1.is_empty());
    assert_eq!(server.0.lock().unwrap().created, 2);
    let shared = TRAIL.replace(
        "CACHE document_session\n",
        "CACHE document_session\n    NAMESPACE \"shared-documents\"\n",
    );
    assert!(f.run(&shared, &server, "dev").1.is_empty());
    assert!(
        f.run_at(&shared, &server, "dev", "other.trail")
            .1
            .is_empty()
    );
    assert_eq!(server.0.lock().unwrap().created, 3);
}

#[test]
fn cache_content_identity_survives_moves_but_keys_and_preparation_invalidate() {
    let f = Fixture::new();
    let server = Server::default();
    assert!(f.run(TRAIL, &server, "dev").1.is_empty());
    std::fs::copy(f.0.join("document.jpg"), f.0.join("moved.jpg")).unwrap();
    let moved = format!("DFA_FILE=moved.jpg\n{TRAIL}");
    assert!(f.run(&moved, &server, "dev").1.is_empty());
    assert_eq!(server.0.lock().unwrap().created, 1);
    let keyed = TRAIL.replace(
        "CACHE document_session\n",
        "CACHE document_session\n    KEY (\"type-one\")\n",
    );
    assert!(f.run(&keyed, &server, "dev").1.is_empty());
    assert!(
        f.run(&keyed.replace("type-one", "type-two"), &server, "dev")
            .1
            .is_empty()
    );
    let changed = TRAIL.replace(
        "    REQUEST upload_document",
        "    upload_version=2\n    REQUEST upload_document",
    );
    assert!(f.run(&changed, &server, "dev").1.is_empty());
    assert_eq!(server.0.lock().unwrap().created, 4);
}

#[test]
fn cache_failed_refresh_leaves_no_usable_entry() {
    let f = Fixture::new();
    let server = Server::default();
    assert!(f.run(TRAIL, &server, "dev").1.is_empty());
    {
        let mut state = server.0.lock().unwrap();
        state.valid.clear();
        state.reject_upload = true;
    }
    assert!(!f.run(TRAIL, &server, "dev").1.is_empty());
    server.0.lock().unwrap().reject_upload = false;
    assert!(f.run(TRAIL, &server, "dev").1.is_empty());
    let state = server.0.lock().unwrap();
    assert_eq!((state.created, state.uploaded, state.results), (3, 3, 3));
}

#[test]
fn cache_explicit_retry_count_bounds_recovery_entries() {
    let f = Fixture::new();
    let server = Server::default();
    server.0.lock().unwrap().reject_result = true;
    let (rows, errors) = f.run(&TRAIL.replace("RETRY(1)", "RETRY(2)"), &server, "dev");
    assert!(!errors.is_empty());
    assert_eq!(rows.len(), 1);
    let state = server.0.lock().unwrap();
    assert_eq!((state.created, state.uploaded, state.results), (3, 3, 3));
}
