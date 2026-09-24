#[cfg(test)]
mod test {
    use twizzler_abi::{device::CacheType, object::Protections};
    use twizzler_kernel_macros::kernel_test;

    use super::super::{Entry, EntryFlags, Table, memory_attr_manager};
    use crate::{
        arch::{
            address::{PhysAddr, VirtAddr},
            memory::{TranslateAs, hw_translate},
        },
        memory::{
            frame::{FrameRef, NR_LEVELS, PHYS_LEVEL_LAYOUTS},
            pagetables::{
                Consistency, Mapper, MappingCursor, MappingFlags, MappingSettings,
                PhysAddrProvider, PhysMapInfo,
            },
            tracker::{FrameAllocFlags, FrameAllocator, alloc_frame, free_frame},
        },
    };

    fn some_page() -> PhysAddr {
        PhysAddr::new(0x4020_0000).unwrap()
    }

    #[kernel_test]
    fn test_level_contract() {
        assert_eq!(Table::top_level(), 3);
        assert_eq!(Table::last_level(), 0);
        for level in 1..=Table::top_level() {
            assert_eq!(Table::next_level(level), level - 1);
        }
        for level in 0..NR_LEVELS {
            assert_eq!(
                Table::level_to_page_size(level),
                PHYS_LEVEL_LAYOUTS[level].size()
            );
            assert!(Table::can_map_at_level(level));
        }
        assert!(!Table::can_map_at_level(Table::top_level()));
    }

    #[kernel_test]
    fn test_get_index_roundtrip() {
        for level in 0..=Table::top_level() {
            for idx in [0usize, 1, 255, 511] {
                let va = VirtAddr::new((idx << (12 + 9 * level)) as u64).unwrap();
                for other in 0..=Table::top_level() {
                    let want = if other == level { idx } else { 0 };
                    assert_eq!(
                        Table::get_index(va, other),
                        want,
                        "level {} idx {}",
                        level,
                        idx
                    );
                }
            }
        }
    }

    #[kernel_test]
    fn test_entry_unused() {
        let e = Entry::new_unused();
        assert!(!e.is_present());
        assert!(!e.is_huge());
    }

    #[kernel_test]
    fn test_entry_huge_vs_table() {
        let leaf = EntryFlags::from(&MappingSettings::new(
            Protections::READ,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        ));
        assert!(!Entry::new(some_page(), EntryFlags::intermediate()).is_huge());
        assert!(!Entry::new(some_page(), leaf | EntryFlags::leaf()).is_huge());
        assert!(Entry::new(some_page(), leaf | EntryFlags::huge()).is_huge());
    }

    #[kernel_test]
    fn test_entry_perms_roundtrip() {
        let perms = [
            Protections::READ,
            Protections::READ | Protections::WRITE,
            Protections::READ | Protections::EXEC,
            Protections::READ | Protections::WRITE | Protections::EXEC,
        ];
        let flag_sets = [
            MappingFlags::empty(),
            MappingFlags::USER,
            MappingFlags::GLOBAL,
            MappingFlags::USER | MappingFlags::GLOBAL,
        ];
        let caches = [
            CacheType::WriteBack,
            CacheType::WriteCombining,
            CacheType::WriteThrough,
            CacheType::Uncacheable,
            CacheType::MemoryMappedIO,
        ];
        for p in perms {
            for f in flag_sets {
                for c in caches {
                    let slot = memory_attr_manager()
                        .attribute_index(c)
                        .unwrap_or_else(|| panic!("no MAIR slot for {:?}", c));
                    let s = MappingSettings::new(p, c, f);
                    let got = Entry::new(some_page(), EntryFlags::from(&s))
                        .flags()
                        .settings();
                    assert_eq!(got.perms(), p, "{:?}", s);
                    assert_eq!(got.flags(), f, "{:?}", s);
                    // Two cache types can share a MAIR attribute; the slot is what must survive.
                    assert_eq!(
                        memory_attr_manager().attribute_index(got.cache()),
                        Some(slot),
                        "{:?}",
                        s
                    );
                }
            }
        }
    }

    /// The COW write-protect path: dropping `WRITE` must make the hardware entry read-only, both
    /// for an entry the kernel built and for a bootloader-style one with AP[2] clear and no
    /// software `WRITE` bit.
    #[kernel_test]
    fn test_entry_write_bit() {
        let ap2 = EntryFlags::AP2_READ_OR_RW.bits();
        let rw = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::WriteBack,
            MappingFlags::USER,
        );
        let mut e = Entry::new(some_page(), EntryFlags::from(&rw));
        assert!(e.flags().settings().perms().contains(Protections::WRITE));
        // Clean: read-only in hardware until the first write dirties it.
        assert_ne!(e.raw() & ap2, 0);
        e.set_flags(e.flags() | EntryFlags::DIRTY);
        assert_eq!(e.raw() & ap2, 0);
        e.set_flags(e.flags() - EntryFlags::WRITE);
        assert!(!e.flags().settings().perms().contains(Protections::WRITE));
        assert_ne!(e.raw() & ap2, 0);

