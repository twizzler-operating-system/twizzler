#![feature(iterator_try_collect)]
#![feature(unix_send_signal)]

use std::{
    collections::BTreeMap,
    ffi::c_void,
    fmt::Debug,
    fs::{File, OpenOptions},
    io::{Read, Write, stderr, stdin, stdout},
    os::{
        fd::{AsFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        twizzler::process::ChildExt,
    },
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    time::Instant,
};

use colored::Colorize;
use embedded_io::ErrorType;
use miette::IntoDiagnostic;
use twizzler_abi::{
    syscall::sys_thread_exit,
    upcall::{UpcallData, UpcallFrame},
};
use twizzler_io::pty::{DEFAULT_TERMIOS, DEFAULT_TERMIOS_RAW};
use twizzler_rt_abi::bindings::{IO_REGISTER_TERMIOS, twz_rt_set_upcall_handler};

#[derive(Debug, Clone, Copy)]
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

    pub fn build_rawfd(&self, _ctx: &mut InvokeCtx) -> miette::Result<RawFd> {
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
    use std::{
        fs::File,
        io::{BufReader, BufWriter, Write},
    };

    use hhmmss::Hhmmss;
    use miette::IntoDiagnostic;

    use crate::{InvokeCtx, ShellInvoke, WaitChild};

    pub struct BuiltinCtx<'a> {
        pub stdin: File,
        pub stdout: File,
        pub stderr: File,
        pub cmd: &'a ShellInvoke,
    }

    impl BuiltinCtx<'_> {
        fn _stdin(&self) -> BufReader<&File> {
            BufReader::new(&self.stdin)
        }

        fn stdout(&self) -> BufWriter<&File> {
            BufWriter::new(&self.stdout)
        }

        fn stderr(&self) -> &File {
            &self.stderr
        }
    }

    pub fn jobs(ctx: &mut BuiltinCtx, invoke: &InvokeCtx) -> miette::Result<()> {
        writeln!(ctx.stdout(), "JOB  ELAPSED TYPE NAME",).into_diagnostic()?;
        for job in invoke.jobs.jobs.values() {
            let dur = job.start_time.elapsed();
            let ty = match job.next {
                crate::NextHop::Seq => "    ",
                crate::NextHop::Pipe => "pipe",
                crate::NextHop::Background => "bgnd",
            };
            writeln!(
                ctx.stdout(),
                "{:3.3} {} {} {}",
                job.id,
                dur.hhmmss(),
                ty,
                job.name
            )
            .into_diagnostic()?;
        }
        Ok(())
    }

    pub fn kill(ctx: &mut BuiltinCtx, invoke: &InvokeCtx) -> miette::Result<()> {
        if ctx.cmd.command.len() < 2 {
            writeln!(ctx.stderr(), "usage: kill <job_id>").into_diagnostic()?;
            return Ok(());
        }
        let Ok(job_id) = ctx.cmd.command[1].parse::<u64>() else {
            writeln!(ctx.stderr(), "invalid job id `{}'", ctx.cmd.command[1]).into_diagnostic()?;
            return Ok(());
        };
        let Some(job) = invoke.jobs.jobs.get(&job_id) else {
            writeln!(ctx.stderr(), "job {} not found", job_id).into_diagnostic()?;
            return Ok(());
        };
        let Ok(ch) = &mut *job.child.lock().unwrap() else {
            writeln!(ctx.stderr(), "job {} not running", job_id).into_diagnostic()?;
            return Ok(());
        };
        if let WaitChild::Child(ch) = ch {
            if let Err(e) = ch.kill() {
                writeln!(ctx.stderr(), "failed to kill job {}: {}", job_id, e).into_diagnostic()?;
            }
        } else {
            writeln!(ctx.stderr(), "job {} not running", job_id).into_diagnostic()?;
        }
        Ok(())
    }

    pub fn wait(ctx: &mut BuiltinCtx, invoke: &InvokeCtx) -> miette::Result<()> {
        if ctx.cmd.command.len() < 2 {
            writeln!(ctx.stderr(), "usage: wait <job_id>").into_diagnostic()?;
            return Ok(());
        }
        let Ok(job_id) = ctx.cmd.command[1].parse::<u64>() else {
            writeln!(ctx.stderr(), "invalid job id `{}'", ctx.cmd.command[1]).into_diagnostic()?;
            return Ok(());
        };
        let Some(job) = invoke.jobs.jobs.get(&job_id) else {
            writeln!(ctx.stderr(), "job {} not found", job_id).into_diagnostic()?;
            return Ok(());
        };
        let Ok(ch) = &mut *job.child.lock().unwrap() else {
            writeln!(ctx.stderr(), "job {} not running", job_id).into_diagnostic()?;
            return Ok(());
        };
        if let WaitChild::Child(ch) = ch {
            if let Err(e) = ch.wait() {
                writeln!(ctx.stderr(), "failed to wait for job {}: {}", job_id, e)
                    .into_diagnostic()?;
            }
        } else {
            writeln!(ctx.stderr(), "job {} not running", job_id).into_diagnostic()?;
        }
        Ok(())
    }

    pub fn cd(ctx: &mut BuiltinCtx, _invoke: &InvokeCtx) -> miette::Result<()> {
        if ctx.cmd.command.len() != 2 {
            writeln!(ctx.stderr(), "usage: cd <directory>").into_diagnostic()?;
            return Ok(());
        }
        if let Err(e) = std::env::set_current_dir(&ctx.cmd.command[1]) {
            writeln!(ctx.stderr(), "failed to change directory: {}", e).into_diagnostic()?;
        }
        Ok(())
    }

    pub fn pwd(ctx: &mut BuiltinCtx, _invoke: &InvokeCtx) -> miette::Result<()> {
        let Ok(current) = std::env::current_dir() else {
            writeln!(ctx.stderr(), "failed to get current directory").into_diagnostic()?;
            return Ok(());
        };
        writeln!(ctx.stdout(), "{}", current.display()).into_diagnostic()?;
        Ok(())
    }

    pub fn echo(ctx: &mut BuiltinCtx, _invoke: &InvokeCtx) -> miette::Result<()> {
        for arg in &ctx.cmd.command[1..] {
            write!(ctx.stdout(), "{} ", arg).into_diagnostic()?;
        }
        writeln!(ctx.stdout()).into_diagnostic()?;
        Ok(())
    }

    pub fn set(ctx: &mut BuiltinCtx, _invoke: &InvokeCtx) -> miette::Result<()> {
        if ctx.cmd.command.len() < 2 {
            writeln!(ctx.stderr(), "usage: set VAR [val]").into_diagnostic()?;
            return Ok(());
        }
        let var = &ctx.cmd.command[1];
        let val = ctx.cmd.command.get(2).map(|s| s.as_str()).unwrap_or("");
        unsafe { std::env::set_var(var, val) };
        Ok(())
    }

    pub fn unset(ctx: &mut BuiltinCtx, _invoke: &InvokeCtx) -> miette::Result<()> {
        if ctx.cmd.command.len() < 2 {
            writeln!(ctx.stderr(), "usage: unset VAR").into_diagnostic()?;
            return Ok(());
        }
        let var = &ctx.cmd.command[1];
        unsafe { std::env::remove_var(var) };
        Ok(())
    }

    pub fn env(ctx: &mut BuiltinCtx, _invoke: &InvokeCtx) -> miette::Result<()> {
        for (key, value) in std::env::vars() {
            writeln!(ctx.stdout(), "{}={}", key, value).into_diagnostic()?;
        }
        Ok(())
    }

    pub fn help(ctx: &mut BuiltinCtx, _invoke: &InvokeCtx) -> miette::Result<()> {
        writeln!(ctx.stdout(), "Twizzler Shell 0.1").into_diagnostic()?;
        writeln!(ctx.stdout(), "This shell has basic line-editing, env vars, pipes and I/O redirection, backgrounding and jobs, and system command execution.").into_diagnostic()?;
        Ok(())
    }
}

