#![feature(auto_traits)]
//#![feature(specialization)]
#![feature(rustc_attrs)]
#![feature(negative_impls)]
#![feature(option_result_unwrap_unchecked)]

pub use twizzler_abi::object::ObjID;

mod base;
pub mod cell;
mod create;
mod init;
pub mod marker;
mod meta;
mod object;
pub mod ptr;
pub mod slot;
pub mod tx;

pub use create::*;
pub use init::*;
pub use object::*;

#[cfg(test)]
mod tests {
    use twizzler_abi::object::Protections;

    use crate::{Object, ObjectInitFlags};
    struct Foo {
        x: u32,
    }
    impl marker::BaseType for Foo {
        fn init<T>(_t: T) -> Self {
            todo!()
        }

        fn tags() -> &'static [(marker::BaseVersion, marker::BaseTag)] {
            todo!()
        }
    }
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
