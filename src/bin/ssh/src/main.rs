use std::io::{Read as _, Write as _};

use async_channel::{Receiver, Sender};
use async_executor::LocalExecutor;
use async_net::TcpStream;
use clap::Parser;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use embedded_io_async::{ErrorType, Read, Write};
use futures::{AsyncReadExt, AsyncWriteExt, FutureExt};
use miette::{Context, IntoDiagnostic, miette};
use sunset::{CliEvent, CliSessionExit, Pty, SignKey, packets::PubKey};
use sunset_async::{ChanIn, ChanInOut, ProgressHolder, SSHClient};
use tracing::Level;

#[derive(Parser)]
#[command(about = "Connect to an SSH server.")]
struct Args {
    /// Host to connect to, as [user@]host.
    destination: String,
    /// Port to connect to.
    #[arg(short, long, default_value_t = 22)]
    port: u16,
    /// Unencrypted OpenSSH-format private key to authenticate with.
    #[arg(short, long)]
    identity: Option<String>,
    /// Password to authenticate with, instead of prompting for one.
    #[arg(long)]
    password: Option<String>,
    /// Check the server host key against this known-hosts file, rejecting unknown keys.
    #[arg(long)]
    known_hosts: Option<String>,
    /// Force a pty, even when running a command.
    #[arg(short = 't')]
    force_tty: bool,
    /// Never request a pty.
    #[arg(short = 'T', conflicts_with = "force_tty")]
    no_tty: bool,
    /// Command to run, instead of a login shell.
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

impl Args {
    fn want_pty(&self) -> bool {
        !self.no_tty && (self.force_tty || self.command.is_empty())
    }

    fn split_destination(&self) -> (String, &str) {
        match self.destination.split_once('@') {
            Some((user, host)) => (user.to_string(), host),
            None => (
                std::env::var("USER").unwrap_or_else(|_| "root".to_string()),
                self.destination.as_str(),
            ),
        }
    }
}

fn main() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::WARN)
            .without_time()
            .compact()
            .finish(),
    )
    .unwrap();
    let _ = tracing_log::LogTracer::init();

    let args = Args::parse();
    let ex = LocalExecutor::new();
    match async_io::block_on(ex.run(client(&args))) {
        Ok(status) => std::process::exit(status),
        Err(e) => {
            eprintln!("ssh: {:?}", e);
            std::process::exit(1);
        }
    }
}

async fn client(args: &Args) -> miette::Result<i32> {
    let (user, host) = args.split_destination();
    let conn = TcpStream::connect((host, args.port))
        .await
        .into_diagnostic()
        .with_context(|| format!("connecting to {}:{}", host, args.port))?;

    let mut ssh_rxbuf = Box::new([0; 4096]);
    let mut ssh_txbuf = Box::new([0; 4096]);
    let cli = SSHClient::new(&mut *ssh_rxbuf, &mut *ssh_txbuf);

    let mut rsock = Reader { sock: conn.clone() };
    let mut wsock = Writer { sock: conn.clone() };

    let (chan_send, chan_recv) = async_channel::bounded(1);
    let (ready_send, ready_recv) = async_channel::bounded(1);

    let status = {
        let runner = async {
            cli.run(&mut rsock, &mut wsock)
                .await
                .into_diagnostic()
                .with_context(|| "client-run")
        }
        .fuse();
        let session = session(&cli, args, &user, host, chan_send, ready_send).fuse();
        let stdio = stdio(args.want_pty(), chan_recv, ready_recv).fuse();
        futures::pin_mut!(runner, session, stdio);

        futures::select! {
            out = runner => { out?; 0 },
            out = session => out?,
            out = stdio => { out?; 0 },
        }
    };

    let _ = conn.shutdown(std::net::Shutdown::Both);
    Ok(status)
}

struct Reader {
    sock: TcpStream,
}

impl ErrorType for Reader {
    type Error = std::io::Error;
}

impl Read for Reader {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.sock.read(buf).await
    }
}

struct Writer {
    sock: TcpStream,
}

