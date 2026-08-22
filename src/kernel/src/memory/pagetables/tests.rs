#[cfg(test)]
mod test {
    use phys_provider::PhysAddrProvider;
    use twizzler_abi::{device::CacheType, object::Protections};
    use twizzler_kernel_macros::kernel_test;

    use crate::{
        arch::{address::VirtAddr, memory::pagetables::Table},
        memory::{
            frame::PHYS_LEVEL_LAYOUTS,
            pagetables::{
                Mapper, MappingCursor, MappingFlags, MappingSettings, PhysMapInfo,
                consistency::Consistency, phys_provider, table::nonleaf_cow,
            },
            tracker::{FrameAllocFlags, FrameAllocator, alloc_frame},
        },
        obj::PageNumber,
        userinit::create_blank_object,
    };

    struct StaticProvider {
        settings: MappingSettings,
    }
    impl PhysAddrProvider for StaticProvider {
        fn peek(&mut self) -> Option<PhysMapInfo> {
            Some(PhysMapInfo {
                frame: None,
                addr: crate::arch::address::PhysAddr::new(0).unwrap(),
                len: usize::MAX,
                settings: self.settings,
            })
        }

        fn consume(&mut self, _len: usize) {}
    }

    #[kernel_test]
    fn test_count() {
        let mut m = Mapper::new(alloc_frame(FrameAllocFlags::ZEROED).start_address());
        for i in 0..Table::PAGE_TABLE_ENTRIES {
            let c = m.root().read_count();
            assert_eq!(c, i);
            m.root_mut().set_count(i + 1);
            let c = m.root().read_count();
            assert_eq!(c, i + 1);
        }
    }

    fn test_mapper_at_level(level: usize) {
        if !Table::can_map_at_level(level) {
            // This system doesn't support leaves at this level.
            return;
        }
        let page_size = Table::level_to_page_size(level);
        let mut m = Mapper::new(alloc_frame(FrameAllocFlags::ZEROED).start_address());
        assert_eq!(
            m.readmap(MappingCursor::new(VirtAddr::new(0).unwrap(), 0))
                .next(),
            None
        );
        assert_eq!(
            m.readmap(MappingCursor::new(
                VirtAddr::new(0).unwrap(),
                page_size * 100
            ))
            .next(),
            None
        );
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL,
            PHYS_LEVEL_LAYOUTS[0],
        );

        let len = page_size;
        let cur = MappingCursor::new(VirtAddr::new(0).unwrap(), len);
        let settings = MappingSettings::new(
            Protections::WRITE | Protections::READ,
            CacheType::WriteBack,
            MappingFlags::empty(),
        );
        let mut phys = StaticProvider { settings };
        let mut consist = Consistency::new_full_global();
        consist.set_full_global();
        let _ = m.map(cur, &mut phys, &mut consist, &mut fa);
        consist.tlb_mut().finish();

        let mut reader = m.readmap(cur);
        let read = reader.nth(0).unwrap();
        assert_eq!(read.vaddr(), VirtAddr::new(0).unwrap());
        assert_eq!(read.len(), page_size);
        assert_eq!(read.settings().cache(), settings.cache());
        assert_eq!(read.settings().perms(), settings.perms());
        assert_eq!(read.settings().flags(), settings.flags());

        assert_eq!(reader.next(), None);

        let settings2 = MappingSettings::new(
            Protections::EXEC | Protections::READ,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        );
        let mut consist = Consistency::new_full_global();
        m.change(cur, &settings2, &mut consist, &mut fa).unwrap();
        consist.tlb_mut().finish();

        let mut reader = m.readmap(cur);
        let read = reader.nth(0).unwrap();
        assert_eq!(read.vaddr(), VirtAddr::new(0).unwrap());
        assert_eq!(read.len(), page_size);
        assert_eq!(read.settings().cache(), settings2.cache());
        assert_eq!(read.settings().perms(), settings2.perms());
        assert_eq!(read.settings().flags(), settings2.flags());

        let mut consist = Consistency::new_full_global();
        m.unmap(cur, &mut consist, &mut fa, &mut None).unwrap();
        consist.tlb_mut().finish();
        consist.into_deferred().run_all();

