#![feature(portable_simd)]
#![feature(lock_value_accessors)]

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock, RwLock,
    },
    time::{Duration, Instant},
};

use secgate::util::HandleMgr;
use tracing::Level;
use twizzler_abi::{
    object::ObjID,
    syscall::{
        sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
        ThreadSyncSleep, ThreadSyncWake,
    },
};
use twizzler_display::{BufferObject, Rect, WindowConfig};
use twizzler_rt_abi::{error::TwzError, Result};
use virtio_gpu::{DeviceWrapper, TwizzlerTransport};

static DISPLAY_INFO: OnceLock<DisplayInfo> = OnceLock::new();

struct DisplayClient {
    window: BufferObject,
    config: RwLock<WindowConfig>,
    new_config: RwLock<Option<WindowConfig>>,
}

#[allow(dead_code)]
struct DisplayInfo {
    gpu: DeviceWrapper<TwizzlerTransport>,
    fb: (*mut u32, usize),
    width: u32,
    height: u32,
    handles: Mutex<HandleMgr<DisplayClient>>,
    buffer: BufferObject,
    /// Bumped by every gate call that changes what the compositor would draw.
    ///
    /// The client buffer waitpoints cover a window whose *contents* moved, and nothing else. They
    /// cannot cover a window being created (it has no word the sleeping compositor is already
    /// waiting on), or reconfigured (which moves a window without touching its buffer), or the
    /// case of no windows at all (an empty wait set). Those three were what the 1s idle backstop
    /// was really for; this is the wake they were missing, so the loops can now block outright.
    windows: AtomicU64,
}

impl DisplayInfo {
    /// Sleep while the window set is unchanged from `gen`, which the caller must read *before*
    /// looking at the handles -- otherwise a change made during the scan is missed rather than
    /// cancelling the sleep.
    fn windows_waitpoint(&self, gen: u64) -> ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.windows as *const AtomicU64),
            gen,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }

    /// Announce a change to the window set and wake the compositor.
    fn windows_changed(&self) {
        self.windows.fetch_add(1, Ordering::SeqCst);
        let mut ops = [ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual(&self.windows as *const AtomicU64),
            usize::MAX,
        ))];
        let _ = sys_thread_sync(&mut ops, None);
    }
}

unsafe impl Send for DisplayInfo {}
unsafe impl Sync for DisplayInfo {}

#[secgate::entry(lib = "twizzler-display")]
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
        windows: AtomicU64::new(0),
    });

    let _ = std::thread::Builder::new()
        .name("compositor".into())
        .spawn(compositor_thread)
        .unwrap();
    let _ = std::thread::Builder::new()
        .name("render".into())
        .spawn(render_thread)
        .unwrap();

    Ok(())
}

impl DisplayClient {
    fn compute_client_damage(
        &self,
        config: WindowConfig,
        client_damage: &mut Vec<Rect>,
        damage: &[Rect],
    ) {
        for damage in damage {
            if damage.x >= config.x
                && damage.x < config.x + config.w
                && damage.y >= config.y
                && damage.y < config.y + config.h
            {
                client_damage.push(Rect::new(
                    damage.x - config.x,
                    damage.y - config.y,
                    damage.w.min(config.w - (damage.x - config.x)),
                    damage.h.min(config.h - (damage.y - config.y)),
                ));
            }
        }
    }
}

/// Frame-rate cap once there is something to draw.
const FRAME_MS: u64 = 16;

fn render_thread() {
    tracing::debug!("render thread started");
    let info = DISPLAY_INFO.get().unwrap();
    let fb = unsafe { core::slice::from_raw_parts_mut(info.fb.0, info.fb.1) };
    loop {
        let start = Instant::now();

        // We're the "compositor" here
        let did_render = info.buffer.has_data_for_read();
        if did_render {
            info.buffer.read_buffer(|buf, w, h| {
                let len = (w * h) as usize;
                assert_eq!(len, buf.len());
                assert!(fb.len() >= len, "{} {}, {} {}", fb.len(), len, w, h);
                for (i, damage) in buf.damage_rects().iter().enumerate() {
                    let damage = Rect::new(
                        damage.x,
                        damage.y,
                        damage.w.min(w - damage.x),
                        damage.h.min(h - damage.y),
                    );
                    tracing::debug!("render screen damage ({}): {:?}", i, damage);
                    for y in damage.y..(damage.y + damage.h) {
                        let start = (y * w + damage.x) as usize;
                        let src = &buf.as_slice()[start..(start + damage.w as usize)];
                        let dst = &mut fb[start..(start + damage.w as usize)];
                        dst.copy_from_slice(src);
                    }
                }
            });
            info.buffer.read_done(info.width, info.height);
            info.gpu.with_device(|g| g.flush().unwrap());
        }

        let elapsed = start.elapsed();
        if did_render {
            // Cap the frame rate, but only after actually drawing something.
            let remaining = Duration::from_millis(FRAME_MS).saturating_sub(elapsed);
            if elapsed.as_millis() > 0 {
                tracing::trace!(
                    "render took {}ms, sleep for {}ms",
                    elapsed.as_millis(),
                    remaining.as_millis()
                );
            }
            std::thread::sleep(remaining);
        } else if let Some(sleep) = info.buffer.read_waitpoint() {
            // Nothing to draw: block until the compositor flips a frame. Untimed, deliberately.
            // A periodic re-check cannot tell a complete wait set from a broken one -- every
            // missed wake becomes latency instead of a fault, permanently invisible -- and the
            // compositor's flip is the only thing that can ever give this thread work.
            let _ = sys_thread_sync(&mut [ThreadSync::new_sleep(sleep)], None);
        }
    }
}

