use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
};
use twizzler_runtime_api::MapFlags;

use crate::{context::engine::ContextEngine, library::BackingData, DynlinkError, DynlinkErrorKind};

#[derive(Clone)]
pub struct Backing {
    obj: twizzler_runtime_api::ObjectHandle,
}

impl BackingData for Backing {
    fn data(&self) -> (*mut u8, usize) {
        (
            unsafe { self.obj.start.add(NULLPAGE_SIZE) },
            MAX_SIZE - NULLPAGE_SIZE * 2,
        )
    }

    fn new_data() -> Result<Self, DynlinkError> {
        let runtime = twizzler_runtime_api::get_runtime();
        let id = sys_object_create(
            ObjectCreate::new(
                BackingType::Normal,
                LifetimeType::Volatile,
                None,
                ObjectCreateFlags::empty(),
            ),
            &[],
            &[],
        )
        .map_err(|_| DynlinkErrorKind::NewBackingFail)?;
        let handle = runtime
            .map_object(id.as_u128(), MapFlags::READ | MapFlags::WRITE)
            .map_err(|_| DynlinkErrorKind::NewBackingFail)?;
        Ok(Self { obj: handle })
    }

    fn load_addr(&self) -> usize {
        self.obj.start as usize
    }

    type InnerType = twizzler_runtime_api::ObjectHandle;

    fn to_inner(self) -> Self::InnerType {
        self.obj
    }
}

pub struct Engine;

impl ContextEngine for Engine {
    type Backing = Backing;

    fn load_segments(
        &mut self,
        src: &Self::Backing,
        ld: &[crate::context::engine::LoadDirective],
    ) -> Result<Vec<Self::Backing>, DynlinkError> {
        todo!()
    }
}
