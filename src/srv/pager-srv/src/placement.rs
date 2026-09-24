//! Where each worker, and the nvme queue interrupt it reaps, runs.
//!
//! Worker `i` owns queue pair `i` (see `threads::nr_workers`), so both are placed by the same
//! function: the interrupt lands on the cpu its waiter sleeps on, and the wake is local.

use std::sync::OnceLock;

use twizzler_abi::{
    object::ObjID,
    syscall::{sys_cpu_info, sys_info, sys_thread_set_affinity, CpuInfo, CpuMask},
};

/// Chosen at boot with `--pager-pin=`, which init exports as `TWZ_PAGER_PIN`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinMode {
    /// Interrupts to the BSP, workers unconstrained.
    None,
    /// Worker and its queue interrupt on one cpu.
    Hard,
    /// Interrupt on the worker's home cpu; the worker may run anywhere sharing its last-level
    /// cache.
    Soft,
}

pub fn pin_mode() -> PinMode {
    static MODE: OnceLock<PinMode> = OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("TWZ_PAGER_PIN").as_deref() {
        Ok("hard") => PinMode::Hard,
        Ok("soft") => PinMode::Soft,
        _ => PinMode::None,
    })
}

fn cpus() -> &'static [CpuInfo] {
    static CPUS: OnceLock<Vec<CpuInfo>> = OnceLock::new();
    CPUS.get_or_init(|| {
        (0..sys_info().cpu_count().get())
            .map_while(|i| sys_cpu_info(i).ok())
            .collect()
    })
}

/// The cpu worker `index`'s queue interrupt is delivered to; `None` leaves it to the kernel.
pub fn worker_cpu(index: usize) -> Option<u32> {
    if pin_mode() == PinMode::None || cpus().is_empty() {
        return None;
    }
    Some(cpus()[index % cpus().len()].id)
}

fn worker_mask(index: usize) -> Option<CpuMask> {
    let home = cpus().get(index % cpus().len().max(1))?;
    match pin_mode() {
        PinMode::None => None,
        PinMode::Hard => Some(CpuMask::single(home.id)),
        PinMode::Soft => {
            let llc = home.caches().last()?.id;
            let mut mask = CpuMask::empty();
            for cpu in cpus() {
                if cpu.caches().last().is_some_and(|c| c.id == llc) {
                    mask.insert(cpu.id);
                }
            }
            Some(mask)
        }
    }
}

/// Apply worker `index`'s affinity to the calling thread.
pub fn pin_current_worker(index: usize) {
    let Some(mask) = worker_mask(index) else {
        return;
    };
    match sys_thread_set_affinity(ObjID::new(0), &mask) {
        Ok(()) => tracing::info!(
            "pager worker {} pinned ({:?}): home cpu {:?}, {} cpus allowed",
            index,
            pin_mode(),
            worker_cpu(index),
            mask.count()
        ),
        Err(e) => tracing::warn!("failed to pin pager worker {}: {}", index, e),
    }
}
