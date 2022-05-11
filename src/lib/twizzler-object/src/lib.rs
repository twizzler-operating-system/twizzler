#![feature(auto_traits)]
//#![feature(specialization)]
#![feature(rustc_attrs)]
#![feature(negative_impls)]

use refs::InvRef;
use twizzler_abi::marker;

pub use twizzler_abi::object::ObjID;
pub use twizzler_abi::object::Protections;

mod base;
pub mod cell;
mod create;
mod init;
mod meta;
mod object;
mod ptr;
mod refs;
mod tx;

pub use create::*;
pub use init::*;
pub use object::*;
struct Foo<'a> {
    x: InvRef<'a, Foo<'a>>,
}
impl<'a> marker::BaseType for Foo<'a> {
    fn init<T>(_t: T) -> Self {
        todo!()
    }

    fn tags() -> &'static [(marker::BaseVersion, marker::BaseTag)] {
        todo!()
    }
}
#[cfg(test)]
mod tests {
    use twizzler_abi::object::Protections;

    use crate::{Object, ObjectInitFlags};

    #[test]
    fn it_works() {
        let o =
            Object::<crate::Foo>::init_id(0.into(), Protections::READ, ObjectInitFlags::empty())
                .unwrap();

        let base = o.base_raw().unwrap();
        let p = base.x.lea();

        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
