//! Which exit-time work runs on each way out of a C, C++ and Rust program. Runs standalone (graded
//! by exit code via test-programs); the C and C++ programs are this crate's other binaries, and the
//! Rust cases self-spawn with --child <mode>.

use std::process::{Command, ExitCode, Stdio};

extern "C" {
    fn atexit(f: extern "C" fn()) -> i32;
    fn write(fd: i32, buf: *const u8, len: usize) -> isize;
}

fn mark(s: &str) {
    let line = format!("{s}\n");
    unsafe { write(1, line.as_ptr(), line.len()) };
}

extern "C" fn on_atexit() {
    mark("atexit");
}

struct TlsMarker;
impl Drop for TlsMarker {
    fn drop(&mut self) {
        mark("tls-dtor");
    }
}

thread_local! {
    static TLS: TlsMarker = const { TlsMarker };
}

fn child_main(mode: &str) -> ExitCode {
    unsafe { atexit(on_atexit) };
    TLS.with(|_| {});
    mark("main");
    match mode {
        "exit" => std::process::exit(3),
        "panic" => panic!("exit-test: deliberate panic"),
        _ => ExitCode::from(3),
    }
}

/// `None`: any failing code.
struct Case {
    prog: &'static str,
    mode: &'static str,
    code: Option<i32>,
    out: &'static [&'static str],
}

const C_EXIT: &[&str] = &["main", "atexit2", "atexit1", "stdio-buffered"];
const CXX_EXIT: &[&str] = &[
    "main",
    "tls-dtor",
    "local-static-dtor",
    "atexit",
    "static-dtor",
    "stdio-buffered",
];

const CASES: &[Case] = &[
    Case {
        prog: "exit-test-c",
        mode: "return",
        code: Some(3),
        out: C_EXIT,
    },
    Case {
        prog: "exit-test-c",
        mode: "exit",
        code: Some(3),
        out: C_EXIT,
    },
    Case {
        prog: "exit-test-c",
        mode: "_exit",
        code: Some(3),
        out: &["main"],
    },
    Case {
        prog: "exit-test-c",
        mode: "quick_exit",
        code: Some(3),
        out: &["main", "at_quick_exit"],
    },
    Case {
        prog: "exit-test-c",
        mode: "abort",
        code: None,
        out: &["main"],
    },
    Case {
        prog: "exit-test-c",
        mode: "pathconf",
        code: Some(0),
        out: &["main"],
    },
    Case {
        prog: "exit-test-cxx",
        mode: "return",
        code: Some(3),
        out: CXX_EXIT,
    },
    Case {
        prog: "exit-test-cxx",
        mode: "exit",
        code: Some(3),
        out: CXX_EXIT,
    },
    Case {
        prog: "exit-test-cxx",
        mode: "abort",
        code: None,
        out: &["main"],
    },
    Case {
        prog: "exit-test-cxx",
        mode: "current_path",
        code: Some(0),
        out: &[],
    },
    Case {
        prog: "exit-test",
        mode: "return",
        code: Some(3),
        out: &["main", "tls-dtor", "atexit"],
    },
    Case {
        prog: "exit-test",
        mode: "panic",
        code: Some(101),
        out: &["main", "tls-dtor", "atexit"],
    },
    Case {
        prog: "exit-test",
        mode: "exit",
        code: Some(3),
        out: &["main"],
    },
    Case {
        prog: "exit-test-c",
        mode: "thread-exit",
        code: Some(3),
        out: C_EXIT,
    },
];

/// Where `unittest` finds test programs.
fn path(prog: &str) -> String {
    ["/pkg/twizzler/bin", "/initrd"]
        .iter()
        .map(|dir| format!("{dir}/{prog}"))
        .find(|path| std::fs::metadata(path).is_ok())
        .unwrap_or_else(|| format!("/initrd/{prog}"))
}

fn run(case: &Case) -> bool {
    let mut cmd = Command::new(path(case.prog));
    if case.prog == "exit-test" {
        cmd.arg("--child");
    }
    // A file, not a pipe: a child that dies abnormally never closes its end of a pipe.
    let path = "/exit-test-out";
    let status =
        std::fs::File::create(path).and_then(|file| cmd.arg(case.mode).stdout(file).status());
    let status = match status {
        Ok(status) => status,
        Err(e) => {
            println!("exit-test: {} {}: spawn failed: {e}", case.prog, case.mode);
            return false;
        }
    };
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let _ = std::fs::remove_file(path);
    let lines: Vec<&str> = text.lines().collect();
    let code = status.code();
    let code_ok = match case.code {
        Some(want) => code == Some(want),
        None => !status.success(),
    };
    let ok = code_ok && lines == case.out;
    println!(
        "exit-test: {} {} {}: code {:?} out {:?}",
        if ok { "ok" } else { "FAIL" },
        case.prog,
        case.mode,
        code,
        lines
    );
    if !ok {
        println!(
            "exit-test:   wanted code {:?} out {:?}",
            case.code, case.out
        );
    }
    ok
}

/// An exit from another thread has to release the process's pipe ends, or this never returns.
fn thread_exit_pipe() -> bool {
    let out = Command::new(path("exit-test-c"))
        .arg("thread-exit")
        .stdout(Stdio::piped())
        .output();
    let ok = out.as_ref().is_ok_and(|out| {
        out.status.code() == Some(3)
            && String::from_utf8_lossy(&out.stdout)
                .lines()
                .eq(C_EXIT.iter().copied())
    });
    println!(
        "exit-test: {} exit-test-c thread-exit (pipe): {:?}",
        if ok { "ok" } else { "FAIL" },
        out.map(|out| (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned()
        ))
    );
    ok
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 2 && args[1] == "--child" {
        return child_main(&args[2]);
    }
    let failed = CASES.iter().filter(|c| !run(c)).count() + !thread_exit_pipe() as usize;
    println!("exit-test: {} of {} cases failed", failed, CASES.len() + 1);
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
