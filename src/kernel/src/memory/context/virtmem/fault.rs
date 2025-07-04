use twizzler_abi::{
    object::{ObjID, Protections, MAX_SIZE},
    upcall::{
        MemoryAccessKind, MemoryContextViolationInfo, ObjectMemoryError, ObjectMemoryFaultInfo,
        SecurityViolationInfo, UpcallInfo,
    },
};

use super::{region::MapRegion, ObjectPageProvider, PageFaultFlags, Slot};
use crate::{
    arch::VirtAddr,
    memory::context::{kernel_context, ContextRef},
    obj::PageNumber,
    security::{AccessInfo, PermsInfo, KERNEL_SCTX},
    thread::{current_memory_context, current_thread_ref},
};

#[allow(unused_variables)]
fn log_fault(addr: VirtAddr, cause: MemoryAccessKind, flags: PageFaultFlags, ip: VirtAddr) {
    // logln!("page-fault: {:?} {:?} {:?} ip={:?}", addr, cause, flags, ip);
}

fn assert_valid(addr: VirtAddr, cause: MemoryAccessKind, flags: PageFaultFlags, ip: VirtAddr) {
    if flags.contains(PageFaultFlags::INVALID) {
        panic!("page table contains invalid bits for address {:?}", addr);
    }
    if !flags.contains(PageFaultFlags::USER) && cause == MemoryAccessKind::InstructionFetch {
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
    if page_number.is_zero() {
        return Err(UpcallInfo::ObjectMemoryFault(ObjectMemoryFaultInfo::new(
            id,
            ObjectMemoryError::NullPageAccess,
            cause,
            addr.into(),
        )));
    }

    if page_number.as_byte_offset() >= MAX_SIZE {
        return Err(UpcallInfo::ObjectMemoryFault(ObjectMemoryFaultInfo::new(
            id,
            ObjectMemoryError::OutOfBounds(page_number.as_byte_offset()),
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
    if ip.is_kernel() {
        return Ok(PermsInfo {
            ctx: user_sctx,
            provide: Protections::all(),
            restrict: Protections::empty(),
        });
    }
    let exec_info = get_map_region(ip, ctx, MemoryAccessKind::InstructionFetch)?;
    let access_kind = match cause {
        MemoryAccessKind::Read => Protections::READ,
        MemoryAccessKind::Write => Protections::WRITE | Protections::READ,
        MemoryAccessKind::InstructionFetch => Protections::EXEC,
    };
    let access_info = AccessInfo {
        target_id: id,
        access_kind,
        exec_id: Some(exec_info.object().id()),
        exec_off: ip - exec_info.range.start,
    };
    if let Some(ct) = current_thread_ref() {
        let perms = ct.secctx.check_active_access(&access_info);
        if (perms.provide | default_prot) & !perms.restrict & access_kind == access_kind {
            return Ok(perms);
        }
        let perms = ct.secctx.search_access(&access_info);
        if (perms.provide | default_prot) & !perms.restrict & access_kind != access_kind {
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
    _flags: PageFaultFlags,
    ip: VirtAddr,
    ctx: ContextRef,
    mut sctx_id: ObjID,
    info: MapRegion,
) -> Result<(), UpcallInfo> {
    let id = info.object.id();
    let page_number = PageNumber::from_address(addr);

    // Step 1: Check for address validity and check for security violations.
    check_object_addr(page_number, id, cause, addr)?;

    let (id_ok, default_prot) = info.object.check_id();
    if !id_ok && !info.object().is_kernel_id() {
        /*
        logln!("ObjId: {:?}, default protections: {:?} ", id, default_prot);
        logln!(
            "id verification failed ({} {}) {:?}",
            info.object.use_pager(),
            info.object.is_kernel_id(),
            info.object.id(),
        );
        */
    }

    let perms = check_security(&ctx, sctx_id, id.clone(), addr, cause, ip, default_prot)?;

    // Do we need to switch contexts?
    if perms.ctx != sctx_id && !addr.is_kernel() {
        current_thread_ref().map(|ct| ct.secctx.switch_context(perms.ctx));
        sctx_id = perms.ctx;
    }

    let mapper = |mut provider: ObjectPageProvider| {
        let cursor = info.mapping_cursor(
            page_number.as_byte_offset(),
            PageNumber::PAGE_SIZE * provider.count(),
        );
        ctx.with_arch(sctx_id, |arch| {
            arch.unmap(cursor);
            arch.map(cursor, &mut provider);
        });
        Ok(())
    };

    info.map(addr, cause, perms, default_prot, mapper)
}

fn get_map_region(
    addr: VirtAddr,
    ctx: &ContextRef,
    cause: MemoryAccessKind,
) -> Result<MapRegion, UpcallInfo> {
    let upcall =
        UpcallInfo::MemoryContextViolation(MemoryContextViolationInfo::new(addr.raw(), cause));
    let slot: Slot = addr.try_into().map_err(|_| upcall)?;
    let mut slot_mgr = ctx.regions.lock();
    slot_mgr
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
    let info = get_map_region(addr, &ctx, cause)?;
    page_fault_to_region(addr, cause, flags, ip, ctx, sctx_id, info)
}

pub fn page_fault(addr: VirtAddr, cause: MemoryAccessKind, flags: PageFaultFlags, ip: VirtAddr) {
    let res = do_page_fault(addr, cause, flags, ip);
    if let Err(upcall) = res {
        logln!("UpCall:{:?}", upcall);
        current_thread_ref().unwrap().send_upcall(upcall);
    }
}
