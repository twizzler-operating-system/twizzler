#![feature(naked_functions)]

use sgtest::Foo;
use twizzler_rt_abi::error::TwzError;

#[secgate::entry(lib = "sgtest")]
pub fn foo(x: Foo) -> Result<u32, TwzError> {
    let caller = secgate::get_caller().unwrap();
    println!("==> {:?}", caller);
    return Ok(3 + x.x);
}
