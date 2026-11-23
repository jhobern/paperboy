#![cfg(all(feature = "gui", target_os = "linux"))]

use std::process::Command;

fn without_display(directory: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_paperboy"));
    command
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env("XDG_RUNTIME_DIR", directory)
        .env("XDG_DATA_HOME", directory.join("data"))
        .env("PAPERBOY_STATE_DIR", directory.join("state"))
        .arg("--gui");
    command
}

#[test]
fn detached_startup_failure_reaches_the_terminal_and_log() {
    let directory = tempfile::tempdir().unwrap();
    let output = without_display(directory.path()).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    let log_path = directory.path().join("state/gui.log");
    let log = std::fs::read_to_string(&log_path).unwrap();
    assert!(!log.trim().is_empty());
    assert!(
        error.contains("GUI exited before startup completed"),
        "{error}"
    );
    assert!(error.contains(&*log_path.to_string_lossy()), "{error}");
    assert!(error.contains(log.trim()), "{error}");

    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(log_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn foreground_failure_goes_directly_to_stderr_without_a_log() {
    let directory = tempfile::tempdir().unwrap();
    let output = without_display(directory.path())
        .arg("--foreground")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(!error.trim().is_empty());
    assert!(
        !error.contains("GUI exited before startup completed"),
        "{error}"
    );
    assert!(!directory.path().join("state/gui.log").exists());
}
