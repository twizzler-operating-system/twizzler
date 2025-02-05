use map::SecCtxMap;
use twizzler::object::Object;

pub mod map;

// ok we have access to an object, now what the fuck do we do.
//

// im assuming that there is some way for a process to
// get the object id for the security context its attached to?

pub struct SecCtx {
    uobj: Object<SecCtxMap>,
}

impl SecCtx {
    //NOTE: maybe im misunderstanding somethign here but im assuming this
    // is how a process knows what ctx its attached to rn?
    pub fn attached_ctx() -> SecCtx {
        todo!("unsure how to get attached sec_ctx as of rn")
    }

    pub fn add_cap(&mut self) {
        todo!()
        // let x = self.uobj.base();
    }

    pub fn remove_cap(&mut self) {}
}
