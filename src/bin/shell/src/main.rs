#![feature(iterator_try_collect)]

use std::{
    fs::{File, OpenOptions},
    io::{self, PipeReader, Read, Write, stdin},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    process::{Child, Command, Stdio},
};

use embedded_io::ErrorType;
use miette::IntoDiagnostic;

#[derive(Debug)]
enum NextHop {
    Seq,
    Pipe,
    Background,
}

#[derive(Debug)]
struct Redirect {
    fd: u32,
    append: bool,
    path: String,
}

impl Redirect {
    pub fn build_stdio(&self, ctx: &mut InvokeCtx) -> miette::Result<Stdio> {
        let mut open = OpenOptions::new();
        let create = self.fd != 0;
        let file = open
            .create(create)
            .append(self.append)
            .truncate(!self.append && create)
            .write(self.fd != 0)
            .read(self.fd == 0)
            .open(&self.path)
            .into_diagnostic()?;
        let file: OwnedFd = file.into();
        let stdio = unsafe { Stdio::from_raw_fd(file.as_raw_fd()) };
        ctx.fds.push(file);

        Ok(stdio)
    }
}

#[derive(Debug)]
struct ShellInvoke {
    redirect: Vec<Redirect>,
    next: NextHop,
    command: Vec<String>,
    env: Vec<(String, String)>,
}

impl ShellInvoke {
    pub fn invoke(&self, ctx: &mut InvokeCtx) -> miette::Result<Child> {
        let mut cmd = Command::new(&self.command[0]);
        cmd.args(&self.command[1..]);
        cmd.envs(self.env.iter().map(|x| (&x.0, &x.1)));

        if let Some(stdin) = ctx.pipe.as_ref() {
            cmd.stdin(unsafe { Stdio::from_raw_fd(stdin.as_raw_fd()) });
        }

        if let Some(r) = self.get_stdin_redir() {
            cmd.stdin(r.build_stdio(ctx)?);
        }

        if let Some(r) = self.get_stdout_redir() {
            cmd.stdout(r.build_stdio(ctx)?);
        }

        if let Some(r) = self.get_stderr_redir() {
            cmd.stderr(r.build_stdio(ctx)?);
        }

        let (wait, pipe) = match &self.next {
            NextHop::Seq => (true, None),
            NextHop::Background => (false, None),
            NextHop::Pipe => {
                let (reader, writer) = std::io::pipe().into_diagnostic()?;
                let reader: OwnedFd = reader.into();
                let writer: OwnedFd = writer.into();

                cmd.stdout(unsafe { Stdio::from_raw_fd(writer.as_raw_fd()) });
                ctx.fds.push(writer);

                (false, Some(reader))
            }
        };

        let mut child = cmd.spawn().into_diagnostic()?;

        ctx.pipe = pipe;
        ctx.fds.clear();
        if wait {
            child.wait().into_diagnostic()?;
        }

        Ok(child)
    }

    fn get_stdin_redir(&self) -> Option<&Redirect> {
        self.redirect.iter().find(|r| r.fd == 0)
    }

    fn get_stdout_redir(&self) -> Option<&Redirect> {
        self.redirect.iter().find(|r| r.fd == 1)
    }

    fn get_stderr_redir(&self) -> Option<&Redirect> {
        self.redirect.iter().find(|r| r.fd == 2)
    }
}

#[derive(Debug)]
struct ShellCommand {
    invokes: Vec<ShellInvoke>,
}

#[derive(Default)]
struct InvokeCtx {
    pipe: Option<OwnedFd>,
    fds: Vec<OwnedFd>,
}

impl ShellCommand {
    pub fn invoke(&self, ctx: &mut InvokeCtx) -> miette::Result<Vec<Child>> {
        self.invokes.iter().map(|i| i.invoke(ctx)).try_collect()
    }
}

