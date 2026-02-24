use std::{
    future::pending,
    os::fd::FromRawFd,
    process::{Command, Stdio},
};

use async_executor::Executor;
use async_net::{TcpListener, TcpStream};
use embedded_io_async::{ErrorType, Read, Write};
use futures::{AsyncReadExt, AsyncWriteExt, FutureExt};
use miette::IntoDiagnostic;
use sunset::{ChanHandle, SignKey};
use sunset_async::{ProgressHolder, SSHServer};
use tracing::Level;
use twizzler::object::Object;
use twizzler_io::pty::{DEFAULT_TERMIOS, PtyBase, PtyServerHandle};
use twizzler_rt_abi::{
    fd::{RawFd, twz_rt_fd_close},
    object::ObjectCreate,
};

static EXECUTOR: Executor = Executor::new();

fn main() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG)
            .finish(),
    )
    .unwrap();

    std::thread::spawn(|| {
        async_io::block_on(EXECUTOR.run(pending::<()>()));
    });

    let listener = async_io::block_on(async { TcpListener::bind("0.0.0.0:5555").await.unwrap() });

    tracing::info!("ready for incomming connections");
    for _ in 0..4 {
        async_io::block_on(EXECUTOR.run(async { accept(&listener).await }));
    }
}

async fn accept(listener: &TcpListener) {
    while let Ok(conn) = listener.accept().await {
        tracing::info!("accepting connection from {}", conn.1);
        match sunset_server(conn.0).await {
            Ok(_) => {
                tracing::info!("closed connection to {}", conn.1);
            }
            Err(e) => {
                tracing::error!("error in connection to {}: {}", conn.1, e);
            }
        }
    }
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

impl Write for Writer {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.sock.write(buf).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.sock.flush().await
    }
}

impl ErrorType for Writer {
    type Error = std::io::Error;
}

async fn sunset_server(conn: TcpStream) -> miette::Result<()> {
    let mut ssh_rxbuf = Box::new([0; 4096]);
    let mut ssh_txbuf = Box::new([0; 4096]);
    let serv = SSHServer::new(&mut *ssh_rxbuf, &mut *ssh_txbuf);

    let mut rsock = Reader { sock: conn.clone() };
    let mut wsock = Writer { sock: conn.clone() };

    let (send, recv) = async_channel::bounded(1);

    let runner = async { serv.run(&mut rsock, &mut wsock).await.into_diagnostic() }.fuse();
    futures::pin_mut!(runner);
    let session = session(&serv, send).fuse();
    futures::pin_mut!(session);
    let shell = shell(&serv, recv).fuse();
    futures::pin_mut!(shell);

    let out = futures::select! {
        out = runner => out,
        out = session => out,
        out = shell => out,
    };
    conn.shutdown(std::net::Shutdown::Read).into_diagnostic()?;

    runner.await?;

    out
}

fn setup_pty() -> (RawFd, Object<PtyBase>) {
    let pty =
        twizzler_io::pty::PtyBase::create_object(ObjectCreate::default(), DEFAULT_TERMIOS).unwrap();
    let client_fd = twizzler_rt_abi::fd::twz_rt_fd_open_pty_client(pty.id().raw(), 0).unwrap();
    //let server_fd = twizzler_rt_abi::fd::twz_rt_fd_open_pty_server(pty.id().raw(), 0).unwrap();

    (client_fd, pty)
}

