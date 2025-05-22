use alloc::collections::btree_map::BTreeMap;
use core::fmt::Display;

use base::{InsertType, SecCtxBase};
use log::debug;
use twizzler::object::{Object, ObjectBuilder, TypedObject};
use twizzler_abi::object::{ObjID, Protections};
use twizzler_rt_abi::{error::TwzError, object::MapFlags};

use crate::Cap;

pub mod base;
// pub mod map;
/// Information about protections for a given object within a context.
#[derive(Clone, Copy)]
pub struct PermsInfo {
    ctx: ObjID,
    prot: Protections,
}



pub struct SecCtx {
    uobj: Object<SecCtxBase>,
    cache: BTreeMap<ObjID, PermsInfo>,
}

impl Default for SecCtx {
    fn default() -> Self {
        let obj = ObjectBuilder::default()
            .build(SecCtxBase::default())
            .unwrap();

        Self {
            uobj: obj,
            cache: BTreeMap::new(),
        }
    }
}

impl Display for SecCtx {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let binding = self.uobj.clone();
        let base = binding.base();

        write!(f, "Sec Ctx ObjID: {} {{\n", self.uobj.id())?;
        write!(f, "base: {:?}", base)?;
        Ok(())
    }
}

impl TryFrom<ObjID> for SecCtx {
    type Error = TwzError;

    fn try_from(value: ObjID) -> Result<Self, Self::Error> {
        let uobj = Object::<SecCtxBase>::map(value, MapFlags::READ | MapFlags::WRITE)?;
    }
}

impl SecCtx {
    //NOTE: maybe im misunderstanding somethign here but im assuming this
    // is how a process knows what ctx its attached to rn?
    pub fn attached_ctx() -> SecCtx {
        todo!("unsure how to get attached sec_ctx as of rn")
    }

    pub fn add_cap(&self, cap: Cap) -> Result<(), TwzError> {
        SecCtxBase::insert(&self.uobj, cap.target, InsertType::Cap(cap))?;

        Ok(())
    }

    pub fn id(&self) -> ObjID {
        self.uobj.id()
    }

    pub fn remove_cap(&mut self) {
        todo!("implement later")
    }
}