        let mut reader = m.readmap(cur);
        assert_eq!(reader.next(), None);
    }

    #[kernel_test]
    fn test_mapper_levels() {
        for i in 0..Table::top_level() {
            test_mapper_at_level(i);
        }
    }

    /// The non-leaf arm of `Table::do_cow_copy` must stay reachable.
    ///
    /// That arm clears `WRITE` across a whole COW-shared sub-table and invalidates wholesale. The
    /// bug it was fixed for -- a stale writable entry surviving the downgrade -- cannot be tested
    /// directly: observing it needs a multi-cpu race with a window that cannot be scheduled. What
    /// can be pinned down is that the path still executes, so that a green run over it means
    /// something rather than nothing. See TLB.md.
    ///
    /// **This test asserted the wrong thing first, and the mistake is worth keeping written down.**
    /// It opened with `assert!(nonleaf_cow::calls() > 0)`, on TLB.md's measurement of 11 calls per
    /// boot -- byte-identical across four boots and both profiles, so seemingly the safest claim
    /// available. It failed 12/12, deterministically, in every config. The 11 calls are counted at
    /// *shutdown*, by the `Syscall::Null` dump arm, and they come from userspace object cloning;
    /// kernel tests run before any of that exists, so at test time the counter is legitimately
    /// zero. The number was real, the assertion on it was not: a count is only meaningful together
    /// with when it is read. Asserting a shutdown-time measurement at test time is the same error
    /// as reading an empty bucket as a maximum -- see unmap.md.
    ///
    /// What replaced it is a deliberate driver, which does not depend on when it runs. Sharing a
    /// whole 2 MiB region by reference marks its level-0 table COW, and the write after it resolves
    /// that through this arm. The delta was first only reported, not asserted, in case the failure
    /// mode was this test's model of which level `setup_cow_range` shares at rather than the
    /// kernel -- a failing kernel test halts the machine, so an ambiguous signal must not assert.
    /// The `cowtest-b` sweep then showed it firing identically in all 12 runs across all four
    /// configs, `1 call, 2 entries` every time -- two entries being exactly the two pages populated
    /// below, so the model is confirmed rather than merely non-zero. It asserts now.
    #[kernel_test]
    fn nonleaf_cow_arm_is_reached() {
        let region = PHYS_LEVEL_LAYOUTS[1].size();
        let src = create_blank_object();
        let dst = create_blank_object();

        // Populate two pages inside one 2 MiB region, so the level-0 table beneath that region's
        // entry has entries to downgrade. Region 1 rather than 0: page 0 is the null page.
        src.write_at(&0xa5u8, region).unwrap();
        src.write_at(&0x5au8, region + PageNumber::PAGE_SIZE)
            .unwrap();

        // Share the region by reference. The outcome that matters is the level-0 table frame being
        // marked COW rather than its pages being copied -- the arm's precondition.
        src.cow_copy(&dst, region, region, region).unwrap();

        let before = nonleaf_cow::calls();
        let entries_before = nonleaf_cow::entries();
        // A write through the shared table has to resolve it: do_cow_copy at level > 0.
        src.write_at(&0xffu8, region).unwrap();
        let calls = nonleaf_cow::calls() - before;
        let entries = nonleaf_cow::entries() - entries_before;
        emerglogln!(
            "== nonleaf cow driver: {} calls, {} entries",
            calls,
            entries
        );
        assert!(
            calls > 0,
            "the non-leaf COW arm did not fire: sharing a 2 MiB region by reference no longer \
             marks its sub-table COW, or a write no longer resolves it here. The arm is what \
             invalidates a whole downgraded sub-table, so nothing else covers it"
        );

        // COW correctness itself is not a model question, so it does assert: the write must land
        // in the source and must not be visible through the copy.
        let val: u8 = src.read_at(region).unwrap();
        assert_eq!(
            val, 0xff,
            "write after COW share did not land in the source"
        );
        let val: u8 = dst.read_at(region).unwrap();
        assert_eq!(
            val, 0xa5,
            "write after COW share leaked into the destination -- the copy did not happen"
        );
    }
}
