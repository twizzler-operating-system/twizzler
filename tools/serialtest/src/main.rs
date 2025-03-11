use std::{
    io::{BufRead, BufReader, Write},
    process::Stdio,
    time::Duration,
};

fn main() {
    let mut child = std::process::Command::new("cargo")
        .arg("start-qemu")
        .arg("-p=release")
        .arg("-q=-nographic")
        .arg("--autostart")
        .arg("serialecho")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();

    let stdout = child.stdout.take().unwrap();

    let (s, r) = std::sync::mpsc::channel();
    let reader = BufReader::new(stdout);
    let _j = std::thread::spawn(move || {
        let mut i = 0;
        let _ = r.recv();
        loop {
            let line = format!("{}\n", i);
            stdin.write_all(line.as_bytes()).unwrap();
            std::thread::sleep(Duration::from_millis(1));
            i += 1;
        }
    });
    let mut started = false;
    let mut exp = 0;
    let mut fails = 0;
    for line in reader.lines() {
        if let Ok(line) = line {
            let line = line.trim_matches(&['\r', '\n', '\t', ' ']);
            println!("{}", line.trim());

            if started {
                let num = u64::from_str_radix(line, 10);
                if exp == 0 {
                    if let Ok(num) = num {
                        exp = num;
                    }
                }
                if Ok(exp) != num {
                    println!("bad line: {:?}", exp);
                    let _ = child.kill();

                    fails += 1;
                    break;
                } else {
                    println!("ok! so far: {} failures", fails);
                    exp += 1;
                }
            }

            if line.trim().starts_with("SEQUENCE START") {
                eprintln!("==> seq start");
                started = true;
                let _ = s.send(());
            }
        }
    }
    println!("DONE: {} fails", fails);

    child.wait().unwrap();
}
