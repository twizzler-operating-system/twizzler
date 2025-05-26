use clap::{Parser, Subcommand};
use colog::default_builder;
use log::LevelFilter;
use twizzler::{
    marker::BaseType,
    object::{Object, ObjectBuilder, RawObject, TypedObject},
};
use twizzler_abi::{
    object::Protections,
    syscall::{sys_sctx_attach, ObjectCreate, ObjectCreateFlags},
};
use twizzler_rt_abi::object::MapFlags;
use twizzler_security::{Cap, SecCtx, SecCtxBase, SecCtxFlags, SigningKey, SigningScheme};

#[derive(debug)]
struct DumbBase {
    payload: u128,
}

impl BaseType for DumbBase {
    fn fingerprint() -> u64 {
        11234
    }
}

fn main() {
    let mut builder = default_builder();
    builder.filter_level(LevelFilter::Trace);
    builder.init();
    let sec_ctx = SecCtx::default();

    let (s_key, v_key) = SigningKey::new_keypair(&SigningScheme::Ecdsa, Default::default())
        .expect("should have worked");

    // lets create an object and try to access it
    let target_obj = ObjectBuilder::new(ObjectCreate::new(
        Default::default(),
        Default::default(),
        Some(v_key.id()),
        Default::default(),
        Protections::empty(),
    ))
    .build(DumbBase { payload: 123456789 })
    .unwrap();

    drop(target_obj);

    let uobj = ObjectBuilder::new(ObjectCreate::new(
        Default::default(),
        Default::default(),
        Some(v_key.id()),
        Default::default(),
        Protections::all(),
    ))
    .build(SecCtxBase::new(Protections::all(), SecCtxFlags::empty()))
    .unwrap();

    let sec_ctx = SecCtx::new(
        ObjectCreate::new(
            Default::default(),
            Default::default(),
            Some(v_key.id()),
            Default::default(),
            Protections::all(),
        ),
        Protections::all(),
        SecCtxFlags::empty(),
    )
    .unwrap();

    let prots = Protections::READ;

    let cap = Cap::new(
        target_obj.id(),
        sec_ctx.id(),
        prots,
        s_key.base(),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
    )
    .unwrap();

    let target_id = cap.target.clone();

    sec_ctx.insert_cap(cap);
    // attach to this sec_ctx
    sys_sctx_attach(sec_ctx.id());

    // time to try accessing this object

    let target = Object::<DumbBase>::map(target_id, MapFlags::READ | MapFlags::WRITE).unwrap();
    let base = target.base();
    println!("base: {:?}", base)
}
