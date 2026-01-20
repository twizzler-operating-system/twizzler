#![feature(iterator_try_collect)]
#![feature(unix_send_signal)]

use std::{
    collections::BTreeMap,
    fmt::Debug,
    fs::OpenOptions,
    io::{Read, Write, stdin, stdout},
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        twizzler::process::ChildExt,
    },
    process::{Child, Command, ExitStatus, Stdio},
    time::Instant,
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
        let raw = self.build_rawfd(ctx)?;
        let stdio = unsafe { Stdio::from_raw_fd(raw) };
        Ok(stdio)
    }

    pub fn build_rawfd(&self, ctx: &mut InvokeCtx) -> miette::Result<RawFd> {
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
        let child_file = file.try_clone().into_diagnostic()?;
        let stdio = child_file.into_raw_fd();
        twizzler_abi::klog_println!("adding fd {} to ctx", file.as_raw_fd());
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

mod builtins {
    use std::os::fd::RawFd;

    pub struct BuiltinCtx {
        pub stdin: RawFd,
        pub stdout: RawFd,
        pub stderr: RawFd,
    }
    pub fn jobs(ctx: &mut BuiltinCtx) {
        println!("GOT JOBS");
    }
}

impl ShellInvoke {
    fn invoke_builtin(
        &self,
        f: impl FnOnce(&mut builtins::BuiltinCtx),
        ctx: &mut InvokeCtx,
    ) -> miette::Result<Job> {
        let mut bctx = builtins::BuiltinCtx {
            stdin: 0,
            stdout: 1,
            stderr: 2,
        };
        if let Some(stdin) = ctx.pipe.as_ref() {
            let child_pipe = stdin.try_clone().into_diagnostic()?;
            bctx.stdin = child_pipe.into_raw_fd();
        }

        if let Some(r) = self.get_stdin_redir() {
            bctx.stdin = r.build_rawfd(ctx)?;
        }

        if let Some(r) = self.get_stdout_redir() {
            bctx.stdout = r.build_rawfd(ctx)?;
        }

        if let Some(r) = self.get_stderr_redir() {
            bctx.stderr = r.build_rawfd(ctx)?;
        }

        let (wait, pipe) = match &self.next {
            NextHop::Seq => (true, None),
            NextHop::Background => (false, None),
            NextHop::Pipe => {
                let (reader, writer) = std::io::pipe().into_diagnostic()?;
                let reader: OwnedFd = reader.into();
                let writer: OwnedFd = writer.into();
                let our_writer = writer.try_clone().into_diagnostic()?;

                bctx.stdout = writer.into_raw_fd();
                twizzler_abi::klog_println!("adding fd {} to ctx", our_writer.as_raw_fd());
                ctx.fds.push(our_writer);

                (false, Some(reader))
            }
        };
        let mut job = Job::new(None, ctx.jobs.next_id(), &self.command[0], !wait);
        f(&mut bctx);

        if ctx.pipe.is_some() {
            twizzler_abi::klog_println!(
                "closing pipe fd {}",
                ctx.pipe.as_ref().unwrap().as_raw_fd()
            );
        }
        ctx.pipe = pipe;
        twizzler_abi::klog_println!("closing fds: {:?}", ctx.fds);
        ctx.fds.clear();
        if wait {
            job.wait()?;
        }
        Ok(job)
    }

    fn try_builtin(&self, ctx: &mut InvokeCtx) -> miette::Result<Option<Job>> {
        match self.command[0].as_str() {
            "jobs" => self.invoke_builtin(builtins::jobs, ctx).map(|j| Some(j)),
            _ => Ok(None),
        }
    }

    pub fn invoke(&self, ctx: &mut InvokeCtx) -> miette::Result<Job> {
        twizzler_abi::klog_println!("call invoke on {}", &self.command[0]);
        if let Some(job) = self.try_builtin(ctx)? {
            return Ok(job);
        }
        let mut cmd = Command::new(&self.command[0]);
        cmd.args(&self.command[1..]);
        cmd.envs(self.env.iter().map(|x| (&x.0, &x.1)));

        if let Some(stdin) = ctx.pipe.take() {
            cmd.stdin(unsafe { Stdio::from_raw_fd(stdin.into_raw_fd()) });
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
                let our_writer = writer.try_clone().into_diagnostic()?;

                cmd.stdout(unsafe { Stdio::from_raw_fd(writer.into_raw_fd()) });
                twizzler_abi::klog_println!("adding fd {} to ctx", our_writer.as_raw_fd());
                ctx.fds.push(our_writer);

                (false, Some(reader))
            }
        };

        twizzler_abi::klog_println!("starting {:?}", cmd);
        let child = cmd.spawn().into_diagnostic()?;
        let mut job = Job::new(Some(child), ctx.jobs.next_id(), &self.command[0], !wait);
        twizzler_abi::klog_println!("wait ready");
        job.child.as_mut().map(|c| c.wait_ready());
        twizzler_abi::klog_println!("ready!");

        twizzler_abi::klog_println!("setting pipe to {:?}", pipe);
        ctx.pipe = pipe;
        twizzler_abi::klog_println!("closing fds: {:?}", ctx.fds);
        ctx.fds.clear();
        if wait {
            drop(cmd);
            job.wait()
                .inspect_err(|e| twizzler_abi::klog_println!("??? {}", e))?;
        }

        Ok(job)
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

struct InvokeCtx<'a> {
    pipe: Option<OwnedFd>,
    fds: Vec<OwnedFd>,
    jobs: &'a mut Jobs,
}

impl<'a> InvokeCtx<'a> {
    pub fn new(jobs: &'a mut Jobs) -> Self {
        InvokeCtx {
            pipe: None,
            fds: Vec::new(),
            jobs,
        }
    }
}

struct Job {
    child: Option<Child>,
    id: u64,
    start_time: Instant,
    name: String,
    background: bool,
}

impl Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("id", &self.id)
            .field("start_time", &self.start_time)
            .field("name", &self.name)
            .finish()
    }
}

