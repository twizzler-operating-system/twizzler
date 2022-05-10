#![feature(auto_traits)]
//#![feature(specialization)]
#![feature(rustc_attrs)]
#![feature(negative_impls)]

use twizzler_abi::marker;

pub use twizzler_abi::object::ObjID;
pub use twizzler_abi::object::Protections;

mod base;
pub mod cell;
mod create;
mod init;
mod object;
mod ptr;
mod tx;

pub use create::*;
pub use init::*;
pub use object::*;
struct Foo {
    _x: *const u32,
}
impl marker::BaseType for Foo {
    fn init<T>(_t: T) -> Self {
        todo!()
    }

    fn tags() -> &'static [(marker::BaseVersion, marker::BaseTag)] {
        todo!()
    }
}
#[cfg(test)]
mod tests {
    use crate::Object;

    #[test]
    fn it_works() {
        let o = Object::<crate::Foo>::init_by_id();
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
