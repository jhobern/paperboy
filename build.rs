//! Pre-flight check for the system tools and libraries PaperBoy's dependency
//! tree needs and Cargo cannot supply for itself.
//!
//! Everything Cargo *can* fetch is already pinned in `Cargo.lock`. What is left
//! is a short list of things that must already be on the machine, each of which
//! otherwise announces itself as a panic inside some crate the user never asked
//! for, several levels down the tree and several minutes into a build:
//!
//! * **libxml2** (+ `pkg-config` to find it) — `hurl` and `hurl_core` depend
//!   unconditionally on the `libxml` crate, because Hurl's XPath support in
//!   `[Captures]`/`[Asserts]` *is* libxml2, and `libxml` binds the system copy
//!   rather than vendoring it. Fails as
//!   `Couldn't find libxml2 via pkg-config`.
//! * **A C compiler, `perl` and `make`** — PaperBoy asks `curl-sys` for
//!   `static-curl`/`static-ssl` so that libcurl and OpenSSL are compiled from
//!   vendored sources instead of needing system `-dev` packages. That trade buys
//!   away two system libraries at the price of a build toolchain: `openssl-src`
//!   literally runs `perl Configure` and then `make`.
//! * **libclang** — `libxml` generates its FFI bindings with `bindgen`, whose
//!   `runtime` feature `dlopen`s libclang during the build. Fails as
//!   `Unable to find libclang`.
//!
//! Audited rather than assumed: the rest of the tree either vendors its C or
//! loads it lazily. `libz-sys` falls back to compiling its bundled zlib when
//! pkg-config finds nothing, and the `gui` feature adds no *build*-time system
//! requirement at all — `wayland-sys` is built with `dlopen`, so its build
//! script links nothing, `khronos-egl` is `dynamic`+`libloading`, and `x11-dl`
//! never fails on a missing probe. Those X11/Wayland libraries are needed to
//! *run* the GUI, not to build it, so they are out of this script's scope.
//!
//! **Why it stops the build rather than only warning.** A `cargo::warning` alone
//! gets lost: measured on a real failing cold build, the warning landed at line
//! 156 of 268, with ~65 `Compiling …` lines between it and the error, and Cargo
//! does not replay build-script warnings at the end. Cargo runs a dependency's
//! build script and the root package's *concurrently*, so this script cannot be
//! scheduled ahead of `libxml`'s to pre-empt it — but by failing, it does become
//! the last thing on screen (Cargo echoes a failing build script's own stdout
//! inside its error report), it puts PaperBoy's name on the error, and it aborts
//! before the remaining crates compile, so nobody waits minutes to be told this.
//! The build was going to fail regardless; the only question was how usefully.
//!
//! Two things it deliberately does not do, each settled by experiment:
//!
//! * **It cannot ask anything.** A build script's stdin, stdout *and* stderr are
//!   all pipes (`is_terminal()` is false for all three), so a prompt would never
//!   reach a human and there would be nothing to read a reply from.
//! * **It never installs anything.** Running a package manager from a build
//!   script is a supply-chain trap — `cargo install` would silently mutate the
//!   system — and `sudo` would deadlock with no TTY to take a password. So the
//!   command is printed for a human to run, and this script only ever reads.
//!
//! Because stopping a build is a strong move, it is hedged three ways: it only
//! stops on checks that are certain (a filesystem or `pkg-config` answer, never
//! the heuristic libclang search), it downgrades everything to a warning when
//! the probe might not match the one that matters (see `probe_is_authoritative`),
//! and `PAPERBOY_SKIP_DEP_CHECK=1` turns it off outright — a way past a false
//! positive that doesn't require waiting for a new release.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Set this to bypass the check entirely, in case it is ever wrong about a
/// machine that would in fact have built fine.
const SKIP_VAR: &str = "PAPERBOY_SKIP_DEP_CHECK";