impl ShellInvoke {
    fn invoke_builtin(
        &self,
        f: impl FnOnce(&mut builtins::BuiltinCtx, &InvokeCtx) -> miette::Result<()>,
        ctx: &mut InvokeCtx,
    ) -> miette::Result<Job> {
        let mut bctx = unsafe {
            builtins::BuiltinCtx {
                stdin: File::from_raw_fd(
                    stdin()
                        .as_fd()
                        .try_clone_to_owned()
                        .into_diagnostic()?
                        .into_raw_fd(),
                ),
                stdout: File::from_raw_fd(
                    stdout()
                        .as_fd()
                        .try_clone_to_owned()
                        .into_diagnostic()?
                        .into_raw_fd(),
                ),
                stderr: File::from_raw_fd(
                    stderr()
                        .as_fd()
                        .try_clone_to_owned()
                        .into_diagnostic()?
                        .into_raw_fd(),
                ),
                cmd: self,
            }
        };
        if let Some(stdin) = ctx.pipe.take() {
            bctx.stdin = unsafe { File::from_raw_fd(stdin.into_raw_fd()) };
        }

        if let Some(r) = self.get_stdin_redir() {
            bctx.stdin = unsafe { File::from_raw_fd(r.build_rawfd(ctx)?) };
        }

        if let Some(r) = self.get_stdout_redir() {
            bctx.stdout = unsafe { File::from_raw_fd(r.build_rawfd(ctx)?) };
        }

        if let Some(r) = self.get_stderr_redir() {
            bctx.stderr = unsafe { File::from_raw_fd(r.build_rawfd(ctx)?) };
        }

        let (wait, pipe) = match &self.next {
            NextHop::Seq => (true, None),
            NextHop::Background => (false, None),
            NextHop::Pipe => {
                let (reader, writer) = std::io::pipe().into_diagnostic()?;
                let reader: OwnedFd = reader.into();
                let writer: OwnedFd = writer.into();

                bctx.stdout = unsafe { File::from_raw_fd(writer.into_raw_fd()) };

                (false, Some(reader))
            }
        };
        let mut job = Job::new(
            Arc::new(Mutex::new(Ok(WaitChild::Builtin))),
            ctx.jobs.next_id(),
            &self.command[0],
            self.next,
        );
        f(&mut bctx, ctx)?;

        ctx.pipe = pipe;
        if wait {
            job.wait()?;
        }
        Ok(job)
    }

