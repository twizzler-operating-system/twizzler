use std::{
    fs::File,
    io::{BufRead, Read},
};

use twizzler::object::{Object, ObjectBuilder, RawObject};
use twizzler_abi::{
    marker::BaseType,
    object::ObjID,
    syscall::{BackingType, LifetimeType},
};
use twizzler_object::{CreateSpec, Object as TwizObj};
use twizzler_rt_abi::object::MapFlags;
use twizzler_security::sec_ctx::map::{CtxMapItemType, SecCtxMap};

fn main() {
    // some fantasy object we want to create a cap for
    let id: u128 = 0x1000000000000a;

    let vobj = ObjectBuilder::<SecCtxMap>::default()
        .build(SecCtxMap::new())
        .unwrap();
    let ptr = SecCtxMap::parse(vobj.id());
    println!("ptr: {:#?}", ptr);

    let map = SecCtxMap::insert(ptr, id.into(), CtxMapItemType::Cap, 100);
    println!("map we just modified: {:#?}", map);

    let (len, buf) = SecCtxMap::lookup(ptr, id.into());
    println!("lookup results {:#?}", buf);
}