/// One thing that has to be on the machine and isn't.
struct Missing {
    /// What to call it, in the user's terms rather than the crate's.
    name: &'static str,
    /// How this script knows — the specific negative result, so the user can
    /// argue with it if it's wrong.
    evidence: String,
    /// Which part of the build wants it, and why PaperBoy can't avoid needing
    /// it. Without this the advice reads as arbitrary.
    needed_by: &'static str,
}

fn main() {
    // Deliberately *not* an unconditional re-run: a build script that always
    // re-runs marks the crate dirty on every `cargo build`, and recompiling
    // PaperBoy on every invocation is a far worse tax than the one stale
    // warning this risks. These are the inputs that can change the answer.
    println!("cargo::rerun-if-changed=build.rs");
    for var in [
        SKIP_VAR,
        "LIBXML2",
        "PKG_CONFIG",
        "PKG_CONFIG_PATH",
        "PKG_CONFIG_LIBDIR",
        "PKG_CONFIG_SYSROOT_DIR",
        "LIBCLANG_PATH",
        "CC",
        "PERL",
        "OPENSSL_SRC_PERL",
        "MAKE",
    ] {
        println!("cargo::rerun-if-env-changed={var}");
    }

    if std::env::var_os(SKIP_VAR).is_some() {
        return;
    }

    // Certain answers: a `pkg-config` verdict, or a file that is or isn't on
    // PATH. These may stop the build.
    let certain: Vec<Missing> = [
        check_libxml2(),
        check_compiler(),
        check_perl(),
        check_make(),
    ]
    .into_iter()
    .flatten()
    .collect();

    // A guess: replicating clang-sys's search well enough to *block* a build
    // isn't realistic, so a negative here only ever advises.
    let uncertain: Vec<Missing> = check_libclang().into_iter().collect();

    if certain.is_empty() && uncertain.is_empty() {
        return;
    }

    let hint = install_hint();
    let fatal = !certain.is_empty() && probe_is_authoritative();

    // The warning block is what a user sees if they scroll back; the panic is
    // what they see at the end. Emitting both costs nothing.
    report(&certain, &uncertain, &hint);

    if fatal {
        panic!("{}", failure_message(&certain, &uncertain, &hint));
    }

    if !certain.is_empty() {
        warn("(Continuing anyway: this check can't be certain on this machine.)");
    }
}

// ---------------------------------------------------------------------------
// The checks
// ---------------------------------------------------------------------------

fn check_libxml2() -> Option<Missing> {
    const NEEDED_BY: &str = "`hurl`, which PaperBoy uses to run requests. Hurl's XPath \
        asserts and captures are libxml2, and the `libxml` crate binds the system \
        copy rather than vendoring it, so Cargo cannot download it for you.";

    // `libxml`'s build script checks LIBXML2 first and, when it is set, skips
    // pkg-config and bindgen entirely in favour of its pre-generated bindings.
    // Someone who has set it has already answered this question.
    if std::env::var_os("LIBXML2").is_some() {
        return None;
    }

    // On windows-msvc `libxml` asks vcpkg rather than pkg-config, and vcpkg's
    // layout is involved enough that a bad guess here would be worse than
    // silence. Leave that path to the README. Note this reads the *target*
    // triple's configuration, not the host's: it is the target's build of
    // `libxml` that decides which probe runs.
    if target_cfg("CARGO_CFG_TARGET_FAMILY").contains("windows")
        && target_cfg("CARGO_CFG_TARGET_ENV") == "msvc"
    {
        return None;
    }

    // Honour the same override pkg-config-rs does, so a user who has pointed
    // Cargo at a specific pkg-config gets probed with that one.
    let pkg_config = std::env::var("PKG_CONFIG").unwrap_or_else(|_| "pkg-config".to_string());

    match Command::new(&pkg_config)
        .args(["--exists", "libxml-2.0"])
        .output()
    {
        Ok(out) if out.status.success() => None,
        Ok(_) => Some(Missing {
            name: "libxml2",
            evidence: format!("`{pkg_config} --exists libxml-2.0` says it isn't installed"),
            needed_by: NEEDED_BY,
        }),
        // Any failure to *launch* pkg-config — missing, or not executable —
        // lands the user in the same place, so it gets the same advice.
        Err(_) => Some(Missing {
            name: "libxml2",
            evidence: format!("`{pkg_config}`, which finds it, is not on PATH"),
            needed_by: NEEDED_BY,
        }),
    }
}

