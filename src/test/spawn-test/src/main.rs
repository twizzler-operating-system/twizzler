//! Smoke test for spawn + stdio piping (plans/sshgitplan.md part 2). Runs standalone (graded by
//! exit code via test-programs), self-spawning as /initrd/spawn-test with --child <mode>.
//!
//! Only fds 0..=2 are inherited across spawn (init_fds skips higher fds); every case here works
//! within that.

use std::{
    io::{Read, Write},
    process::{Command, Stdio},
};

const SELF: &str = "/initrd/spawn-test";
const PAYLOAD_LEN: usize = 1 << 20;

fn pattern_byte(i: usize) -> u8 {
    (i.wrapping_mul(31).wrapping_add(7) & 0xff) as u8
}

fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn payload() -> Vec<u8> {
    (0..PAYLOAD_LEN).map(pattern_byte).collect()
}

fn child_main(mode: &str, arg: Option<&str>) -> ! {
    let code = match mode {
        "spew" => {
            let data = payload();
            let mut out = std::io::stdout();
            out.write_all(&data).and_then(|_| out.flush()).is_err() as i32
        }
        "checksum" => {
            let mut data = Vec::new();
            match std::io::stdin().read_to_end(&mut data) {
                Ok(_) => {
                    println!("{} {:x}", data.len(), fnv1a(&data));
                    0
                }
                Err(_) => 1,
            }
        }
        "echo" => {
            // Chunked copy, so echoed data flows back while the parent is still writing.
            let mut buf = [0u8; 4096];
            let mut sin = std::io::stdin();
            let mut sout = std::io::stdout();
            loop {
                match sin.read(&mut buf) {
                    Ok(0) => break 0,
                    Ok(n) => {
                        if sout
                            .write_all(&buf[..n])
                            .and_then(|_| sout.flush())
                            .is_err()
                        {
                            break 1;
                        }
                    }
                    Err(_) => break 1,
                }
            }
        }
        "null-probe" => {
            // stdin is /dev/null: immediate EOF. stdout/stderr are /dev/null: writes succeed.
            let mut buf = [0u8; 16];
            match std::io::stdin().read(&mut buf) {
                Ok(0) => {
                    println!("null-probe alive");
                    0
                }
                Ok(_) => 3,
                Err(_) => 4,
            }
        }
        "env-report" => {
            // One line the parent parses. `big` counts SPAWN_TEST_BIG_* vars whose value matches
            // big_val(i), so a truncated or reordered environment shows up as a short count.
            let a = std::env::var("SPAWN_TEST_A").unwrap_or_else(|_| "-".to_string());
            let mut count = 0usize;
            let mut big = 0usize;
            for (k, v) in std::env::vars() {
                count += 1;
                if let Some(i) = k.strip_prefix("SPAWN_TEST_BIG_") {
                    if i.parse::<usize>().is_ok_and(|i| v == big_val(i)) {
                        big += 1;
                    }
                }
            }
            let path = std::env::var_os("PATH").is_some() as i32;
            println!(
                "ENVREPORT a={} path={} count={} big={}",
                a, path, count, big
            );
            0
        }
        "exit" => arg.and_then(|a| a.parse().ok()).unwrap_or(101),
        "exit-thread" => {
            // process::exit from a non-main thread while main is parked in an untimed
            // sys_thread_sync (mpsc recv). POSIX: the whole process ends with code 7. Before the
            // exit-liveness fix this killed only the worker and the compartment lived forever.
            // (An exit(0) variant has to wait for a toolchain std whose thread trampoline uses
            // twz_rt_thread_exit -- with an older std, 0 from a non-main thread still means
            // thread-exit.)
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(100));
                std::process::exit(7);
            });
            let (_tx, rx) = std::sync::mpsc::channel::<()>();
            let _ = rx.recv();
            98
        }
        _ => 100,
    };
    std::process::exit(code);
}

struct Harness {
    failed: usize,
}

