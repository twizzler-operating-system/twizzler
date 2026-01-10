use std::io::{Read, Write};

use twizzler_io::pty::{PtyClientHandle, PtyServerHandle};

#[derive(Clone)]
pub enum PtyHandleKind {
    Server(PtyServerHandle),
    Client(PtyClientHandle),
}

impl PtyHandleKind {
    pub fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            PtyHandleKind::Server(server) => server.clone().read(buf),
            PtyHandleKind::Client(client) => client.clone().read(buf),
        }
    }
}

impl PtyHandleKind {
    pub fn write(&self, buf: &[u8]) -> std::io::Result<usize> {
        tracing::info!("WRITE");
        match self {
            PtyHandleKind::Server(server) => server.clone().write(buf),
            PtyHandleKind::Client(client) => client.clone().write(buf),
        }
    }

    pub fn flush(&self) -> std::io::Result<()> {
        match self {
            PtyHandleKind::Server(server) => server.clone().flush(),
            PtyHandleKind::Client(client) => client.clone().flush(),
        }
    }
}