fn check_compiler() -> Option<Missing> {
    if std::env::var_os("CC").is_some() {
        return None;
    }
    // cc-rs picks a platform default; any of these being present means the
    // vendored C builds have something to compile with.
    if ["cc", "gcc", "clang"].iter().any(|bin| has(bin)) {
        return None;
    }
    Some(Missing {
        name: "a C compiler",
        evidence: "none of `cc`, `gcc` or `clang` is on PATH, and CC is unset".to_string(),
        needed_by: "the vendored libcurl, OpenSSL and zlib builds. PaperBoy enables \
            curl-sys's `static-curl`/`static-ssl` so those are compiled from source \
            here, which is what spares you needing them as system packages.",
    })
}

fn check_perl() -> Option<Missing> {
    // openssl-src reads OPENSSL_SRC_PERL, then PERL, then falls back to `perl`.
    if std::env::var_os("OPENSSL_SRC_PERL").is_some() || std::env::var_os("PERL").is_some() {
        return None;
    }
    if has("perl") {
        return None;
    }
    Some(Missing {
        name: "perl",
        evidence: "`perl` is not on PATH, and neither PERL nor OPENSSL_SRC_PERL is set".to_string(),
        needed_by: "the vendored OpenSSL build: `openssl-src` configures OpenSSL by \
            running `perl Configure`.",
    })
}

fn check_make() -> Option<Missing> {
    if std::env::var_os("MAKE").is_some() {
        return None;
    }
    if has("make") || has("gmake") {
        return None;
    }
    Some(Missing {
        name: "make",
        evidence: "neither `make` nor `gmake` is on PATH".to_string(),
        needed_by: "the vendored OpenSSL build, which drives OpenSSL's own makefile.",
    })
}

/// A best-effort look for libclang, mirroring the *obvious* parts of the search
/// `clang-sys` does at build time. It is deliberately generous: this only ever
/// produces advice, so a false negative (quietly missing a libclang that is in
/// fact present) is a much cheaper mistake than a false positive would be.
fn check_libclang() -> Option<Missing> {
    if std::env::var_os("LIBCLANG_PATH").is_some() {
        return None;
    }

    let mut dirs: Vec<PathBuf> = vec![
        // Linux, the usual distro locations.
        PathBuf::from("/usr/lib"),
        PathBuf::from("/usr/lib64"),
        PathBuf::from("/usr/local/lib"),
        PathBuf::from("/usr/lib/x86_64-linux-gnu"),
        PathBuf::from("/usr/lib/aarch64-linux-gnu"),
        // macOS: the Command Line Tools ship libclang.dylib, which is why a Mac
        // with `xcode-select --install` needs nothing further here.
        PathBuf::from("/Library/Developer/CommandLineTools/usr/lib"),
        PathBuf::from(
            "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib",
        ),
        PathBuf::from("/opt/homebrew/opt/llvm/lib"),
        PathBuf::from("/usr/local/opt/llvm/lib"),
    ];

    // Versioned LLVM trees: /usr/lib/llvm-18/lib and friends.
    if let Ok(entries) = std::fs::read_dir("/usr/lib") {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("llvm") {
                dirs.push(entry.path().join("lib"));
            }
        }
    }

    // Whatever the installed LLVM says about itself, if it can be asked.
    if let Ok(out) = Command::new("llvm-config").arg("--libdir").output() {
        if out.status.success() {
            let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !dir.is_empty() {
                dirs.push(PathBuf::from(dir));
            }
        }
    }

    if dirs.iter().any(|dir| contains_libclang(dir)) {
        return None;
    }

    Some(Missing {
        name: "libclang",
        evidence: "no libclang shared library found in the usual places".to_string(),
        needed_by: "`bindgen`, which generates `libxml`'s bindings and loads libclang \
            while building. Without it the build fails with \"Unable to find libclang\".",
    })
}