        let mut boot = Entry::new(some_page(), EntryFlags::empty());
        boot = unsafe { core::mem::transmute::<u64, Entry>(boot.raw() & !ap2) };
        assert!(!boot.flags().contains(EntryFlags::WRITE));
        assert!(boot.flags().settings().perms().contains(Protections::WRITE));
        boot.set_flags(boot.flags() - EntryFlags::WRITE);
        assert!(!boot.flags().settings().perms().contains(Protections::WRITE));
        assert_ne!(boot.raw() & ap2, 0);
    }

    #[kernel_test]
    fn test_object_table_flag() {
        let mut e = Entry::new(some_page(), EntryFlags::intermediate());
        assert!(!e.is_object_table());
        e.set_object_table(true);
        assert!(e.is_object_table());
        e.set_flags(e.flags());
        assert!(e.is_object_table());
        assert_eq!(e.table_addr(), some_page());
        e.set_object_table(false);
        assert!(!e.is_object_table());
    }

    struct OneFrame(Option<(FrameRef, MappingSettings)>);

    impl PhysAddrProvider for OneFrame {
        fn peek(&mut self) -> Option<PhysMapInfo> {
            self.0.map(|(frame, settings)| PhysMapInfo {
                addr: frame.start_address(),
                len: PHYS_LEVEL_LAYOUTS[0].size(),
                settings,
                frame: Some(frame),
            })
        }

        fn consume(&mut self, _len: usize) {
            self.0 = None;
        }
    }

    fn new_table() -> FrameRef {
        let frame = alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL);
        frame.set_pt(true);
        frame.inc_refcount();
        frame
    }

    fn drop_table(frame: FrameRef) {
        frame.set_pt(false);
        if frame.dec_refcount() == 0 {
            free_frame(frame);
        }
    }

    #[kernel_test]
    fn test_wired_not_freed() {
        let root = new_table();
        let mut m = Mapper::new(root.start_address());
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let page = alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL);
        let refs = page.refcount();
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::WriteBack,
            MappingFlags::WIRED,
        );
        let cur = MappingCursor::new(
            VirtAddr::new(0x1000_0000).unwrap(),
            PHYS_LEVEL_LAYOUTS[0].size(),
        );
        let mut phys = OneFrame(Some((page, settings)));
        let mut consist = Consistency::new_full_global();
        m.map(cur, &mut phys, &mut consist, &mut fa).unwrap();
        consist.tlb_mut().finish();
        assert_eq!(
            m.readmap(cur).next().map(|i| i.paddr()),
            Some(page.start_address())
        );

        let mut consist = Consistency::new_full_global();
        m.unmap(cur, &mut consist, &mut fa, &mut None).unwrap();
        consist.tlb_mut().finish();
        consist.into_deferred().run_all();
        assert_eq!(m.readmap(cur).next(), None);
        assert_eq!(page.refcount(), refs);
        free_frame(page);
        drop_table(root);
    }

    /// The MMU's own walk of the live kernel tables agrees with the software walk.
    #[kernel_test]
    fn test_hw_agrees_with_readmap() {
        let checked = crate::arch::context::check_kernel_map(64);
        assert!(checked > 0);
    }

    /// Builds TTBR0 tables by hand -- root, two intermediates, a last-level table under a
    /// descriptor carrying `perms`, and one user page that is itself writable and dirty -- and
    /// asks the MMU whether EL0 may read and write it.
    fn el0_access_through_table(table_perms: Protections) -> (bool, bool) {
        let va = VirtAddr::new(0x10_0000_0000).unwrap();
        let tables = [new_table(), new_table(), new_table(), new_table()];
        let page = alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL);
        let table =
            |f: &FrameRef| unsafe { &mut *f.start_address().kernel_vaddr().as_mut_ptr::<Table>() };
        for (i, level) in (1..=3).rev().enumerate() {
            let mut flags = EntryFlags::intermediate();
            if level == 1 {
                flags.apply_perms(table_perms);
            }
            table(&tables[i])[Table::get_index(va, level)] =
                Entry::new(tables[i + 1].start_address(), flags);
        }
        let leaf = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::WriteBack,
            MappingFlags::USER,
        );
        table(&tables[3])[Table::get_index(va, 0)] = Entry::new(
            page.start_address(),
            EntryFlags::from(&leaf) | EntryFlags::DIRTY,
        );

        let old = crate::interrupt::disable();
        let prev: u64;
        let result = unsafe {
            core::arch::asm!(
                "dsb ishst",
                "mrs {p}, ttbr0_el1",
                "msr ttbr0_el1, {n}",
                "isb",
                "tlbi vmalle1",
                "dsb nsh",
                "isb",
                p = out(reg) prev,
                n = in(reg) tables[0].start_address().raw(),
            );
            let r = (
                hw_translate(va, false, TranslateAs::User).is_ok(),
                hw_translate(va, true, TranslateAs::User).is_ok(),
            );
            core::arch::asm!(
                "msr ttbr0_el1, {p}",
                "isb",
                "tlbi vmalle1",
                "dsb nsh",
                "isb",
                p = in(reg) prev,
            );
            r
        };
        crate::interrupt::set(old);
        free_frame(page);
        for t in tables {
            drop_table(t);
        }
        result
    }

    /// An object mapped read-only is read-only from EL0 even when the page below allows writes:
    /// ARM ignores a table descriptor's own permission bits, so `APTable` has to carry it.
    #[kernel_test]
    fn test_ro_object_not_writable() {
        assert_eq!(
            el0_access_through_table(Protections::READ | Protections::WRITE),
            (true, true)
        );
        assert_eq!(el0_access_through_table(Protections::READ), (true, false));
    }
}
