#[cfg(test)]
mod test {
    use twizzler_abi::{object::Protections, syscall::MapFlags};
    use twizzler_kernel_macros::kernel_test;

    use crate::{
        arch::{VirtAddr, context::ArchContext},
        memory::{
            context::{KernelMemoryContext, KernelObjectHandle, ObjectContextInfo, kernel_context},
            frame::PHYS_LEVEL_LAYOUTS,
            pagetables::{MappingCursor, MappingFlags, MappingSettings, Table},
            tracker::{FrameAllocFlags, FrameAllocator},
        },
        obj::pagetables::ObjectPageTable,
    };

    struct Foo {
        x: u32,
    }

    #[kernel_test]
    fn test_kernel_object() {
        let obj = crate::obj::Object::new_kernel();
        crate::obj::register_object(obj.clone());

        let ctx = kernel_context();
        let mut handle = ctx.insert_kernel_object(ObjectContextInfo::new(
            obj,
            Protections::READ | Protections::WRITE,
            twizzler_abi::device::CacheType::WriteBack,
            MapFlags::empty(),
        ));

        *handle.base_mut() = Foo { x: 42 };
    }

    /// An unmap is charged to the object whose table it actually released, not to whoever asked.
    /// If a slot is taken over between a region's removal and its unmap, the removal must leave the
    /// original object's map count alone -- decrementing it there is what underflowed the count and
    /// could reap a live object.
    #[kernel_test]
    fn test_object_unmap_charged_to_owner() {
        let a = crate::obj::Object::new_kernel();
        let b = crate::obj::Object::new_kernel();
        crate::obj::register_object(a.clone());
        crate::obj::register_object(b.clone());

        let arch = ArchContext::new();
        let cursor = MappingCursor::new(
            VirtAddr::new(twizzler_abi::object::MAX_SIZE as u64).unwrap(),
            twizzler_abi::object::MAX_SIZE,
        );
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            twizzler_abi::device::CacheType::WriteBack,
            MappingFlags::USER,
        );
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        fa.precharge(
            cursor.max_number_new_tables(Table::top_level(), ObjectPageTable::top_level() - 1) * 2,
            FrameAllocFlags::WAIT_OK,
        );

        let mut pt_a = a.lock_page_tables();
        arch.object_map(cursor, &mut pt_a, settings, &mut fa);
        assert_eq!(pt_a.map_count(), 1);
        let a_table = pt_a.context_table_addr();
        drop(pt_a);

        let mut pt_b = b.lock_page_tables();
        arch.object_map(cursor, &mut pt_b, settings, &mut fa);
        let b_table = pt_b.context_table_addr();
        assert_eq!(pt_b.map_count(), 1);
        drop(pt_b);
        assert_ne!(a_table, b_table);

        // This unmap removes b's entry, so a keeps its count and b loses one.
        assert!(!arch.unmap_object(cursor, a_table, &mut fa));
        assert_eq!(a.lock_page_tables().map_count(), 1);

        arch.object_map(cursor, &mut b.lock_page_tables(), settings, &mut fa);
        assert!(arch.unmap_object(cursor, b_table, &mut fa));
    }

    /// `insert_kernel_object` charges for its mapping before it knows which slot it will get, so a
    /// slot needing more tables than [`VirtContext::slot_map_tables`] reports would silently
    /// under-charge -- and an under-charged `map_object` falls through to a non-waiting allocation
    /// that can simply fail. Checked at the boundaries the count could plausibly differ at: the
    /// first and last slot of a top-level entry, and the first of the next one.
    #[kernel_test]
    fn test_slot_map_precharge_is_slot_independent() {
        use twizzler_abi::object::MAX_SIZE;

        use super::super::{Slot, VirtContext};

        let expected = VirtContext::slot_map_tables();
        let per_top_entry = Table::level_to_page_size(Table::top_level()) / MAX_SIZE;
        let last_user_slot = Slot::try_from(VirtAddr::end_user_memory().offset(-1isize).unwrap())
            .unwrap()
            .raw();
        for slot in [
            0,
            1,
            per_top_entry - 1,
            per_top_entry,
            per_top_entry + 1,
            last_user_slot,
        ] {
            let Ok(slot) = Slot::try_from(slot) else {
                continue;
            };
            let cursor = MappingCursor::new(slot.start_vaddr(), MAX_SIZE);
            assert_eq!(
                cursor.max_number_new_tables(Table::top_level(), ObjectPageTable::top_level() - 1),
                expected,
                "slot at {:?} needs a different precharge",
                slot.start_vaddr(),
            );
        }
    }
}