fn contains_libclang(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name.starts_with("libclang") && (name.contains(".so") || name.contains(".dylib"))
    })
}

/// May a failed check be trusted enough to stop the build?
///
/// Only when this script asked exactly the question the real build will ask. It
/// does not, in two cases:
///
/// * **Cross-compilation.** pkg-config-rs consults `PKG_CONFIG_PATH_<target>`,
///   `HOST_PKG_CONFIG_PATH` and friends ahead of the plain variables, so a
///   cross-build can have a perfectly good target libxml2 that a plain
///   `pkg-config --exists` on the host cannot see.
/// * **Target-suffixed or host-prefixed pkg-config settings.** Same reason, even
///   when host and target happen to match: those variables redirect the real
///   probe somewhere this one didn't look.
fn probe_is_authoritative() -> bool {
    let host = std::env::var("HOST").unwrap_or_default();
    let target = std::env::var("TARGET").unwrap_or_default();
    if host.is_empty() || target.is_empty() || host != target {
        return false;
    }

    for base in [
        "PKG_CONFIG",
        "PKG_CONFIG_PATH",
        "PKG_CONFIG_LIBDIR",
        "PKG_CONFIG_SYSROOT_DIR",
    ] {
        // The three shapes pkg-config-rs looks for ahead of the plain name.
        let candidates = [
            format!("{base}_{target}"),
            format!("{base}_{}", target.replace('-', "_")),
            format!("HOST_{base}"),
        ];
        if candidates.iter().any(|var| std::env::var_os(var).is_some()) {
            return false;
        }
    }

    true
}