impl Harness {
    fn case(&mut self, name: &str, f: impl FnOnce() -> Result<(), String>) {
        match f() {
            Ok(()) => println!("PASS {}", name),
            Err(e) => {
                self.failed += 1;
                println!("FAIL {}: {}", name, e);
            }
        }
    }
}

fn spawn_child(mode: &str) -> Command {
    let mut cmd = Command::new(SELF);
    cmd.arg("--child").arg(mode);
    cmd
}

fn case_stdout_pipe() -> Result<(), String> {
    let mut child = spawn_child("spew")
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn: {}", e))?;
    let mut data = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut data)
        .map_err(|e| format!("read: {}", e))?;
    let status = child.wait().map_err(|e| format!("wait: {}", e))?;
    if !status.success() {
        return Err(format!("child status {:?}", status));
    }
    if data.len() != PAYLOAD_LEN {
        return Err(format!("got {} bytes, want {}", data.len(), PAYLOAD_LEN));
    }
    let want = fnv1a(&payload());
    let got = fnv1a(&data);
    if got != want {
        return Err(format!("checksum {:x}, want {:x}", got, want));
    }
    Ok(())
}

fn case_stdin_pipe() -> Result<(), String> {
    let mut child = spawn_child("checksum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn: {}", e))?;
    let data = payload();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(&data)
        .map_err(|e| format!("write: {}", e))?;
    // EOF-on-writer-drop: the child's read_to_end must terminate on this.
    drop(stdin);
    let mut report = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut report)
        .map_err(|e| format!("read report: {}", e))?;
    let status = child.wait().map_err(|e| format!("wait: {}", e))?;
    if !status.success() {
        return Err(format!("child status {:?}", status));
    }
    let want = format!("{} {:x}\n", PAYLOAD_LEN, fnv1a(&data));
    if report != want {
        return Err(format!(
            "report {:?}, want {:?}",
            report.trim(),
            want.trim()
        ));
    }
    Ok(())
}

fn case_bidir_echo() -> Result<(), String> {
    let mut child = spawn_child("echo")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn: {}", e))?;
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let writer = std::thread::spawn(move || -> Result<(), String> {
        let data = payload();
        for chunk in data.chunks(8192) {
            stdin
                .write_all(chunk)
                .map_err(|e| format!("write: {}", e))?;
        }
        Ok(()) // drop closes: child sees EOF
    });
    let mut data = Vec::new();
    stdout
        .read_to_end(&mut data)
        .map_err(|e| format!("read: {}", e))?;
    writer.join().map_err(|_| "writer panicked".to_string())??;
    let status = child.wait().map_err(|e| format!("wait: {}", e))?;
    if !status.success() {
        return Err(format!("child status {:?}", status));
    }
    if data.len() != PAYLOAD_LEN || fnv1a(&data) != fnv1a(&payload()) {
        return Err(format!("echoed {} bytes, mismatch", data.len()));
    }
    Ok(())
}

fn case_exit_status() -> Result<(), String> {
    let status = spawn_child("exit")
        .arg("7")
        .status()
        .map_err(|e| format!("spawn: {}", e))?;
    if status.code() != Some(7) {
        return Err(format!("code {:?}, want Some(7)", status.code()));
    }
    let status = spawn_child("exit")
        .arg("0")
        .status()
        .map_err(|e| format!("spawn: {}", e))?;
    if !status.success() {
        return Err(format!("exit-0 child status {:?}", status));
    }
    Ok(())
}

fn case_exit_from_thread() -> Result<(), String> {
    let status = spawn_child("exit-thread")
        .status()
        .map_err(|e| format!("spawn: {}", e))?;
    if status.code() != Some(7) {
        return Err(format!(
            "code {:?}, want Some(7) (98=recv returned, 101=lost to forced-exit code)",
            status.code()
        ));
    }
    Ok(())
}