impl Job {
    pub fn new(child: Option<Child>, id: u64, name: impl ToString, background: bool) -> Self {
        Job {
            child,
            id,
            start_time: Instant::now(),
            name: name.to_string(),
            background,
        }
    }

    pub fn try_wait(&mut self) -> miette::Result<Option<ExitStatus>> {
        match &mut self.child {
            Some(child) => child.try_wait().into_diagnostic(),
            None => Ok(Some(ExitStatus::default())),
        }
    }

    pub fn wait(&mut self) -> miette::Result<ExitStatus> {
        match &mut self.child {
            Some(child) => child.wait().into_diagnostic(),
            None => Ok(ExitStatus::default()),
        }
    }
}

#[derive(Default)]
struct Jobs {
    jobs: BTreeMap<u64, Job>,
    next_job_id: u64,
    id_stack: Vec<u64>,
}

impl Jobs {
    pub fn next_id(&mut self) -> u64 {
        if self.id_stack.is_empty() {
            self.next_job_id += 1;
            self.next_job_id
        } else {
            self.id_stack.pop().unwrap()
        }
    }

    pub fn release_id(&mut self, id: u64) {
        if self.next_job_id == id {
            self.next_job_id -= 1;
            return;
        }
        self.id_stack.push(id);
    }
}

impl ShellCommand {
    pub fn invoke(&self, ctx: &mut InvokeCtx) -> miette::Result<Vec<Job>> {
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
    let mut jobs = Jobs::default();
    loop {
        //let mstats = monitor_api::stats().unwrap();
        //println!("{:?}", mstats);
        //let line = editor.readline("twz> ", &mut io).unwrap();

        print!("twz> ");
        stdout().flush().unwrap();
        let mut s = String::new();
        let _ = stdin().read_line(&mut s).unwrap();

        if s.is_empty() {
            continue;
        }

        let cmd = ShellCommand::parse(&s).unwrap();
        //println!("==> {:?}", cmd);

        let mut ctx = InvokeCtx::new(&mut jobs);
        let res = cmd.invoke(&mut ctx);
        drop(ctx);

        if let Ok(new_jobs) = res {
            for new_job in new_jobs {
                if new_job.background {
                    println!("job {} backgrounding", new_job.id);
                    jobs.jobs.insert(new_job.id, new_job);
                }
            }
        } else {
            twizzler_abi::klog_println!("err: {}", res.unwrap_err());
        }

        let finished = jobs
            .jobs
            .extract_if(.., |_, j| j.try_wait().is_ok_and(|s| s.is_some()))
            .collect::<Vec<_>>();
        for (_, job) in finished {
            println!("job {} finished", job.id);
            jobs.jobs.remove(&job.id);
            jobs.release_id(job.id);
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
