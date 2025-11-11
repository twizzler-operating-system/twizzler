#![feature(naked_functions)]

use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use display_core::{BufferObject, WindowConfig};
use secgate::{util::HandleMgr, GateCallInfo};
use tracing::Level;
use twizzler_abi::object::ObjID;
use twizzler_rt_abi::{error::TwzError, Result};
use virtio_gpu::{DeviceWrapper, TwizzlerTransport};

static DISPLAY_INFO: OnceLock<DisplayInfo> = OnceLock::new();

struct DisplayClient {
    window: BufferObject,
    x: u32,
    y: u32,
}

struct DisplayInfo {
    gpu: DeviceWrapper<TwizzlerTransport>,
    fb: (*mut u32, usize),
    width: u32,
    height: u32,
    handles: Mutex<HandleMgr<DisplayClient>>,
}

unsafe impl Send for DisplayInfo {}
unsafe impl Sync for DisplayInfo {}

#[secgate::secure_gate]
pub fn start_display() -> Result<()> {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG)
            .finish(),
    )
    .unwrap();

    if DISPLAY_INFO.get().is_some() {
        tracing::info!("cannot call start_display more than once");
        return Err(TwzError::NOT_SUPPORTED);
    }

    let (send, _) = std::sync::mpsc::channel();
    let Ok(gpu) = virtio_gpu::get_device(send) else {
        tracing::error!("failed to setup display, no supported display found");
        return Err(TwzError::NOT_SUPPORTED);
    };

    let Ok(current_info) = gpu.with_device(|g| g.resolution()) else {
        tracing::error!("failed to get display resolution");
        return Err(TwzError::NOT_SUPPORTED);
    };

    tracing::info!(
        "setting up display with resolution {}x{}",
        current_info.0,
        current_info.1
    );

    let Ok(fb) = gpu.with_device(|g| g.setup_framebuffer().map(|p| (p.as_mut_ptr(), p.len())))
    else {
        tracing::error!("failed to setup framebuffer");
        return Err(TwzError::NOT_SUPPORTED);
    };
    let fb = (fb.0.cast(), fb.1 / 4);

    let _ = DISPLAY_INFO.set(DisplayInfo {
        gpu,
        fb,
        width: current_info.0,
        height: current_info.1,
        handles: Mutex::new(HandleMgr::new(None)),
    });

    std::thread::spawn(compositor_thread);

    Ok(())
}

fn compositor_thread() {
    tracing::info!("compositor thread started");
    let info = DISPLAY_INFO.get().unwrap();
    let fb = unsafe { core::slice::from_raw_parts_mut(info.fb.0, info.fb.1) };
    let mut i = 0;
    loop {
        let start = Instant::now();

        let handles = info.handles.lock().unwrap();
        let mut did_work = false;
        for (_id, client) in handles.handles() {
            if client.window.has_data_for_compositor() {
                did_work = true;
                client.window.read_compositor_buffer(|buf, mut w, mut h| {
                    if client.y + h >= info.height {
                        h = info.height - client.y;
                    }
                    if client.x + w >= info.width {
                        w = info.width - client.x;
                    }
                    for y in 0..h {
                        for x in 0..w {
                            let val = buf[(y * w + x) as usize];
                            fb[((y + client.y) * info.width + (x + client.x)) as usize] = val;
                        }
                    }
                });
                client.window.compositor_done();
            }
        }
        drop(handles);

        if did_work {
            info.gpu.with_device(|g| g.flush().unwrap());
        }

        let elapsed = start.elapsed();
        let remaining = Duration::from_millis(16).saturating_sub(elapsed);
        tracing::trace!(
            "took {}ms, sleep for {}ms",
            elapsed.as_millis(),
            remaining.as_millis()
        );
        std::thread::sleep(remaining);
        i += 1;
    }
}

#[secgate::secure_gate(options(info))]
pub fn create_window(call_info: &GateCallInfo, winfo: WindowConfig) -> Result<(ObjID, u32)> {
    let info = DISPLAY_INFO.get().unwrap();
    let bo = BufferObject::create_new(winfo.w, winfo.h)?;
    let mut handles = info.handles.lock().unwrap();
    let handle = handles
        .insert(
            call_info.source_context().unwrap_or(0.into()),
            DisplayClient {
                window: bo.clone(),
                x: winfo.x,
                y: winfo.y,
            },
        )
        .ok_or(TwzError::INVALID_ARGUMENT)?;
    tracing::debug!("created window {:?} as handle {}", winfo, handle);
    Ok((bo.id(), handle))
}

#[secgate::secure_gate(options(info))]
pub fn drop_window(call_info: &GateCallInfo, handle: u32) -> Result<()> {
    tracing::debug!("dropping window {}", handle);
    let info = DISPLAY_INFO.get().unwrap();
    let mut handles = info.handles.lock().unwrap();
    handles
        .remove(call_info.source_context().unwrap_or(0.into()), handle)
        .ok_or(TwzError::INVALID_ARGUMENT)?;
    Ok(())
}