impl ErrorType for Writer {
    type Error = std::io::Error;
}

impl Write for Writer {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.sock.write(buf).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.sock.flush().await
    }
}

/// What to do once the `ProgressHolder` (which holds the session lock) has been dropped.
enum Next {
    Nothing,
    OpenSession,
    SessionReady,
    Done,
}

type Channels<'g> = (ChanInOut<'g>, Option<ChanIn<'g>>);

async fn session<'g, 'a>(
    cli: &'g SSHClient<'a>,
    args: &Args,
    user: &str,
    host: &str,
    chan_send: Sender<Channels<'g>>,
    ready_send: Sender<()>,
) -> miette::Result<i32> {
    let mut key = match args.identity.as_deref() {
        Some(path) => Some(load_identity(path)?),
        None => None,
    };
    let mut status = 0;

    loop {
        let mut ph = ProgressHolder::new();
        let next = match cli.progress(&mut ph).await.into_diagnostic()? {
            CliEvent::Hostkey(hostkey) => {
                let accept = {
                    let key = hostkey.hostkey().into_diagnostic()?;
                    match args.known_hosts.as_deref() {
                        Some(path) => known_hosts_match(path, host, &key)?,
                        None => {
                            eprintln!(
                                "ssh: accepting unverified {} host key for {}",
                                key.algorithm_name().unwrap_or("unknown"),
                                host
                            );
                            true
                        }
                    }
                };
                if accept {
                    hostkey.accept().into_diagnostic()?;
                } else {
                    hostkey.reject().into_diagnostic()?;
                    return Err(miette!("host key verification failed for {}", host));
                }
                Next::Nothing
            }
            CliEvent::Banner(banner) => {
                eprint!("{}", banner.banner().into_diagnostic()?);
                Next::Nothing
            }
            CliEvent::Username(request) => {
                request.username(user).into_diagnostic()?;
                Next::Nothing
            }
            CliEvent::Password(request) => {
                let password = match args.password.as_ref() {
                    Some(password) => password.clone(),
                    None => read_password(user, host).await?,
                };
                request.password(password).into_diagnostic()?;
                Next::Nothing
            }
            CliEvent::Pubkey(request) => {
                match key.take() {
                    Some(key) => request.pubkey(key).into_diagnostic()?,
                    None => request.skip().into_diagnostic()?,
                }
                Next::Nothing
            }
            CliEvent::AgentSign(request) => {
                request.skip().into_diagnostic()?;
                Next::Nothing
            }
            CliEvent::Authenticated => Next::OpenSession,
            CliEvent::SessionOpened(mut opener) => {
                if args.want_pty() {
                    opener.pty(pty_config()?).into_diagnostic()?;
                }
                if args.command.is_empty() {
                    opener.shell().into_diagnostic()?;
                } else {
                    opener.exec(args.command.join(" ")).into_diagnostic()?;
                }
                Next::SessionReady
            }
            CliEvent::SessionExit(exit) => {
                status = match exit {
                    CliSessionExit::Status(code) => code as i32,
                    CliSessionExit::Signal(_) => 255,
                };
                Next::Done
            }
            CliEvent::Defunct => Next::Done,
            CliEvent::PollAgain => Next::Nothing,
        };
        drop(ph);

        match next {
            Next::Nothing => {}
            Next::OpenSession => {
                let channels = if args.want_pty() {
                    (cli.open_session_pty().await.into_diagnostic()?, None)
                } else {
                    let (io, err) = cli.open_session_nopty().await.into_diagnostic()?;
                    (io, Some(err))
                };
                chan_send
                    .send(channels)
                    .await
                    .map_err(|_| miette!("session channel closed"))?;
            }
            Next::SessionReady => ready_send.send(()).await.into_diagnostic()?,
            Next::Done => return Ok(status),
        }
    }
}

