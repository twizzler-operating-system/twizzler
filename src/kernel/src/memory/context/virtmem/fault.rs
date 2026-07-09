use twizzler_abi::{
    object::{MAX_SIZE, ObjID, Protections},
    upcall::{
        MemoryAccessKind, MemoryContextViolationInfo, ObjectMemoryFaultInfo, SecurityViolationInfo,
        UpcallInfo,
    },
};
use twizzler_rt_abi::error::ObjectError;

use super::{PageFaultFlags, Slot, region::MapRegion};
use crate::{
    arch::VirtAddr,
    instant::Instant,
    memory::{
        FAULT_STATS,
        context::{ContextRef, kernel_context},
    },
    obj::PageNumber,
    security::{AccessInfo, KERNEL_SCTX, PermsInfo},
    thread::{current_memory_context, current_thread_ref},
};

#[allow(unused_variables)]
fn log_fault(addr: VirtAddr, cause: MemoryAccessKind, flags: PageFaultFlags, ip: VirtAddr) {
    FAULT_STATS
        .total
        .fetch_add(1, core::sync::atomic::Ordering::SeqCst);

    if flags.contains(PageFaultFlags::USER) && !ip.is_kernel() && !addr.is_kernel() {
        log::trace!("page-fault: {:?} {:?} {:?} ip={:?}", addr, cause, flags, ip);
    }

    if let Some(ct) = current_thread_ref() {
        let old = ct
            .last_pf_addr
            .swap(addr.raw(), core::sync::atomic::Ordering::SeqCst);
        if old == addr.raw() {
            log::debug!(
                "page-fault: {:?} {:?} {:?} ip={:?} (repeated fault)",
                addr,
                cause,
                flags,
                ip
            );
        }
    }
}

fn assert_valid(addr: VirtAddr, cause: MemoryAccessKind, flags: PageFaultFlags, ip: VirtAddr) {
    if flags.contains(PageFaultFlags::INVALID) {
        panic!("page table contains invalid bits for address {:?}", addr);
    }
    if !flags.contains(PageFaultFlags::USER) && cause == MemoryAccessKind::InstructionFetch {
        logln!(
            "==> {} {} {}",
            addr.is_kernel_object_memory(),
            addr.is_kernel(),
            ip.is_kernel()
        );
        panic!(
            "kernel page-fault at IP {:?} caused by {:?} to/from {:?} with flags {:?}",
            ip, cause, addr, flags
        );
    }
    if !flags.contains(PageFaultFlags::USER) && addr.is_kernel() && !addr.is_kernel_object_memory()
    {
        panic!(
            "kernel page-fault at IP {:?} caused by {:?} to/from {:?} with flags {:?}",
            ip, cause, addr, flags
        );
    }
}

fn check_violations(
    addr: VirtAddr,
    cause: MemoryAccessKind,
    flags: PageFaultFlags,
    _ip: VirtAddr,
) -> Result<(), UpcallInfo> {
    if flags.contains(PageFaultFlags::USER) && addr.is_kernel() {
        // info!("generating upcall, addr: {addr:?}, flags: {flags:?}");
        return Err(UpcallInfo::MemoryContextViolation(
            MemoryContextViolationInfo::new(addr.raw(), cause),
        ));
    }
    Ok(())
}

fn get_context(addr: VirtAddr, flags: PageFaultFlags) -> (ContextRef, ObjID) {
    let sctx_id = current_thread_ref()
        .map(|ct| ct.secctx.active_id())
        .unwrap_or(KERNEL_SCTX);
    let user_ctx = current_memory_context();
    if addr.is_kernel_object_memory() {
        assert!(!flags.contains(PageFaultFlags::USER));
        (kernel_context().clone(), KERNEL_SCTX)
    } else {
        (user_ctx.clone().unwrap(), sctx_id)
    }
}

fn check_object_addr(
    page_number: PageNumber,
    id: ObjID,
    cause: MemoryAccessKind,
    addr: VirtAddr,
) -> Result<(), UpcallInfo> {
    if page_number.is_zero() || page_number.as_byte_offset() >= MAX_SIZE {
        return Err(UpcallInfo::ObjectMemoryFault(ObjectMemoryFaultInfo::new(
            id,
            ObjectError::NotMapped.into(),
            cause,
            addr.into(),
        )));
    }
    Ok(())
}

