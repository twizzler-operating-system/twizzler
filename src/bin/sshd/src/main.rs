use std::{sync::Arc, thread::sleep, time::Duration};

use async_net::{TcpListener, TcpStream};
use futures_lite::{AsyncReadExt, AsyncWriteExt};
use rand::{RngCore, SeedableRng, TryRngCore};
use tracing::Level;
use zssh::{
    Behavior, SecretKey, Transport,
    ed25519_dalek::{SECRET_KEY_LENGTH, SigningKey, ed25519::signature::rand_core::CryptoRng},
};

fn main() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG)
            .finish(),
    )
    .unwrap();

    async_io::block_on(async {
        tracing::info!("binding");
        let listener = TcpListener::bind("0.0.0.0:5555").await.unwrap();
        tracing::info!("binding: done");
        let key = Arc::new(SecretKey::Ed25519 {
            secret_key: SigningKey::from_bytes(&[0; SECRET_KEY_LENGTH]),
        });
        tracing::info!("waiting for incoming");
        while let Ok(conn) = listener.accept().await {
            tracing::info!("incoming connection from {}", conn.1);

            let stream = IoStream { stream: conn.0 };
            server(stream, key.clone()).await;
        }
    })
}

async fn server(stream: IoStream, key: Arc<SecretKey>) {
    let ssh = Ssh {
        stream,
        rng: Rng {
            rng: rand::rngs::StdRng::from_os_rng(),
        },
        key,
    };
    let mut buf = [0; 4096];
    let mut transport = Transport::new(&mut buf, ssh);
    match transport.accept().await {
        Ok(mut client) => {
            tracing::info!("client connected");

            //let out = std::process::Command::new("/initrd/ls").output().unwrap();

            //client.write_all_stdout(&out.stdout).await.unwrap();
            //client.write_all_stderr(&out.stderr).await.unwrap();
            client
                .write_all_stdout(b"test banner for sshd on Twizzler!\n")
                .await
                .unwrap();
            sleep(Duration::from_secs(1));
            client.exit(0).await.unwrap();
            transport
                .disconnect(zssh::DisconnectReason::ByApplication)
                .await
                .unwrap();
        }
        Err(e) => {
            tracing::warn!("failed to accept client: {:?}", e);
        }
    }
}

#[derive(Clone)]
struct Ssh {
    stream: IoStream,
    rng: Rng,
    key: Arc<SecretKey>,
}

#[derive(Clone)]
struct IoStream {
    stream: TcpStream,
}

#[derive(Clone)]
struct SshUser {}

#[derive(Clone)]
struct SshCommand {
    cmd: String,
}

impl embedded_io_async::Read for IoStream {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.stream.read(buf).await
    }
}

impl embedded_io_async::ErrorType for IoStream {
    type Error = std::io::Error;
}

impl embedded_io_async::Write for IoStream {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.stream.write(buf).await
    }
}

#[derive(Clone)]
struct Rng {
    rng: rand::rngs::StdRng,
}

impl CryptoRng for Rng {}
impl zssh::ed25519_dalek::ed25519::signature::rand_core::RngCore for Rng {
    fn next_u32(&mut self) -> u32 {
        self.rng.next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        self.rng.next_u64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.rng.fill_bytes(dest)
    }

    fn try_fill_bytes(
        &mut self,
        dest: &mut [u8],
    ) -> Result<(), zssh::ed25519_dalek::ed25519::signature::rand_core::Error> {
        let _ = self.rng.try_fill_bytes(dest);
        Ok(())
    }
}

impl Behavior for Ssh {
    type Stream = IoStream;

    fn stream(&mut self) -> &mut Self::Stream {
        &mut self.stream
    }

    type Random = Rng;

    fn random(&mut self) -> &mut Self::Random {
        &mut self.rng
    }

    fn host_secret_key(&self) -> &zssh::SecretKey {
        &self.key
    }

    fn allow_user(
        &mut self,
        username: &str,
        _auth_method: &zssh::AuthMethod,
    ) -> Option<Self::User> {
        tracing::info!(":: {}", username);
        Some(SshUser {})
    }

    type User = SshUser;

    type Command = SshCommand;

    fn parse_command(&mut self, command: &str) -> Self::Command {
        tracing::info!(":: {}", command);
        SshCommand {
            cmd: command.to_string(),
        }
    }
}