async fn stdio(
    raw: bool,
    chan_recv: Receiver<Channels<'_>>,
    ready_recv: Receiver<()>,
) -> miette::Result<()> {
    let (chan, chan_err) = chan_recv.recv().await.into_diagnostic()?;
    ready_recv.recv().await.into_diagnostic()?;

    let _raw = if raw { Some(RawMode::enable()?) } else { None };
    let (mut cin, mut cout) = chan.split();

    let to_remote = async {
        let mut buf = [0; 1024];
        loop {
            let (count, b) = blocking::unblock(move || {
                let mut buf = buf;
                (std::io::stdin().read(&mut buf), buf)
            })
            .await;
            buf = b;
            let count = count.into_diagnostic()?;
            if count == 0 {
                break;
            }
            cout.write_all(&buf[0..count]).await.into_diagnostic()?;
        }
        Ok(())
    }
    .fuse();

    let from_remote = copy_out(&mut cin, false).fuse();
    let from_remote_err = async {
        match chan_err {
            Some(mut err) => copy_out(&mut err, true).await,
            None => futures::future::pending().await,
        }
    }
    .fuse();

    futures::pin_mut!(to_remote, from_remote, from_remote_err);
    futures::select! {
        out = to_remote => out,
        out = from_remote => out,
        out = from_remote_err => out,
    }
}

async fn copy_out(chan: &mut impl Read<Error = sunset::Error>, err: bool) -> miette::Result<()> {
    let mut buf = [0; 1024];
    loop {
        let count = chan.read(&mut buf).await.into_diagnostic()?;
        if count == 0 {
            return Ok(());
        }
        let out = buf;
        blocking::unblock(move || {
            let mut sink: Box<dyn std::io::Write> = if err {
                Box::new(std::io::stderr())
            } else {
                Box::new(std::io::stdout())
            };
            sink.write_all(&out[0..count]).and_then(|_| sink.flush())
        })
        .await
        .into_diagnostic()?;
    }
}

struct RawMode;

impl RawMode {
    fn enable() -> miette::Result<Self> {
        enable_raw_mode().into_diagnostic()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn pty_config() -> miette::Result<Pty> {
    let (cols, rows) = size().unwrap_or((80, 24));
    let term: String = std::env::var("TERM")
        .unwrap_or_else(|_| "xterm-256color".to_string())
        .chars()
        .take(sunset::config::MAX_TERM)
        .collect();
    Ok(Pty {
        term: term.as_str().try_into().map_err(|_| miette!("bad TERM"))?,
        cols: cols as u32,
        rows: rows as u32,
        width: 0,
        height: 0,
        modes: heapless::Vec::new(),
    })
}

fn load_identity(path: &str) -> miette::Result<SignKey> {
    let key = std::fs::read(path)
        .into_diagnostic()
        .with_context(|| format!("reading identity {}", path))?;
    SignKey::from_openssh(key)
        .into_diagnostic()
        .with_context(|| format!("parsing identity {}", path))
}

fn known_hosts_match(path: &str, host: &str, key: &PubKey) -> miette::Result<bool> {
    let known = std::fs::read_to_string(path)
        .into_diagnostic()
        .with_context(|| format!("reading known hosts {}", path))?;
    for line in known.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((hosts, keytext)) = line.split_once(' ') else {
            continue;
        };
        if hosts.split(',').any(|h| h == host) && key.matches_openssh(keytext).unwrap_or(false) {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn read_password(user: &str, host: &str) -> miette::Result<String> {
    eprint!("{}@{}'s password: ", user, host);
    std::io::stderr().flush().into_diagnostic()?;
    let password = blocking::unblock(|| {
        // Read with echo off, so the password doesn't end up on the terminal.
        let _raw = RawMode::enable()?;
        let mut password = String::new();
        for byte in std::io::stdin().bytes() {
            match byte.into_diagnostic()? {
                b'\r' | b'\n' => break,
                0x7f | 0x08 => {
                    password.pop();
                }
                0x03 => return Err(miette!("interrupted")),
                byte => password.push(byte as char),
            }
        }
        Ok(password)
    })
    .await?;
    eprintln!();
    Ok(password)
}
