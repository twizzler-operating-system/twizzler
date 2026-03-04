use secgate::TwzError;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Foo {
    pub x: u32,
}

#[secgate::gatecall]
fn foo(f: Foo) -> Result<u32, TwzError> {}

pub fn bar(f: Foo) -> Foo {
    let y = foo(f).unwrap();
    Foo { x: y }
}