    fn try_builtin(&self, ctx: &mut InvokeCtx) -> miette::Result<Option<Job>> {
        match self.command[0].as_str() {
            "jobs" => self.invoke_builtin(builtins::jobs, ctx).map(|j| Some(j)),
            "kill" => self.invoke_builtin(builtins::kill, ctx).map(|j| Some(j)),
            "cd" => self.invoke_builtin(builtins::cd, ctx).map(|j| Some(j)),
            "pwd" => self.invoke_builtin(builtins::pwd, ctx).map(|j| Some(j)),
            "echo" => self.invoke_builtin(builtins::echo, ctx).map(|j| Some(j)),
            "wait" => self.invoke_builtin(builtins::wait, ctx).map(|j| Some(j)),
            "set" => self.invoke_builtin(builtins::set, ctx).map(|j| Some(j)),
            "unset" => self.invoke_builtin(builtins::unset, ctx).map(|j| Some(j)),
            "env" => self.invoke_builtin(builtins::env, ctx).map(|j| Some(j)),
            "help" => self.invoke_builtin(builtins::help, ctx).map(|j| Some(j)),
            _ => Ok(None),
        }
    }

    pub fn invoke(&self, ctx: &mut InvokeCtx) -> miette::Result<Job> {
        if let Some(job) = self.try_builtin(ctx)? {
            return Ok(job);
        }
        let mut cmd = Command::new(&self.command[0]);
        cmd.args(&self.command[1..]);
        cmd.envs(std::env::vars());
        cmd.envs(self.env.iter().map(|x| (&x.0, &x.1)));
        cmd.current_dir(std::env::current_dir().into_diagnostic()?);

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

                cmd.stdout(unsafe { Stdio::from_raw_fd(writer.into_raw_fd()) });

                (false, Some(reader))
            }
        };

        ctx.pipe = pipe;

        let child = if wait {
            let mut c = cmd.spawn().into_diagnostic()?;
            drop(cmd);
            c.wait().into_diagnostic()?;
            Arc::new(Mutex::new(Ok(WaitChild::Child(c))))
        } else {
            let cell = Arc::new(Mutex::new(Ok(WaitChild::Waiting)));
            let tcell = cell.clone();
            std::thread::spawn(move || {
                let child = cmd.spawn();
                match child {
                    Ok(mut child) => {
                        let _ = child.wait_ready();
                        *tcell.lock().unwrap() = Ok(WaitChild::Child(child));
                    }
                    Err(err) => {
                        *tcell.lock().unwrap() = Err(err);
                    }
                }
                drop(cmd);
                // TODO: this shouldn't be needed
                sys_thread_exit(0);
            });
            cell
        };

        let job = Job::new(child, ctx.jobs.next_id(), &self.command[0], self.next);
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
    jobs: &'a mut Jobs,
}

impl<'a> InvokeCtx<'a> {
    pub fn new(jobs: &'a mut Jobs) -> Self {
        InvokeCtx { pipe: None, jobs }
    }
}

enum WaitChild {
    Builtin,
    Waiting,
    Child(Child),
}