fn compositor_thread() {
    tracing::debug!("compositor thread started");
    let info = DISPLAY_INFO.get().unwrap();
    let mut last_window_count = 0;
    let mut done_fill = Instant::now();
    let mut done_comp = Instant::now();
    let mut damage = Vec::new();
    let mut client_damage = Vec::new();
    let mut waits: Vec<ThreadSyncSleep> = Vec::new();
    loop {
        damage.clear();
        waits.clear();
        let start = Instant::now();
        // Before the scan below, so a window created or reconfigured while it runs cancels the
        // sleep at the bottom instead of being missed by it.
        let gen = info.windows.load(Ordering::SeqCst);

        let mut clients = Vec::new();
        let mut pending = false;
        let handles = info.handles.lock().unwrap();
        for h in handles.handles() {
            if let Some(new_config) = h.2.new_config.replace(None).unwrap() {
                let old_config = h.2.config.replace(new_config).unwrap();
                tracing::debug!("window changed: {:?} => {:?}", old_config, new_config);
                damage.push(Rect::from(new_config));
                damage.push(Rect::from(old_config));
            }
            let config = *h.2.config.read().unwrap();
            if h.2.window.has_data_for_read() {
                pending = true;
                h.2.window.read_buffer(|b, _, _| {
                    for dmg in b.damage_rects() {
                        damage.push(Rect::new(
                            config.x + dmg.x,
                            config.y + dmg.y,
                            dmg.w.min(config.w - dmg.x),
                            dmg.h.min(config.h - dmg.y),
                        ));
                    }
                });
            }
            // Collected here because `h.2` is only reachable under the lock; the sleep itself
            // happens after the drop below.
            if let Some(w) = h.2.window.read_waitpoint() {
                waits.push(w);
            }
            clients.push((h.2, config));
        }
        let done_count = Instant::now();

        clients.sort_by_key(|c| c.1.z);

        if clients.len() != last_window_count {
            tracing::debug!(
                "window count changed from {} to {}",
                last_window_count,
                clients.len()
            );
            damage.clear();
            damage.push(Rect::full());
        }

        // `pending`, not just `!damage.is_empty()`: a client can flip a buffer whose damage set is
        // empty, and that buffer still has to be consumed -- `read_done` below is what clears its
        // ready flag. Skip it and the flag stays set, so `read_waitpoint` keeps returning `None`
        // and the sleep at the bottom omits the one window that actually has work. The old 1s
        // backstop hid that as a stutter; without it, it would be a stall.
        let did_composite = !damage.is_empty() || pending;
        if did_composite {
            tracing::debug!("damage = {:?}", damage);
            info.buffer.update_buffer(|mut fbbuf, fbw, fbh| {
                if clients.len() != last_window_count {
                    fbbuf.as_slice_mut().fill(0);
                    fbbuf.damage(Rect::full());
                } else {
                    for dmg in damage.drain(..) {
                        fbbuf.damage(dmg);
                    }
                }
                done_fill = Instant::now();

                for client in &clients {
                    client_damage.clear();
                    client.0.compute_client_damage(
                        client.1,
                        &mut client_damage,
                        fbbuf.damage_rects(),
                    );
                    if !client_damage.is_empty() {
                        client.0.window.read_buffer(|buf, bufw, bufh| {
                            for damage in &client_damage {
                                let mut damage = Rect::new(
                                    damage.x,
                                    damage.y,
                                    damage.w.min(bufw - damage.x),
                                    damage.h.min(bufh - damage.y),
                                );
                                tracing::debug!("client damage {:?}", damage);

                                if client.1.y + damage.y >= fbh {
                                    continue;
                                }
                                if client.1.x + damage.x >= fbw {
                                    continue;
                                }
                                if client.1.y + damage.h >= fbh {
                                    damage.h = fbh - client.1.y;
                                }
                                if client.1.x + damage.w >= fbw {
                                    damage.w = fbw - client.1.x;
                                }

                                // Copy each line. In the future, we can do alpha blending.
                                for y in damage.y..(damage.y + damage.h) {
                                    let src = &buf.as_slice()[(y * bufw) as usize..];
                                    let dst = &mut fbbuf.as_slice_mut()
                                        [((y + client.1.y) * fbw + client.1.x) as usize..];
                                    (&mut dst[0..(damage.w as usize)])
                                        .copy_from_slice(&src[0..(damage.w as usize)]);
                                }
                            }
                        });
                    }

                    if client.0.window.has_data_for_read() {
                        client.0.window.read_done(client.1.w, client.1.h);
                    }
                }

                done_comp = Instant::now();
            });
            info.buffer.flip();
        }
        last_window_count = clients.len();
        drop(clients);
        drop(handles);

        let elapsed = start.elapsed();
        if did_composite {
            let remaining = Duration::from_millis(FRAME_MS).saturating_sub(elapsed);
            if elapsed.as_millis() > 0 {
                tracing::trace!(
                    "composite took {}ms, sleep for {}ms : {} {} {}",
                    elapsed.as_millis(),
                    remaining.as_millis(),
                    (done_count - start).as_micros(),
                    (done_fill - done_count).as_micros(),
                    (done_comp - done_fill).as_micros(),
                );
            }
            std::thread::sleep(remaining);
        } else {
            // Nothing changed. Sleep on every client's buffer at once -- `sys_thread_sync` takes
            // the whole set and releases on the first to move -- so a window updating anywhere
            // wakes us immediately, and an idle screen costs nothing at all rather than 62 wakes
            // a second. `windows_waitpoint` completes the set: it is what a client *connecting*
            // wakes, and it keeps the set non-empty when there are no windows to sleep on.
            //
            // Waitpoints are collected after the handle lock is dropped: sleeping under it would
            // block every gate call into this compartment, including the one creating a window.
            let mut ops: Vec<ThreadSync> = waits.drain(..).map(ThreadSync::new_sleep).collect();
            ops.push(ThreadSync::new_sleep(info.windows_waitpoint(gen)));
            let _ = sys_thread_sync(&mut ops, None);
        }
    }
}

