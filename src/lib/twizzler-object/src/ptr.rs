use crate::Object;

impl<T> Object<T> {
    pub fn raw_lea<P>(&self, off: usize) -> *const P {
        let (start, _) = twizzler_abi::slot::to_vaddr_range(self.slot);
        unsafe { ((start + off) as *const P).as_ref().unwrap() }
    }

    pub fn raw_lea_mut<P>(&self, off: usize) -> *mut P {
        let (start, _) = twizzler_abi::slot::to_vaddr_range(self.slot);
        unsafe { ((start + off) as *mut P).as_mut().unwrap() }
    }

}