/// Read one of Cargo's `CARGO_CFG_*` variables, which describe the **target**.
fn target_cfg(var: &str) -> String {
    std::env::var(var).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Saying so
// ---------------------------------------------------------------------------

/// The warning block. Kept short and rule-delimited on purpose: Cargo schedules
/// this script in the middle of the build, among the `Compiling …` lines, so it
/// has to be something the eye can catch while scrolling back.
fn report(certain: &[Missing], uncertain: &[Missing], hint: &[String]) {
    warn("──────────────────────────────────────────────────────────────");
    warn("PaperBoy needs some build dependencies that aren't installed:");
    for item in certain {
        warn(&format!("  · {} — {}", item.name, item.evidence));
    }
    for item in uncertain {
        warn(&format!("  · {} — {} (unsure)", item.name, item.evidence));
    }
    warn("  Install them with:");
    for line in hint {
        warn(&format!("      {line}"));
    }
    warn("──────────────────────────────────────────────────────────────");
}

/// The panic text. This is the *last* thing the user sees, so it repeats the
/// command rather than referring back to a warning that has scrolled away.
fn failure_message(certain: &[Missing], uncertain: &[Missing], hint: &[String]) -> String {
    let subject = if certain.len() == 1 {
        "a required build dependency is missing"
    } else {
        "required build dependencies are missing"
    };
    let mut message = format!("PaperBoy can't be built here: {subject}.\n\n");

    for item in certain {
        message.push_str(&format!(
            "  {} is a required build dependency, and it isn't installed.\n",
            item.name
        ));
        message.push_str(&format!("      How we know: {}.\n", item.evidence));
        message.push_str(&wrapped(
            &format!("Required by {}", item.needed_by),
            "      ",
        ));
        message.push('\n');
    }

    for item in uncertain {
        message.push_str(&format!(
            "  {} may also be missing — this check is a guess, so it isn't the\n  \
             reason the build stopped.\n",
            item.name
        ));
        message.push_str(&format!("      How we know: {}.\n", item.evidence));
        message.push_str(&wrapped(
            &format!("Required by {}", item.needed_by),
            "      ",
        ));
        message.push('\n');
    }

    message.push_str("  Install what's missing with:\n");
    for line in hint {
        message.push_str(&format!("      {line}\n"));
    }
    message.push_str(concat!(
        "\n",
        "  PaperBoy stops here on purpose. Left alone, the build fails later\n",
        "  anyway, inside a crate you never asked for and only after several\n",
        "  more minutes of compiling.\n",
        "\n",
        "  The README's \"Build prerequisites\" section covers every platform.\n",
        "  If this check is wrong about your machine, set\n",
    ));
    message.push_str(&format!("  {SKIP_VAR}=1 to bypass it.\n"));
    message
}

/// Wrap prose to a sensible width. The `needed_by` strings are written as
/// sentences rather than pre-broken lines so they stay editable, which means
/// something has to fold them before they hit a terminal.
fn wrapped(text: &str, indent: &str) -> String {
    const WIDTH: usize = 66;
    let mut out = String::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > WIDTH {
            out.push_str(&format!("{indent}{line}\n"));
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push_str(&format!("{indent}{line}\n"));
    }
    out
}

/// Emit one line of advice. Cargo prefixes each with `warning: paperboy@x.y.z:`,
/// so these stay short enough to survive that on a normal terminal.
fn warn(line: &str) {
    println!("cargo::warning={line}");
}

/// The command this particular machine wants, chosen by looking for the package
/// manager that is actually installed rather than by guessing from the OS name
/// alone — plenty of Linux boxes have more than one, and a suggestion the user
/// cannot run is no better than no suggestion.
///
/// Each command deliberately installs the whole prerequisite set rather than
/// only the piece that was detected as missing: they are cheap to re-run, a
/// package manager will simply report the ones already present, and it saves a
/// user who is missing two things from having to come back twice.
fn install_hint() -> Vec<String> {
    // Read from Cargo's view of the target rather than `cfg!(target_os)` so the
    // whole function stays exercisable without a Mac to hand.
    if target_cfg("CARGO_CFG_TARGET_OS") == "macos" {
        let mut hint = vec![
            "xcode-select --install   # C compiler, libclang, libxml2, perl, make".to_string(),
        ];
        if has("brew") {
            hint.push("brew install pkg-config".to_string());
        } else if has("port") {
            hint.push("sudo port install pkgconfig".to_string());
        } else {
            hint.push(
                "install Homebrew (https://brew.sh), then: brew install pkg-config".to_string(),
            );
        }
        return hint;
    }

    // Ordered so that a distro's native manager is found before anything that
    // might merely be present alongside it.
    let candidates: &[(&str, &str)] = &[
        (
            "apt-get",
            "sudo apt install build-essential pkg-config libxml2-dev libclang-dev perl",
        ),
        (
            "dnf",
            "sudo dnf install pkgconf-pkg-config gcc make perl libxml2-devel clang-devel",
        ),
        (
            "yum",
            "sudo yum install pkg-config gcc make perl libxml2-devel clang-devel",
        ),
        (
            "zypper",
            "sudo zypper install pkg-config gcc make perl libxml2-devel clang-devel",
        ),
        (
            "pacman",
            "sudo pacman -S pkgconf base-devel perl libxml2 clang",
        ),
        (
            "apk",
            "sudo apk add build-base pkgconfig perl libxml2-dev clang-dev",
        ),
    ];
    for (bin, command) in candidates {
        if has(bin) {
            return vec![(*command).to_string()];
        }
    }

    vec![
        "install your distribution's pkg-config, libxml2, clang, C compiler, perl and make packages"
            .to_string(),
    ]
}

/// Is `bin` an executable on `PATH`? Spawning `which` would itself be a
/// dependency on something that may not be installed, so walk `PATH` directly.
fn has(bin: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate: PathBuf = dir.join(bin);
        candidate.is_file()
    })
}