struct Job {
    child: Arc<Mutex<std::io::Result<WaitChild>>>,
    id: u64,
    start_time: Instant,
    name: String,
    next: NextHop,
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
    pub fn new(
        child: Arc<Mutex<std::io::Result<WaitChild>>>,
        id: u64,
        name: impl ToString,
        next: NextHop,
    ) -> Self {
        Job {
            child,
            id,
            start_time: Instant::now(),
            name: name.to_string(),
            next,
        }
    }

    pub fn try_wait(&mut self) -> miette::Result<Option<ExitStatus>> {
        match &mut *self.child.lock().unwrap() {
            Ok(WaitChild::Child(child)) => child.try_wait().into_diagnostic(),
            Ok(WaitChild::Builtin) => Ok(Some(ExitStatus::default())),
            Ok(WaitChild::Waiting) => Ok(None),
            _ => Ok(Some(ExitStatus::default())),
        }
    }

    pub fn wait(&mut self) -> miette::Result<ExitStatus> {
        match &mut *self.child.lock().unwrap() {
            Ok(WaitChild::Child(child)) => child.wait().into_diagnostic(),
            Ok(WaitChild::Waiting) => panic!("job is still waiting"),
            _ => Ok(ExitStatus::default()),
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

    pub fn scan(&mut self) {
        let finished = self
            .jobs
            .extract_if(.., |_, j| j.try_wait().is_ok_and(|s| s.is_some()))
            .collect::<Vec<_>>();
        for (_, job) in finished {
            if matches!(job.next, NextHop::Background) {
                println!("job {} finished", job.id);
            }
            self.jobs.remove(&job.id);
            self.release_id(job.id);
        }
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

            if prefix_len >= part.len() {
                return Err(miette::miette!("invalid redirect: `{}'", part));
            }

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
        let len = std::io::stdout().write(buf)?;
        std::io::stdout().flush()?;
        Ok(len)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        std::io::stdout().flush()
    }
}

unsafe extern "C-unwind" fn upcall_handler(frame: *mut c_void, data: *const c_void) {
    let data = unsafe { data.cast::<UpcallData>().as_ref().unwrap() };
    let _frame = unsafe { frame.cast::<UpcallFrame>().as_ref().unwrap() };
    match data.info {
        twizzler_abi::upcall::UpcallInfo::Mailbox(val) => {
            twizzler_abi::klog_println!("shell: signal {}", val);
            if val == libc::SIGINFO as u64 {
                let mstats = monitor_api::stats().unwrap();
                twizzler_abi::klog_println!("{:?}", mstats);
            }
        }
        _ => {
            panic!("fatal error: {:?}", data);
        }
    }
}

fn main() {
    unsafe { twz_rt_set_upcall_handler(Some(upcall_handler)) };

    let mut io = TwzIo;
    let mut buffer = [0; 1024];
    let mut history = [0; 1024];
    let mut editor = noline::builder::EditorBuilder::from_slice(&mut buffer)
        .with_slice_history(&mut history)
        .build_sync(&mut io)
        .unwrap();
    colored::control::set_override(true);
    let mut jobs = Jobs::default();
    loop {
        jobs.scan();

        let cd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let user = "root".red();
        let host = "twizzler".bright_blue();
        let cd = cd.to_str().unwrap().bright_cyan();

        let s = if true {
            // TODO: color seems to break noline
            let prompt = format!("root@twizzler [/]# ");
            twizzler_rt_abi::io::twz_rt_fd_set_config(0, IO_REGISTER_TERMIOS, DEFAULT_TERMIOS_RAW)
                .unwrap();
            let line = editor.readline(prompt.as_str(), &mut io).unwrap();
            twizzler_rt_abi::io::twz_rt_fd_set_config(0, IO_REGISTER_TERMIOS, DEFAULT_TERMIOS)
                .unwrap();
            line.to_string()
        } else {
            print!("{}@{} [{}]> ", user, host, cd);
            stdout().flush().unwrap();
            let mut s = String::new();
            let _ = stdin().read_line(&mut s).unwrap();
            s
        };

        if s.is_empty() {
            continue;
        }

        let cmd = ShellCommand::parse(&s).unwrap();

        let mut ctx = InvokeCtx::new(&mut jobs);
        let res = cmd.invoke(&mut ctx);
        drop(ctx);

        if let Ok(new_jobs) = res {
            for new_job in new_jobs {
                if matches!(new_job.next, NextHop::Background) {
                    println!("job {} backgrounding ({})", new_job.id, new_job.name);
                }
                jobs.jobs.insert(new_job.id, new_job);
            }
        } else {
            eprintln!("shell: {}", res.unwrap_err());
        }
    }
}
