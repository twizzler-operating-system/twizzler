use alloc::sync::Arc;

use twizzler_abi::{
    object::{ObjID, Protections, MAX_SIZE},
    upcall::{
        MemoryAccessKind, MemoryContextViolationInfo, ObjectMemoryError, ObjectMemoryFaultInfo,
        UpcallInfo,
    },
};

use super::{region::MapRegion, PageFaultFlags, Slot};
use crate::{
    arch::VirtAddr,
    memory::{
        context::{kernel_context, ContextRef},
        frame::PHYS_LEVEL_LAYOUTS,
        pagetables::MappingSettings,
        tracker::{FrameAllocFlags, FrameAllocator},
    },
    obj::{pages::Page, range::PageStatus, PageNumber},
    security::{AccessInfo, PermsInfo, KERNEL_SCTX},
    thread::{current_memory_context, current_thread_ref},
};

#[allow(unused_variables)]
fn log_fault(addr: VirtAddr, cause: MemoryAccessKind, flags: PageFaultFlags, ip: VirtAddr) {
    //logln!("page-fault: {:?} {:?} {:?} ip={:?}", addr, cause, flags, ip);
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
    id: ObjID,
    _addr: VirtAddr,
    cause: MemoryAccessKind,
    ip: VirtAddr,
) -> Result<PermsInfo, UpcallInfo> {
    if ip.is_kernel() {
        return Ok(PermsInfo {
            ctx: KERNEL_SCTX,
            prot: Protections::all(),
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
        if perms.prot & access_kind == access_kind {
            return Ok(perms);
        }
        let perms = ct.secctx.search_access(&access_info);
        if perms.prot & access_kind != access_kind {
            todo!();
        }
        Ok(perms)
    } else {
        Ok(PermsInfo {
            ctx: KERNEL_SCTX,
            prot: Protections::all(),
        })
    }
}

fn page_fault_to_region(
    addr: VirtAddr,
    cause: MemoryAccessKind,
    _flags: PageFaultFlags,
    ip: VirtAddr,
    ctx: ContextRef,
    sctx_id: ObjID,
    info: MapRegion,
) -> Result<(), UpcallInfo> {
    let id = info.object.id();
    let page_number = PageNumber::from_address(addr);
    let is_kern_obj = addr.is_kernel_object_memory();

    check_object_addr(page_number, id, cause, addr)?;
    let perms = check_security(&ctx, id, addr, cause, ip)?;

    if perms.ctx != sctx_id {
        // TODO
        //logln!("switch {} {}", perms.ctx, sctx_id);
        //current_thread_ref().map(|ct| ct.secctx.switch_context(perms.ctx));
        //sctx_id = perms.ctx;
    }

    let mut obj_page_tree = info.object.lock_page_tree();
    if info.object.use_pager() {
        if matches!(obj_page_tree.try_get_page(page_number), PageStatus::NoPage) {
            drop(obj_page_tree);
            crate::pager::get_object_page(&info.object, page_number);
            obj_page_tree = info.object.lock_page_tree();
        }
    }

    let mut fa = FrameAllocator::new(
        FrameAllocFlags::ZEROED | FrameAllocFlags::WAIT_OK,
        PHYS_LEVEL_LAYOUTS[0],
    );

    let do_map = |page: Arc<Page>, cow: bool| {
        let settings = info.mapping_settings(cow, is_kern_obj);
        let settings = MappingSettings::new(
            settings.perms() & perms.prot,
            settings.cache(),
            settings.flags(),
        );
        let cursor = info.mapping_cursor(page_number.as_byte_offset(), PageNumber::PAGE_SIZE);
        ctx.with_arch(sctx_id, |arch| {
            arch.unmap(cursor);
            arch.map(cursor, &mut info.phys_provider(&page), &settings);
        });
    };

    let mut status =
        obj_page_tree.get_page(page_number, cause == MemoryAccessKind::Write, Some(&mut fa));

    if matches!(status, PageStatus::NoPage) && !info.object.use_pager() {
        let page = Page::new(fa.try_allocate().unwrap());
        obj_page_tree.add_page(page_number, page, Some(&mut fa));
        status =
            obj_page_tree.get_page(page_number, cause == MemoryAccessKind::Write, Some(&mut fa));
    }

    if let PageStatus::Ready(page, cow) = status {
        do_map(page, cow);
        Ok(())
    } else {
        todo!();
    }
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
        current_thread_ref().unwrap().send_upcall(upcall);
    }
}
