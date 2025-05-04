use core::fmt::Display;

use log::debug;
use map::{CtxMapItemType, SecCtxMap};
use twizzler::object::{Object, ObjectBuilder, TypedObject};
use twizzler_abi::object::ObjID;
use twizzler_rt_abi::{error::TwzError, object::MapFlags};

use crate::Cap;

pub mod map;

// ok we have access to an object, now what the fuck do we do.
//

// im assuming that there is some way for a process to
// get the object id for the security context its attached to?
pub struct SecCtx {
    uobj: Object<SecCtxMap>,
}

impl Default for SecCtx {
    fn default() -> Self {
        let obj = ObjectBuilder::default()
            .build(SecCtxMap::default())
            .unwrap();

        Self { uobj: obj }
    }
}

impl Display for SecCtx {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let binding = self.uobj.clone();
        let map = binding.base();

        write!(f, "Sec Ctx ObjID: {} {{\n", self.uobj.id())?;
        for (i, entry) in map.buf.into_iter().enumerate().take(map.len as usize) {
            write!(f, "Entry {}: {}\n", i, entry)?;
        }

        Ok(())
    }
}

impl TryFrom<ObjID> for SecCtx {
    type Error = TwzError;

    fn try_from(value: ObjID) -> Result<Self, Self::Error> {
        let uobj = Object::<SecCtxMap>::map(value, MapFlags::READ | MapFlags::WRITE)?;

        Ok(Self { uobj })
    }
}

impl SecCtx {
    //NOTE: maybe im misunderstanding somethign here but im assuming this
    // is how a process knows what ctx its attached to rn?
    pub fn attached_ctx() -> SecCtx {
        todo!("unsure how to get attached sec_ctx as of rn")
    }

    pub fn add_cap(&self, cap: Cap) {
        let ptr = SecCtxMap::insert(&self.uobj, cap.target, CtxMapItemType::Cap);

        let tx = self.uobj.clone().tx().unwrap();

        unsafe {
            *ptr = cap;
        }

        tx.commit().unwrap();

        debug!("Added capability at ptr: {:#?}", ptr);
    }

    pub fn id(&self) -> ObjID {
        self.uobj.id()
    }

    pub fn remove_cap(&mut self) {}
}
