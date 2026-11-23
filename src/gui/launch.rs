use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::i18n::{Language, Strings};

pub fn start(foreground: bool, ready_file: Option<PathBuf>) -> Result<(), String> {
    if foreground {
        super::run(ready_file)
    } else {
        launch_detached().map_err(|e| e.to_string())
    }
}

fn launch_detached() -> io::Result<()> {
    let strings = Strings::for_language(&Language::English);
    let log_path = crate::persistence::state_path()
        .ok_or_else(|| io::Error::other(strings.gui_log_unavailable))?
        .with_file_name("gui.log");
    std::fs::create_dir_all(log_path.parent().expect("state file has a parent"))?;
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let log = options.open(&log_path)?;
    let log_start = log.metadata()?.len();
    let startup = tempfile::Builder::new().prefix("paperboy-gui-").tempdir()?;
    let ready = startup.path().join("ready");
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(std::env::args_os().skip(1))
        .arg("--foreground")
        .arg("--gui-ready-file")
        .arg(&ready)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    detach(&mut command);
    let mut child = command.spawn()?;
    if let Err(error) = wait_for_startup(&mut child, &ready) {
        if child.try_wait()?.is_none() {
            child.kill()?;
            child.wait()?;
        }
        let mut log = File::open(&log_path)?;
        log.seek(SeekFrom::Start(log_start))?;
        let mut output = Vec::new();
        log.take(64 * 1024).read_to_end(&mut output)?;
        return Err(io::Error::other(format!(
            "{error}\n{}: {}\n{}",
            strings.gui_log_label,
            log_path.display(),
            String::from_utf8_lossy(&output).trim_end()
        )));
    }
    Ok(())
}

fn wait_for_startup(child: &mut Child, ready: &Path) -> io::Result<()> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(
                Strings::for_language(&Language::English)
                    .gui_launch_exited
                    .replace("{status}", &status.to_string()),
            ));
        }
        if ready.try_exists()? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // Only async-signal-safe calls are permitted between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};

    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(mode: &str, ready: &Path) -> Child {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "gui::launch::tests::startup_child",
                "--nocapture",
            ])
            .env("PAPERBOY_TEST_GUI_STARTUP", mode)
            .env("PAPERBOY_TEST_GUI_READY", ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach(&mut command);
        command.spawn().unwrap()
    }

    #[test]
    fn startup_child() {
        let Ok(mode) = std::env::var("PAPERBOY_TEST_GUI_STARTUP") else {
            return;
        };
        if mode == "exit" {
            std::process::exit(7);
        }
        if mode == "exit-success" {
            return;
        }
        // The process must still be alive when its launcher returns.
        File::create_new(std::env::var_os("PAPERBOY_TEST_GUI_READY").unwrap()).unwrap();
        std::thread::sleep(Duration::from_secs(20));
    }

    #[test]
    fn startup_returns_while_the_detached_child_is_still_running() {
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("ready");
        let mut child = child("ready", &ready);
        let result = wait_for_startup(&mut child, &ready);
        let running = child.try_wait().unwrap().is_none();
        #[cfg(unix)]
        let session = unsafe { libc::getsid(child.id() as libc::pid_t) };
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert!(running);
        #[cfg(unix)]
        assert_eq!(session, child.id() as libc::pid_t);
    }

    #[test]
    fn exiting_without_readiness_is_an_error_even_with_exit_code_zero() {
        for mode in ["exit", "exit-success"] {
            let directory = tempfile::tempdir().unwrap();
            let mut child = child(mode, &directory.path().join("ready"));
            assert!(wait_for_startup(&mut child, &directory.path().join("ready")).is_err());
            assert!(child.try_wait().unwrap().is_some());
        }
    }
}
