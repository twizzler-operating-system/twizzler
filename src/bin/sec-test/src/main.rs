use clap::{Parser, Subcommand};
use colog::default_builder;
use log::LevelFilter;
use twizzler::object::{Object, ObjectBuilder, TypedObject};
use twizzler_abi::object::Protections;
use twizzler_rt_abi::object::MapFlags;
use twizzler_security::{sec_ctx::SecCtx, Cap, SigningKey, SigningScheme};

fn main() {
    let mut builder = default_builder();
    builder.filter_level(LevelFilter::Trace);
    builder.init();
    let sec_ctx = SecCtx::default();

    let target = 0x123.into();
    let accessor = 0x321.into();
    let prots = Protections::all();

    let (s_key, v_key) = SigningKey::new_keypair(&SigningScheme::Ecdsa, Default::default())
        .expect("should have worked");

    let cap = Cap::new(
        target,
        accessor,
        prots,
        s_key.base(),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
    )
    .unwrap();

    sec_ctx.add_cap(cap);

    println!("{}", sec_ctx);

    let id = sec_ctx.id();
    drop(sec_ctx);

    let sec_ctx = SecCtx::try_from(id).expect("should be found");

    println!("just read: {}", sec_ctx)
}
