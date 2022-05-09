#![feature(auto_traits)]
#![feature(specialization)]
#![feature(rustc_attrs)]
#![feature(negative_impls)]
pub mod marker;
pub mod object;

struct Foo {
    _x: *const u32,
}
impl marker::BaseType for Foo {
    fn init<T>(_t: T) -> Self {
        todo!()
    }
}
#[cfg(test)]
mod tests {
    use crate::object::Object;

    #[test]
    fn it_works() {
        let o = Object::<crate::Foo>::init_by_id();
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