impl ShellInvoke {
    fn parse(input: &str, next: NextHop) -> miette::Result<Option<Self>> {
        let mut redirect = Vec::new();
        let mut command = Vec::new();
        let mut env = Vec::new();
        let mut parse_env = true;

        let parts: Vec<&str> = input.split_whitespace().collect();
        for part in parts {
            if parse_env && part.contains('=') {
                let split = part.split_once('=').unwrap();
                env.push((split.0.to_string(), split.1.to_string()));
                continue;
            }
            parse_env = false;

            let (fd, append, prefix_len) = if part.starts_with("2>>") {
                (2, true, 3)
            } else if part.starts_with("2>") {
                (2, false, 2)
            } else if part.starts_with(">>") {
                (1, true, 2)
            } else if part.starts_with(">") {
                (1, false, 1)
            } else if part.starts_with("<") {
                (0, false, 1)
            } else {
                command.push(part.to_string());
                continue;
            };

            redirect.push(Redirect {
                fd,
                append,
                path: part[prefix_len..].to_string(),
            });
        }

        if command.is_empty() {
            return Ok(None);
        }

        Ok(Some(Self {
            redirect,
            next,
            command,
            env,
        }))
    }
}

impl ShellCommand {
    fn parse(line: &str) -> miette::Result<Self> {
        let split = line.split_inclusive([';', '|', '\n', '&']);

        let mut invokes = Vec::new();
        for item in split {
            if item.len() == 0 {
                continue;
            }

            let last = item.chars().last().unwrap();
            let next = match last {
                '|' => NextHop::Pipe,
                '&' => NextHop::Background,
                _ => NextHop::Seq,
            };

            let si = ShellInvoke::parse(item.trim_end_matches([';', '|', '&', '\n']), next)?;
            if let Some(si) = si {
                invokes.push(si);
            }
        }

        Ok(Self { invokes })
    }
}

struct TwzIo;

impl ErrorType for TwzIo {
    type Error = std::io::Error;
}

impl embedded_io::Read for TwzIo {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let len = std::io::stdin().read(buf)?;

        Ok(len)
    }
}

impl embedded_io::Write for TwzIo {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        std::io::stdout().write(buf)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        std::io::stdout().flush()
    }
}

fn main() {
    println!("Hello, world!");

    println!("To run a program, type its name.");
    let mut io = TwzIo;
    let mut buffer = [0; 1024];
    let mut history = [0; 1024];
    let mut editor = noline::builder::EditorBuilder::from_slice(&mut buffer)
        .with_slice_history(&mut history)
        .build_sync(&mut io)
        .unwrap();
    loop {
        //let mstats = monitor_api::stats().unwrap();
        //println!("{:?}", mstats);
        //let line = editor.readline("twz> ", &mut io).unwrap();

        print!("twz> ");
        let mut s = String::new();
        let _ = stdin().read_line(&mut s).unwrap();

        if s.is_empty() {
            continue;
        }

        let cmd = ShellCommand::parse(&s).unwrap();
        println!("==> {:?}", cmd);

        let mut ctx = InvokeCtx::default();
        let res = cmd.invoke(&mut ctx);

        if let Ok(children) = res {
            for mut child in children {
                let _ = child
                    .wait()
                    .inspect_err(|e| tracing::warn!("failed to wait for child: {e}"));
            }
        }
        /*
        let cmd = s.split_whitespace().collect::<Vec<_>>();
        if cmd.len() == 0 {
            continue;
        }

        let background = cmd.iter().any(|s| *s == "&");

        // Find env vars
        let cmd = cmd.into_iter().map(|s| as_env(s)).collect::<Vec<_>>();
        let vars = cmd
            .iter()
            .filter_map(|r| match r {
                Ok((k, v)) => Some((k, v)),
                Err(_) => None,
            })
            .collect::<Vec<_>>();
        let cmd = cmd
            .iter()
            .filter_map(|r| match r {
                Ok(_) => None,
                Err(s) => Some(s),
            })
            .collect::<Vec<_>>();

        tracing::info!("got env: {:?}, cmd: {:?}", vars, cmd);

        let comp = CompartmentLoader::new(cmd[0], cmd[0], NewCompartmentFlags::empty())
            .args(&cmd)
            .with_controller(ControllerOption::Object(pty.id()))
            .env(vars.into_iter().map(|(k, v)| format!("{}={}", k, v)))
            .load();
        if let Ok(comp) = comp {
            if background {
                tracing::info!("continuing compartment {} in background", cmd[0]);
            } else {
                let mut flags = comp.info().flags;
                while !flags.contains(CompartmentFlags::EXITED) {
                    flags = comp.wait(flags);
                }
            }
        } else {
            warn!("failed to start {}", cmd[0]);
        }
        */
    }
}

fn as_env<'a>(s: &'a str) -> Result<(&'a str, &'a str), &'a str> {
    let mut split = s.split("=");
    Ok((split.next().ok_or(s)?, split.next().ok_or(s)?))
}