async fn shell(
    serv: &SSHServer<'_>,
    ch_ch: async_channel::Receiver<(ChanHandle, Option<String>)>,
) -> miette::Result<()> {
    let (ch, command) = ch_ch.recv().await.into_diagnostic()?;
    let (stdio, _stderr) = serv.stdio_stderr(ch).await.into_diagnostic()?;
    let (mut stdin, mut stdout) = stdio.split();

    let mut cmd = if let Some(command) = command {
        Command::new(command)
    } else {
        Command::new("/initrd/shell")
    };

    //cmd.env_clear();

    let (client_fd, pty) = setup_pty();

    unsafe {
        cmd.stdin(Stdio::from_raw_fd(client_fd));
        cmd.stdout(Stdio::from_raw_fd(client_fd));
        cmd.stderr(Stdio::from_raw_fd(client_fd));
    }

    let netreader = async {
        let mut server = PtyServerHandle::new(pty.id(), None).unwrap();
        loop {
            let mut buf = [0; 1024];
            let count = stdin.read(&mut buf).await.unwrap();
            let (_, s) = blocking::unblock(move || {
                (
                    <PtyServerHandle as std::io::Write>::write_all(&mut server, &buf[0..count])
                        .unwrap(),
                    server,
                )
            })
            .await;
            server = s;
        }
    }
    .fuse();

    let netwriter = async {
        let mut server = PtyServerHandle::new(pty.id(), None).unwrap();
        loop {
            let mut buf = [0; 1024];
            let (count, buf, s) = blocking::unblock(move || {
                (
                    <PtyServerHandle as std::io::Read>::read(&mut server, &mut buf).unwrap(),
                    buf,
                    server,
                )
            })
            .await;
            server = s;
            stdout.write_all(&buf[0..count]).await.unwrap();
            stdout.flush().await.unwrap();
        }
    }
    .fuse();

    tracing::debug!("spawning {}", cmd.get_program().display());
    let mut handle = cmd.spawn().into_diagnostic()?;

    twz_rt_fd_close(client_fd);
    let handle = blocking::unblock(move || {
        let _ = handle.wait();
    })
    .fuse();

    futures::pin_mut!(handle);
    futures::pin_mut!(netreader);
    futures::pin_mut!(netwriter);

    futures::select! {
        _ = netreader => (),
        _ = netwriter => (),
        _ = handle => (),
    };

    Ok(())
}

async fn session(
    serv: &SSHServer<'_>,
    sender: async_channel::Sender<(ChanHandle, Option<String>)>,
) -> miette::Result<()> {
    let mut chan_handle = None;
    loop {
        let mut ph = ProgressHolder::new();
        let event = serv.progress(&mut ph).await.into_diagnostic()?;
        match event {
            sunset::ServEvent::Hostkeys(serv_hostkeys) => {
                let key = SignKey::generate(sunset::KeyType::Ed25519, None).into_diagnostic()?;
                serv_hostkeys.hostkeys(&[&key]).into_diagnostic()?;
            }
            sunset::ServEvent::FirstAuth(serv_first_auth) => {
                let name = serv_first_auth.username().into_diagnostic()?;
                tracing::debug!("logging in as {}", name);
                serv_first_auth.allow().into_diagnostic()?;
            }
            //sunset::ServEvent::PasswordAuth(serv_password_auth) => todo!(),
            //sunset::ServEvent::PubkeyAuth(serv_pubkey_auth) => todo!(),
            sunset::ServEvent::OpenSession(serv_open_session) => {
                if chan_handle.is_some() {
                    serv_open_session
                        .reject(sunset::ChanFail::SSH_OPEN_ADMINISTRATIVELY_PROHIBITED)
                        .into_diagnostic()?;
                } else {
                    let ch = serv_open_session.accept().into_diagnostic()?;
                    tracing::debug!("opened session, channel = {}", ch.num());
                    chan_handle = Some(ch);
                }
            }
            sunset::ServEvent::SessionShell(serv_shell_request) => {
                tracing::debug!("shell start on channel {}", serv_shell_request.channel());
                if let Some(ch) = chan_handle.take() {
                    serv_shell_request.succeed().into_diagnostic()?;
                    sender.send((ch, None)).await.into_diagnostic()?;
                } else {
                    serv_shell_request.fail().into_diagnostic()?;
                }
            }
            sunset::ServEvent::SessionExec(serv_exec_request) => {
                tracing::debug!(
                    "session exec on channel {}: {}",
                    serv_exec_request.channel(),
                    serv_exec_request.command().into_diagnostic()?
                );
                if let Some(ch) = chan_handle.take() {
                    let command = serv_exec_request.command().into_diagnostic()?.to_string();
                    serv_exec_request.succeed().into_diagnostic()?;
                    sender.send((ch, Some(command))).await.into_diagnostic()?;
                } else {
                    serv_exec_request.fail().into_diagnostic()?;
                }
            }
            sunset::ServEvent::SessionPty(serv_pty_request) => {
                let ch = serv_pty_request.channel();
                tracing::debug!("pty request on channel {}", ch);
                serv_pty_request.succeed().into_diagnostic()?;
            }
            sunset::ServEvent::SessionEnv(serv_environment_request) => {
                let name = serv_environment_request.name().into_diagnostic()?;
                let value = serv_environment_request.value().into_diagnostic()?;
                let ch = serv_environment_request.channel();
                tracing::debug!("env request on channel {}: {}={}", ch, name, value);
                serv_environment_request.succeed().into_diagnostic()?;
            }
            sunset::ServEvent::PollAgain => {}
            sunset::ServEvent::Defunct => {
                return Ok(());
            }
            _ => {
                tracing::warn!("unknown event: {:?}", event);
            }
        }
    }
}