fn check_security(
    ctx: &ContextRef,
    user_sctx: ObjID,
    id: ObjID,
    addr: VirtAddr,
    cause: MemoryAccessKind,
    ip: VirtAddr,
    default_prot: Protections,
) -> Result<PermsInfo, UpcallInfo> {
    if ip.is_kernel() || user_sctx.raw() == 0 {
        return Ok(PermsInfo {
            ctx: user_sctx,
            provide: Protections::all(),
            restrict: Protections::empty(),
        });
    }
    let exec_info = get_map_region(ip, ctx, MemoryAccessKind::InstructionFetch, ip)?;
    let access_kind = match cause {
        MemoryAccessKind::Read => Protections::READ,
        MemoryAccessKind::Write => Protections::WRITE | Protections::READ,
        MemoryAccessKind::InstructionFetch => Protections::EXEC | Protections::READ,
    };
    let access_info = AccessInfo {
        target_id: id,
        access_kind,
        exec_id: Some(exec_info.object().id()),
        exec_off: ip - exec_info.range.start,
    };
    if let Some(ct) = current_thread_ref() {
        let perms = ct.secctx.check_active_access(&access_info, default_prot);

        if perms.provide & !perms.restrict & access_kind == access_kind {
            return Ok(perms);
        }
        let perms = ct.secctx.search_access(&access_info, default_prot);
        if perms.provide & !perms.restrict & access_kind != access_kind {
            log::error!(
                "security violation: addr={:?}, cause={:?}, ip={:?}, perms={:?}, access_info={:?}",
                addr,
                cause,
                ip,
                perms,
                access_info
            );
            Err(UpcallInfo::SecurityViolation(SecurityViolationInfo {
                address: addr.raw(),
                access_kind: cause,
            }))
        } else {
            Ok(perms)
        }
    } else {
        Ok(PermsInfo {
            ctx: KERNEL_SCTX,
            provide: Protections::all(),
            restrict: Protections::empty(),
        })
    }
}

fn page_fault_to_region(
    addr: VirtAddr,
    cause: MemoryAccessKind,
    flags: PageFaultFlags,
    ip: VirtAddr,
    ctx: ContextRef,
    sctx_id: ObjID,
    info: MapRegion,
) -> Result<(), UpcallInfo> {
    let start_time = Instant::now();
    let id = info.object.id();
    let page_number = PageNumber::from_address(addr);

    // Step 1: Check for address validity and check for security violations.
    check_object_addr(page_number, id, cause, addr)?;

    let (_id_ok, default_prot) = info.object.check_id();

    // TODO: enforce id_ok

    let perms = check_security(&ctx, sctx_id, id.clone(), addr, cause, ip, default_prot)?;

    // Do we need to switch contexts?
    if perms.ctx != sctx_id {
        current_thread_ref().map(|ct| ct.secctx.switch_context(perms.ctx));
    }

    if let Err(e) = info.handle_fault(
        addr,
        ip,
        cause,
        flags,
        start_time,
        perms,
        default_prot,
        perms.ctx,
    ) {
        return Err(UpcallInfo::ObjectMemoryFault(ObjectMemoryFaultInfo::new(
            id,
            e,
            cause,
            addr.into(),
        )));
    }
    Ok(())
}

fn get_map_region(
    addr: VirtAddr,
    ctx: &ContextRef,
    cause: MemoryAccessKind,
    _ip: VirtAddr,
) -> Result<MapRegion, UpcallInfo> {
    let upcall =
        UpcallInfo::MemoryContextViolation(MemoryContextViolationInfo::new(addr.raw(), cause));
    let slot: Slot = addr.try_into().map_err(|_| upcall)?;
    let mut slot_mgr = ctx.regions.lock();
    if let Some(region) = slot_mgr.lookup_region(slot.start_vaddr()) {
        return Ok(region.clone());
    }
    drop(slot_mgr);
    let kctx = kernel_context();
    let mut k_regions = kctx.regions.lock();
    k_regions
        .lookup_region(slot.start_vaddr())
        .cloned()
        .ok_or(upcall)
}

pub fn do_page_fault(
    addr: VirtAddr,
    cause: MemoryAccessKind,
    flags: PageFaultFlags,
    ip: VirtAddr,
) -> Result<(), UpcallInfo> {
    log_fault(addr, cause, flags, ip);
    assert_valid(addr, cause, flags, ip);
    check_violations(addr, cause, flags, ip)?;

    let (ctx, sctx_id) = get_context(addr, flags);
    let info = get_map_region(addr, &ctx, cause, ip)?;
    page_fault_to_region(addr, cause, flags, ip, ctx, sctx_id, info)
}

pub fn page_fault(addr: VirtAddr, cause: MemoryAccessKind, flags: PageFaultFlags, ip: VirtAddr) {
    let res = do_page_fault(addr, cause, flags, ip);
    if flags.contains(PageFaultFlags::USER) && !ip.is_kernel() && !addr.is_kernel() {
        log::trace!(
            "done page-fault: {:?} {:?} {:?} ip={:?}",
            addr,
            cause,
            flags,
            ip
        );
    }
    if let Err(upcall) = res {
        current_thread_ref().unwrap().send_upcall(upcall);
    }
}
