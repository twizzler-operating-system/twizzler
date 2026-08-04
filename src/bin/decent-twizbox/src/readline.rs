//! A minimal single-line editor for the REPL: printable characters,
//! backspace, and up/down arrow recall through this run's input history.
//!
//! Deliberately not a full readline: no in-line cursor movement (left/right
//! arrows are ignored), no persistence across runs, ASCII input only. Just
//! enough to stop retyping the last command. Falls back to plain buffered
//! `stdin` on non-Unix platforms or when stdin isn't a terminal (piped
//! input, tests).

use std::io::{self, Write};

pub fn read_line(prompt: &str, history: &mut Vec<String>) -> io::Result<Option<String>> {
    #[cfg(unix)]
    {
        if unix::is_tty() {
            return unix::read_line(prompt, history);
        }
    }
    plain_read_line(prompt, history)
}

fn plain_read_line(prompt: &str, history: &mut Vec<String>) -> io::Result<Option<String>> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    if io::stdin().read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let line = line.trim_end_matches(['\n', '\r']).to_string();
    if !line.is_empty() {
        history.push(line.clone());
    }
    Ok(Some(line))
}

#[cfg(unix)]
mod unix {
    use std::io::{self, Read, Write};
    use std::os::unix::io::AsRawFd;

    pub fn is_tty() -> bool {
        unsafe { libc::isatty(io::stdin().as_raw_fd()) != 0 }
    }

    /// Puts the terminal into raw mode for the lifetime of this guard and
    /// restores the original settings on drop (including on early return /
    /// panic unwind).
    struct RawMode {
        fd: i32,
        original: libc::termios,
    }

    impl RawMode {
        fn enable(fd: i32) -> io::Result<Self> {
            let mut original: libc::termios = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut raw = original;
            unsafe { libc::cfmakeraw(&mut raw) };
            // cfmakeraw also disables signal generation; keep it so Ctrl-C
            // still sends SIGINT like normal instead of arriving as byte 0x03.
            raw.c_lflag |= libc::ISIG;
            if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { fd, original })
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.original) };
        }
    }

    pub fn read_line(prompt: &str, history: &mut Vec<String>) -> io::Result<Option<String>> {
        let fd = io::stdin().as_raw_fd();
        print!("{prompt}");
        io::stdout().flush()?;
        let _raw = RawMode::enable(fd)?;

        let mut buf = String::new();
        let mut history_index = history.len();
        let mut stdin = io::stdin();
        let mut byte = [0u8; 1];

        loop {
            if stdin.read(&mut byte)? == 0 {
                print!("\r\n");
                io::stdout().flush()?;
                return Ok(None); // EOF, e.g. Ctrl-D
            }
            match byte[0] {
                b'\r' | b'\n' => {
                    print!("\r\n");
                    io::stdout().flush()?;
                    break;
                }
                0x04 => {
                    // Ctrl-D. Raw mode (ICANON off) disables the terminal's
                    // usual EOF-on-Ctrl-D handling, so it has to be done by
                    // hand here instead of showing up as a zero-byte read.
                    print!("\r\n");
                    io::stdout().flush()?;
                    return Ok(None);
                }
                0x7f | 0x08 => {
                    // Backspace/Delete
                    if buf.pop().is_some() {
                        print!("\u{8} \u{8}");
                        io::stdout().flush()?;
                    }
                }
                0x1b => {
                    // Arrow keys arrive as ESC '[' 'A'/'B'/'C'/'D'.
                    let mut seq = [0u8; 2];
                    if stdin.read(&mut seq)? != 2 || seq[0] != b'[' {
                        continue;
                    }
                    match seq[1] {
                        b'A' if history_index > 0 => {
                            history_index -= 1;
                            redraw(prompt, &mut buf, &history[history_index])?;
                        }
                        b'B' if history_index + 1 < history.len() => {
                            history_index += 1;
                            redraw(prompt, &mut buf, &history[history_index])?;
                        }
                        b'B' if history_index < history.len() => {
                            history_index = history.len();
                            redraw(prompt, &mut buf, "")?;
                        }
                        _ => {} // left/right and anything else: not supported
                    }
                }
                byte @ 0x20..=0x7e => {
                    buf.push(byte as char);
                    print!("{}", byte as char);
                    io::stdout().flush()?;
                }
                _ => {} // non-ASCII/control bytes: not supported, dropped
            }
        }

        if !buf.is_empty() {
            history.push(buf.clone());
        }
        Ok(Some(buf))
    }

    fn redraw(prompt: &str, buf: &mut String, new_text: &str) -> io::Result<()> {
        print!("\r\x1b[K{prompt}{new_text}");
        io::stdout().flush()?;
        *buf = new_text.to_string();
        Ok(())
    }
}