fn big_val(i: usize) -> String {
    format!("{:090}", i)
}

fn env_report(cmd: &mut Command) -> Result<String, String> {
    let out = cmd
        .stdout(Stdio::piped())
        .output()
        .map_err(|e| format!("spawn: {}", e))?;
    if !out.status.success() {
        return Err(format!("child status {:?}", out.status));
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .find(|l| l.starts_with("ENVREPORT "))
        .map(|l| l.to_string())
        .ok_or_else(|| format!("no ENVREPORT in {:?}", s))
}

// Env-content cases: nothing else in the system asserts what a child actually receives through
// std Command's explicit-env path (the one cargo uses for build scripts), only that spawns work.
fn case_env_explicit() -> Result<(), String> {
    let report = env_report(spawn_child("env-report").env("SPAWN_TEST_A", "alpha"))?;
    if !report.contains(" a=alpha ") {
        return Err(format!("explicit var missing: {}", report));
    }
    if std::env::var_os("PATH").is_some() && !report.contains(" path=1 ") {
        return Err(format!("inherited PATH missing: {}", report));
    }
    Ok(())
}

fn case_env_remove() -> Result<(), String> {
    let report = env_report(
        spawn_child("env-report")
            .env("SPAWN_TEST_A", "x")
            .env_remove("PATH"),
    )?;
    if !report.contains(" a=x ") {
        return Err(format!("explicit var missing: {}", report));
    }
    // path=1 here means the child read some other environment (e.g. a stale copy) rather than
    // the one this spawn constructed.
    if !report.contains(" path=0 ") {
        return Err(format!("removed PATH still visible: {}", report));
    }
    Ok(())
}

fn case_env_big() -> Result<(), String> {
    // ~12KB across 120 vars, the shape of cargo's build-script environment.
    let mut cmd = spawn_child("env-report");
    cmd.env("SPAWN_TEST_A", "omega");
    for i in 0..120 {
        cmd.env(format!("SPAWN_TEST_BIG_{}", i), big_val(i));
    }
    let report = env_report(&mut cmd)?;
    if !report.contains(" a=omega ") {
        return Err(format!("explicit var missing: {}", report));
    }
    if !report.contains(" big=120") {
        return Err(format!("big-env vars lost: {}", report));
    }
    Ok(())
}

fn case_null_stdio() -> Result<(), String> {
    let status = spawn_child("null-probe")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("spawn: {}", e))?;
    match status.code() {
        Some(0) => Ok(()),
        c => Err(format!(
            "code {:?}, want Some(0) (3=data on null stdin, 4=read err)",
            c
        )),
    }
}

fn case_inherit() -> Result<(), String> {
    let status = spawn_child("exit")
        .arg("0")
        .status()
        .map_err(|e| format!("spawn: {}", e))?;
    if !status.success() {
        return Err(format!("child status {:?}", status));
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("--child") {
        child_main(
            args.get(2).map(|s| s.as_str()).unwrap_or(""),
            args.get(3).map(|s| s.as_str()),
        );
    }

    // A deadlocked pipe case would otherwise hang the whole suite.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(120));
        println!("FAIL spawn-test: watchdog timeout");
        std::process::exit(2);
    });

    let mut h = Harness { failed: 0 };
    h.case("stdout-pipe", case_stdout_pipe);
    h.case("stdin-pipe-eof", case_stdin_pipe);
    h.case("bidir-echo", case_bidir_echo);
    h.case("exit-status", case_exit_status);
    h.case("exit-from-thread", case_exit_from_thread);
    h.case("null-stdio", case_null_stdio);
    h.case("inherit", case_inherit);
    h.case("env-explicit", case_env_explicit);
    h.case("env-remove", case_env_remove);
    h.case("env-big", case_env_big);

    if h.failed > 0 {
        println!("spawn-test: {} case(s) failed", h.failed);
        std::process::exit(1);
    }
    println!("spawn-test: all cases passed");
}
