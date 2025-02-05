use map::SecCtxMap;
use twizzler::object::Object;

use crate::Cap;

pub mod map;

// ok we have access to an object, now what the fuck do we do.
//

// im assuming that there is some way for a process to
// get the object id for the security context its attached to?
pub struct SecCtx {
    uobj: Object<SecCtxMap>,
    map: *mut SecCtxMap,
}

impl SecCtx {
    //NOTE: maybe im misunderstanding somethign here but im assuming this
    // is how a process knows what ctx its attached to rn?
    pub fn attached_ctx() -> SecCtx {
        todo!("unsure how to get attached sec_ctx as of rn")
    }

    pub fn add_cap(&mut self, cap: Cap) {
        let _write_at_offset = SecCtxMap::insert(
            self.map,
            cap.target,
            map::CtxMapItemType::Cap,
            size_of::<Cap>() as u32,
        );
        //TODO: how do i write into an object
    }

    pub fn remove_cap(&mut self) {}
}