#[secgate::entry(lib = "twizzler-display")]
pub fn create_window(winfo: WindowConfig) -> Result<(ObjID, u32)> {
    let call_info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let info = DISPLAY_INFO.get().unwrap();
    let bo = BufferObject::create_new(winfo.w, winfo.h)?;
    let mut handles = info.handles.lock().unwrap();
    let handle = handles
        .insert(
            call_info.source_context().unwrap_or(0.into()),
            DisplayClient {
                window: bo.clone(),
                config: RwLock::new(winfo),
                new_config: RwLock::new(None),
            },
        )
        .ok_or(TwzError::INVALID_ARGUMENT)?;
    tracing::debug!("created window {:?} as handle {}", winfo, handle);
    drop(handles);
    info.windows_changed();
    Ok((bo.id(), handle))
}

#[secgate::entry(lib = "twizzler-display")]
pub fn drop_window(handle: u32) -> Result<()> {
    tracing::debug!("dropping window {}", handle);
    let call_info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let info = DISPLAY_INFO.get().unwrap();
    let mut handles = info.handles.lock().unwrap();
    handles
        .remove(call_info.source_context().unwrap_or(0.into()), handle)
        .ok_or(TwzError::INVALID_ARGUMENT)?;
    drop(handles);
    info.windows_changed();
    Ok(())
}

#[secgate::entry(lib = "twizzler-display")]
pub fn reconfigure_window(handle: u32, wconfig: WindowConfig) -> Result<()> {
    tracing::debug!("reconfiguring window {} => {:?}", handle, wconfig);
    let call_info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let info = DISPLAY_INFO.get().unwrap();
    {
        let handles = info.handles.lock().unwrap();
        let client = handles
            .lookup(call_info.source_context().unwrap_or(0.into()), handle)
            .ok_or(TwzError::INVALID_ARGUMENT)?;
        *client.new_config.write().unwrap() = Some(wconfig);
    }
    info.windows_changed();
    Ok(())
}

#[secgate::entry(lib = "twizzler-display")]
pub fn get_window_config(handle: u32) -> Result<WindowConfig> {
    let info = DISPLAY_INFO.get().unwrap();
    let call_info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let handles = info.handles.lock().unwrap();
    let client = handles
        .lookup(call_info.source_context().unwrap_or(0.into()), handle)
        .ok_or(TwzError::INVALID_ARGUMENT)?;
    let w = client.config.read().unwrap();
    Ok(*w)
}

#[secgate::entry(lib = "twizzler-display")]
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
