#[cfg(test)]
mod test {
    use phys_provider::PhysAddrProvider;
    use twizzler_abi::{device::CacheType, object::Protections};
    use twizzler_kernel_macros::kernel_test;

    use crate::{
        arch::{address::VirtAddr, memory::pagetables::Table},
        memory::{
            pagetables::{
                consistency::Consistency, phys_provider, Mapper, MappingCursor, MappingFlags,
                MappingSettings, PhysMapInfo,
            },
            tracker::{alloc_frame, FrameAllocFlags},
        },
    };

    struct StaticProvider {
        settings: MappingSettings,
    }
    impl PhysAddrProvider for StaticProvider {
        fn peek(&mut self) -> Option<PhysMapInfo> {
            Some(PhysMapInfo {
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
        let _ = m.map(cur, &mut phys, consist);

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
        m.change(cur, &settings2);

        let mut reader = m.readmap(cur);
        let read = reader.nth(0).unwrap();
        assert_eq!(read.vaddr(), VirtAddr::new(0).unwrap());
        assert_eq!(read.len(), page_size);
        assert_eq!(read.settings().cache(), settings2.cache());
        assert_eq!(read.settings().perms(), settings2.perms());
        assert_eq!(read.settings().flags(), settings2.flags());

        let d = m.unmap(cur);
        d.run_all();

        let mut reader = m.readmap(cur);
        assert_eq!(reader.next(), None);
    }

    #[kernel_test]
    fn test_mapper_levels() {
        for i in 0..Table::top_level() {
            test_mapper_at_level(i);
        }
    }
}
