#[cfg(test)]
mod test {
    use phys_provider::PhysAddrProvider;
    use twizzler_kernel_macros::kernel_test;

    use crate::{
        arch::{address::VirtAddr, memory::pagetables::Table},
        memory::{
            context::MappingPerms,
            frame::PhysicalFrameFlags,
            map::CacheType,
            pagetables::{phys_provider, Mapper, MappingCursor, MappingFlags, MappingSettings},
        },
    };

    struct StaticProvider {}
    impl PhysAddrProvider for StaticProvider {
        fn peek(&mut self) -> (crate::arch::address::PhysAddr, usize) {
            (crate::arch::address::PhysAddr::new(0).unwrap(), usize::MAX)
        }

        fn consume(&mut self, _len: usize) {}
    }

    #[kernel_test]
    fn test_count() {
        let mut m =
            Mapper::new(crate::memory::alloc_frame(PhysicalFrameFlags::ZEROED).start_address());
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
        let mut m =
            Mapper::new(crate::memory::alloc_frame(PhysicalFrameFlags::ZEROED).start_address());
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
        let mut phys = StaticProvider {};
        let settings = MappingSettings::new(
            MappingPerms::WRITE | MappingPerms::READ,
            CacheType::WriteBack,
            MappingFlags::empty(),
        );
        m.map(cur, &mut phys, &settings);

        let mut reader = m.readmap(cur);
        let read = reader.nth(0).unwrap();
        assert_eq!(read.vaddr(), VirtAddr::new(0).unwrap());
        assert_eq!(read.psize(), page_size);
        assert_eq!(read.settings().cache(), settings.cache());
        assert_eq!(read.settings().perms(), settings.perms());
        assert_eq!(read.settings().flags(), settings.flags());

        assert_eq!(reader.next(), None);

        let settings2 = MappingSettings::new(
            MappingPerms::EXECUTE | MappingPerms::READ,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        );
        m.change(cur, &settings2);

        let mut reader = m.readmap(cur);
        let read = reader.nth(0).unwrap();
        assert_eq!(read.vaddr(), VirtAddr::new(0).unwrap());
        assert_eq!(read.psize(), page_size);
        assert_eq!(read.settings().cache(), settings2.cache());
        assert_eq!(read.settings().perms(), settings2.perms());
        assert_eq!(read.settings().flags(), settings2.flags());

        let _ = m.unmap(cur);

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
