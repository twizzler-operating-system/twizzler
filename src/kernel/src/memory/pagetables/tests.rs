#[cfg(test)]
mod test {
    use twizzler_kernel_macros::kernel_test;

    use crate::{
        arch::{address::VirtAddr, memory::pagetables::Table},
        memory::{
            context::MappingPerms,
            frame::PhysicalFrameFlags,
            map::CacheType,
            pagetables::{
                Mapper, MappingCursor, MappingFlags, MappingSettings, PhysAddrProvider, PhysFrame,
            },
        },
    };

    struct SimpleP {
        next: Option<PhysFrame>,
    }

    impl PhysAddrProvider for SimpleP {
        fn peek(&mut self) -> PhysFrame {
            if let Some(ref next) = self.next {
                return next.clone();
            } else {
                let f = crate::memory::alloc_frame(PhysicalFrameFlags::ZEROED);
                self.next = Some(PhysFrame::new(
                    f.start_address().as_u64().try_into().unwrap(),
                    f.size(),
                ));
                self.peek()
            }
        }

        fn consume(&mut self, _len: usize) {
            self.next = None;
        }
    }

    #[kernel_test]
    fn test_count() {
        let mut m = Mapper::new(
            crate::memory::alloc_frame(PhysicalFrameFlags::ZEROED)
                .start_address()
                .as_u64()
                .try_into()
                .unwrap(),
        );
        for i in 0..Table::PAGE_TABLE_ENTRIES {
            let c = m.root().read_count();
            assert_eq!(c, i);
            m.root_mut().set_count(i + 1);
            let c = m.root().read_count();
            assert_eq!(c, i + 1);
        }
    }

    #[kernel_test]
    fn test_mapper() {
        let mut m = Mapper::new(
            crate::memory::alloc_frame(PhysicalFrameFlags::ZEROED)
                .start_address()
                .as_u64()
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            m.readmap(MappingCursor::new(VirtAddr::new(0).unwrap(), 0))
                .next(),
            None
        );
        assert_eq!(
            m.readmap(MappingCursor::new(VirtAddr::new(0).unwrap(), 0x1000 * 100))
                .next(),
            None
        );

        // TODO: magic numbers
        let cur = MappingCursor::new(VirtAddr::new(0).unwrap(), 0x1000);
        let mut phys = SimpleP { next: None };
        let settings = MappingSettings::new(
            MappingPerms::WRITE | MappingPerms::READ,
            CacheType::WriteBack,
            MappingFlags::empty(),
        );
        m.map(cur, &mut phys, &settings);

        let mut reader = m.readmap(cur);
        let read = reader.nth(0).unwrap();
        assert_eq!(read.vaddr(), VirtAddr::new(0).unwrap());
        assert_eq!(read.psize(), 0x1000);
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
        assert_eq!(read.psize(), 0x1000);
        assert_eq!(read.settings().cache(), settings2.cache());
        assert_eq!(read.settings().perms(), settings2.perms());
        assert_eq!(read.settings().flags(), settings2.flags());

        m.unmap(cur);

        let mut reader = m.readmap(cur);
        assert_eq!(reader.next(), None);
    }
}
