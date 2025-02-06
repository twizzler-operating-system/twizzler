use std::{fs::File, io::Read};

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
    let id: u128 = 0x1000000000000a;

    let mut f = File::create(id.to_string()).unwrap();

    let mut buf: [u8; 4096] = [0; 4096];

    // let create_spec = CreateSpec::new(LifetimeType::Persistent, BackingType::Normal);
    // let _ = TwizObj::<SecCtxMap>::create::<u8>(&create_spec, id).unwrap();
    //
    let vobj = ObjectBuilder::<SecCtxMap>::default()
        .build(SecCtxMap::new())
        .unwrap();
    let ptr = SecCtxMap::parse(vobj.id());

    SecCtxMap::insert(ptr, id.into(), CtxMapItemType::Cap, 100);

    // let obj = Object::<SecCtxMap>::map(id.into(), MapFlags::READ).unwrap();
    // let ptr = obj.base_mut_ptr::<SecCtxMap>();

    // time to see if this shit works
    // let x = f.read(&mut buf);

    println!("bytes read: {}", f.read(&mut buf).unwrap());
    println!("Status: {}", std::str::from_utf8(&buf).unwrap());
}
