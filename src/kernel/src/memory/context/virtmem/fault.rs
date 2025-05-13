use twizzler_abi::{
    object::MAX_SIZE,
    upcall::{
        MemoryAccessKind, MemoryContextViolationInfo, ObjectMemoryError, ObjectMemoryFaultInfo,
        UpcallInfo,
    },
};

use super::{PageFaultFlags, Slot};
use crate::{
    arch::VirtAddr,
    memory::{
        context::kernel_context,
        frame::PHYS_LEVEL_LAYOUTS,
        tracker::{FrameAllocFlags, FrameAllocator},
    },
    obj::{pages::Page, range::PageStatus, PageNumber},
    security::KERNEL_SCTX,
    thread::{current_memory_context, current_thread_ref},
};

pub fn page_fault(addr: VirtAddr, cause: MemoryAccessKind, flags: PageFaultFlags, ip: VirtAddr) {
    //logln!("page-fault: {:?} {:?} {:?} ip={:?}", addr, cause, flags, ip);
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
    } else {
        if flags.contains(PageFaultFlags::USER) && addr.is_kernel() {
            current_thread_ref()
                .unwrap()
                .send_upcall(UpcallInfo::MemoryContextViolation(
                    MemoryContextViolationInfo::new(addr.raw(), cause),
                ));
            return;
        }

        let mut sctx_id = current_thread_ref()
            .map(|ct| ct.secctx.active_id())
            .unwrap_or(KERNEL_SCTX);
        let user_ctx = current_memory_context();
        let (ctx, is_kern_obj) = if addr.is_kernel_object_memory() {
            assert!(!flags.contains(PageFaultFlags::USER));
            sctx_id = KERNEL_SCTX;
            (kernel_context(), true)
        } else {
            (user_ctx.as_ref().unwrap_or_else(||
            panic!("page fault in userland with no memory context at IP {:?} caused by {:?} to/from {:?} with flags {:?}, thread {}", ip, cause, addr, flags, current_thread_ref().map_or(0, |t| t.id()))), false)
        };
        let slot: Slot = match addr.try_into() {
            Ok(s) => s,
            Err(_) => {
                current_thread_ref()
                    .unwrap()
                    .send_upcall(UpcallInfo::MemoryContextViolation(
                        MemoryContextViolationInfo::new(addr.raw(), cause),
                    ));
                return;
            }
        };

        let page_number = PageNumber::from_address(addr);
        let mut slot_mgr = ctx.regions.lock();
        let info = slot_mgr.lookup_region(slot.start_vaddr()).cloned();
        drop(slot_mgr);
        if let Some(info) = info {
            let id = info.object.id();
            let null_upcall = UpcallInfo::ObjectMemoryFault(ObjectMemoryFaultInfo::new(
                id,
                ObjectMemoryError::NullPageAccess,
                cause,
                addr.into(),
            ));

            let oob_upcall = UpcallInfo::ObjectMemoryFault(ObjectMemoryFaultInfo::new(
                id,
                ObjectMemoryError::OutOfBounds(page_number.as_byte_offset()),
                cause,
                addr.into(),
            ));

            if info.object.use_pager() {
                let mut obj_page_tree = info.object.lock_page_tree();
                if matches!(obj_page_tree.try_get_page(page_number), PageStatus::NoPage) {
                    drop(obj_page_tree);
                    crate::pager::get_object_page(&info.object, page_number);
                }
            }

            let mut obj_page_tree = info.object.lock_page_tree();
            if page_number.is_zero() {
                // drop these mutexes in case upcall sending generetes a page fault.
                drop(obj_page_tree);
                current_thread_ref().unwrap().send_upcall(null_upcall);
                return;
            }
            if page_number.as_byte_offset() >= MAX_SIZE {
                // drop these mutexes in case upcall sending generetes a page fault.
                drop(obj_page_tree);
                current_thread_ref().unwrap().send_upcall(oob_upcall);
                return;
            }
            let mut fa = FrameAllocator::new(
                FrameAllocFlags::ZEROED | FrameAllocFlags::WAIT_OK,
                PHYS_LEVEL_LAYOUTS[0],
            );
            let status = obj_page_tree.get_page(
                page_number,
                cause == MemoryAccessKind::Write,
                Some(&mut fa),
            );
            if let PageStatus::Ready(page, cow) = status {
                ctx.with_arch(sctx_id, |arch| {
                    // TODO: don't need all three every time.
                    arch.unmap(
                        info.mapping_cursor(page_number.as_byte_offset(), PageNumber::PAGE_SIZE),
                    );
                    arch.map(
                        info.mapping_cursor(page_number.as_byte_offset(), PageNumber::PAGE_SIZE),
                        &mut info.phys_provider(&page),
                        &info.mapping_settings(cow, is_kern_obj),
                    );
                    arch.change(
                        info.mapping_cursor(page_number.as_byte_offset(), PageNumber::PAGE_SIZE),
                        &info.mapping_settings(cow, is_kern_obj),
                    );
                });
            } else if matches!(status, PageStatus::NoPage) {
                let page = Page::new(fa.try_allocate().unwrap());
                obj_page_tree.add_page(page_number, page, Some(&mut fa));
                let PageStatus::Ready(page, cow) = obj_page_tree.get_page(
                    page_number,
                    cause == MemoryAccessKind::Write,
                    Some(&mut fa),
                ) else {
                    panic!("unreachable");
                };
                ctx.with_arch(sctx_id, |arch| {
                    // TODO: don't need all three every time.
                    arch.unmap(
                        info.mapping_cursor(page_number.as_byte_offset(), PageNumber::PAGE_SIZE),
                    );
                    arch.map(
                        info.mapping_cursor(page_number.as_byte_offset(), PageNumber::PAGE_SIZE),
                        &mut info.phys_provider(&page),
                        &info.mapping_settings(cow, is_kern_obj),
                    );
                    arch.change(
                        info.mapping_cursor(page_number.as_byte_offset(), PageNumber::PAGE_SIZE),
                        &info.mapping_settings(cow, is_kern_obj),
                    );
                });
            }
        } else {
            current_thread_ref()
                .unwrap()
                .send_upcall(UpcallInfo::MemoryContextViolation(
                    MemoryContextViolationInfo::new(addr.raw(), cause),
                ));
        }
    }
}
