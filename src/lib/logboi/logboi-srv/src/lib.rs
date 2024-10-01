#![feature(naked_functions)]

use std::sync::Mutex;

use secgate::{
    secure_gate,
    util::{Descriptor, HandleMgr, SimpleBuffer},
};
use twizzler_abi::{
    object::ObjID,
    syscall::{
        sys_kernel_console_write, sys_object_create, KernelConsoleWriteFlags, ObjectCreate,
        ObjectCreateFlags,
    },
};
use twizzler_runtime_api::{get_runtime, MapFlags};

extern crate twz_rt;

// Per-client metadata.
struct LogClient {
    buffer: SimpleBuffer,
}

impl LogClient {
    fn sbid(&self) -> ObjID {
        self.buffer.handle().id
    }
}

impl LogClient {
    fn new() -> Option<Self> {
        // Create and map a handle for the simple buffer.
        let id = sys_object_create(
            ObjectCreate::new(
                twizzler_abi::syscall::BackingType::Normal,
                twizzler_abi::syscall::LifetimeType::Volatile,
                None,
                ObjectCreateFlags::empty(),
            ),
            &[],
            &[],
        )
        .ok()?;
        let handle = get_runtime()
            .map_object(id, MapFlags::WRITE | MapFlags::READ)
            .ok()?;
        let buffer = SimpleBuffer::new(handle);
        Some(Self { buffer })
    }
}

// internal logging state, protected by a lock.
struct Logger {
    handles: HandleMgr<LogClient>,
}

impl Logger {
    const fn new() -> Self {
        Self {
            handles: HandleMgr::new(None),
        }
    }
}

struct LogBoi {
    inner: Mutex<Logger>,
}

static LOGBOI: LogBoi = LogBoi {
    inner: Mutex::new(Logger::new()),
};

#[secure_gate(options(info))]
pub fn logboi_open_handle(info: &secgate::GateCallInfo) -> Option<(Descriptor, ObjID)> {
    let mut logger = LOGBOI.inner.lock().ok()?;
    let client = LogClient::new()?;
    let id = client.sbid();
    let desc = logger
        .handles
        .insert(info.source_context().unwrap_or(0.into()), client)?;

    Some((desc, id))
}

#[secure_gate(options(info))]
pub fn logboi_close_handle(info: &secgate::GateCallInfo, desc: Descriptor) {
    let mut logger = LOGBOI.inner.lock().unwrap();
    logger
        .handles
        .remove(info.source_context().unwrap_or(0.into()), desc);
}

#[secure_gate(options(info))]
pub fn logboi_post(info: &secgate::GateCallInfo, desc: Descriptor, buf_len: usize) {
    let mut buf = vec![0u8; buf_len];
    let logger = LOGBOI.inner.lock().unwrap();
    let Some(client) = logger
        .handles
        .lookup(info.source_context().unwrap_or(0.into()), desc)
    else {
        return;
    };
    let len = client.buffer.read(&mut buf);
    let msg = format!("[log] {}\n", String::from_utf8_lossy(&buf[0..len]));
    let _ = sys_kernel_console_write(msg.as_bytes(), KernelConsoleWriteFlags::DISCARD_ON_FULL);
}
