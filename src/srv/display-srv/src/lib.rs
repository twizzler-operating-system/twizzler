#![feature(naked_functions)]
#![feature(portable_simd)]

use std::{
    sync::{Mutex, OnceLock, RwLock},
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
    config: RwLock<WindowConfig>,
}

#[allow(dead_code)]
struct DisplayInfo {
    gpu: DeviceWrapper<TwizzlerTransport>,
    fb: (*mut u32, usize),
    width: u32,
    height: u32,
    handles: Mutex<HandleMgr<DisplayClient>>,
    buffer: BufferObject,
}

unsafe impl Send for DisplayInfo {}
unsafe impl Sync for DisplayInfo {}

#[secgate::secure_gate]
pub fn start_display() -> Result<()> {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .without_time()
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
        buffer: BufferObject::create_new(current_info.0, current_info.1)?,
    });

    std::thread::spawn(compositor_thread);
    std::thread::spawn(render_thread);

    Ok(())
}

fn render_thread() {
    tracing::debug!("render thread started");
    let info = DISPLAY_INFO.get().unwrap();
    let fb = unsafe { core::slice::from_raw_parts_mut(info.fb.0, info.fb.1) };
    loop {
        let start = Instant::now();

        // We're the "compositor" here
        if info.buffer.has_data_for_compositor() {
            info.buffer.read_compositor_buffer(|buf, w, h| {
                let len = (w * h) as usize;
                assert_eq!(len, buf.len());
                assert!(fb.len() >= len, "{} {}, {} {}", fb.len(), len, w, h);
                (&mut fb[0..len]).copy_from_slice(buf);
                info.gpu.with_device(|g| g.flush().unwrap());
            });
            info.buffer.compositor_done(info.width, info.height);
        }

        let elapsed = start.elapsed();
        let remaining = Duration::from_millis(16).saturating_sub(elapsed);
        if elapsed.as_millis() > 0 {
            tracing::trace!(
                "render took {}ms, sleep for {}ms",
                elapsed.as_millis(),
                remaining.as_millis()
            );
        }
        std::thread::sleep(remaining);
    }
}

fn compositor_thread() {
    tracing::debug!("compositor thread started");
    let info = DISPLAY_INFO.get().unwrap();
    let mut last_window_count = 0;
    let mut done_fill = Instant::now();
    let mut done_comp = Instant::now();
    loop {
        let start = Instant::now();

        let handles = info.handles.lock().unwrap();
        let mut must_recomp = false;
        let mut updates = Vec::new();
        let mut this_window_count = 0;
        for h in handles.handles() {
            if h.2.window.has_data_for_compositor() {
                must_recomp = true;
                updates.push((h.0, h.1, h.2.config.read().unwrap().z));
            }
            this_window_count += 1;
        }
        let done_count = Instant::now();

        if this_window_count != last_window_count {
            must_recomp = true;
        }
        last_window_count = this_window_count;

        updates.sort_by_key(|u| u.2);

        if must_recomp {
            info.buffer.fill_current_buffer(|fbbuf, fbw, fbh| {
                fbbuf.fill(0);
                done_fill = Instant::now();
                for client in updates.iter().map(|u| handles.lookup(u.0, u.1).unwrap()) {
                    let client_wc = *client.config.read().unwrap();
                    client.window.read_compositor_buffer(|buf, mut w, mut h| {
                        if client_wc.y + h >= fbh {
                            h = fbh - client_wc.y;
                        }
                        if client_wc.x + w >= fbw {
                            w = fbw - client_wc.x;
                        }

                        // Copy each line. In the future, we can do alpha blending.
                        for y in 0..h {
                            let src = &buf[(y * w) as usize..];
                            let dst =
                                &mut fbbuf[((y + client_wc.y) * fbw + client_wc.x) as usize..];
                            (&mut dst[0..(w as usize)]).copy_from_slice(&src[0..(w as usize)]);
                        }
                    });
                    client.window.compositor_done(client_wc.w, client_wc.h);
                }
                done_comp = Instant::now();
            });
            info.buffer.flip();
        }
        drop(handles);

        let elapsed = start.elapsed();
        let remaining = Duration::from_millis(16).saturating_sub(elapsed);
        if elapsed.as_millis() > 0 {
            tracing::trace!(
                "composite took {}ms, sleep for {}ms (must_recomp = {}): {} {} {}",
                elapsed.as_millis(),
                remaining.as_millis(),
                must_recomp,
                (done_count - start).as_micros(),
                (done_fill - done_count).as_micros(),
                (done_comp - done_fill).as_micros(),
            );
        }
        std::thread::sleep(remaining);
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
                config: RwLock::new(winfo),
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

#[secgate::secure_gate(options(info))]
pub fn reconfigure_window(
    call_info: &GateCallInfo,
    handle: u32,
    wconfig: WindowConfig,
) -> Result<()> {
    tracing::debug!("reconfiguring window {} => {:?}", handle, wconfig);
    let info = DISPLAY_INFO.get().unwrap();
    let handles = info.handles.lock().unwrap();
    let client = handles
        .lookup(call_info.source_context().unwrap_or(0.into()), handle)
        .ok_or(TwzError::INVALID_ARGUMENT)?;
    *client.config.write().unwrap() = wconfig;
    Ok(())
}

#[secgate::secure_gate(options(info))]
pub fn get_window_config(call_info: &GateCallInfo, handle: u32) -> Result<WindowConfig> {
    let info = DISPLAY_INFO.get().unwrap();
    let handles = info.handles.lock().unwrap();
    let client = handles
        .lookup(call_info.source_context().unwrap_or(0.into()), handle)
        .ok_or(TwzError::INVALID_ARGUMENT)?;
    let w = client.config.read().unwrap();
    Ok(*w)
}

#[secgate::secure_gate]
pub fn get_display_info() -> Result<WindowConfig> {
    let info = DISPLAY_INFO.get().unwrap();
    Ok(WindowConfig {
        w: info.width,
        h: info.height,
        x: 0,
        y: 0,
        z: 0,
    })
}
