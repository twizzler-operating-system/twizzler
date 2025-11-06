use colog::default_builder;
use log::{info, LevelFilter};
use twizzler::{
    marker::BaseType,
    object::{Object, ObjectBuilder, RawObject, TypedObject},
};
use twizzler_abi::{
    object::Protections,
    syscall::{sys_sctx_attach, sys_thread_set_active_sctx_id, ObjectCreate},
};
use twizzler_rt_abi::object::MapFlags;
use twizzler_security::{Cap, SecCtx, SecCtxFlags, SigningKey, SigningScheme};

#[derive(Debug, Clone, Copy)]
struct DumbBase {
    _payload: u128,
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

    let (s_key, v_key) = SigningKey::new_keypair(&SigningScheme::Ecdsa, Default::default())
        .expect("should have worked");

    // // create some security context
    let sec_ctx = SecCtx::new(
        ObjectCreate::new(
            Default::default(),
            Default::default(),
            None,
            Default::default(),
            Protections::all(),
        ),
        Protections::all(),
        SecCtxFlags::empty(),
    )
    .unwrap();

    sys_sctx_attach(sec_ctx.id()).unwrap();
    sys_thread_set_active_sctx_id(sec_ctx.id()).unwrap();

    // by default an object has empty permissions
    let spec = ObjectCreate::new(
        Default::default(),
        Default::default(),
        Some(v_key.id()),
        Default::default(),
        // Protections::all(),
        // Protections::READ | Protections::WRITE,
        // Protections::READ,
        Protections::empty(),
    );
    info!("creating target object with spec: {:?}", spec);

    // we build that object
    let target_obj = ObjectBuilder::new(spec)
        .build(DumbBase {
            _payload: 123456789,
        })
        .unwrap();

    // get that target id and chill
    let target_id = target_obj.id().clone();
    drop(target_obj);

    // print some stuff
    info!("target_id :{:?}", target_id);
    info!("sec_ctx id:{:?}", sec_ctx.id());

    // prots??
    let prots = Protections::empty();

    // create a new capability
    let cap = Cap::new(
        target_id,
        sec_ctx.id(),
        prots,
        s_key.base(),
        Default::default(),
        Default::default(),
        Default::default(),
    )
    .unwrap();

    sec_ctx.insert_cap(cap).unwrap();
    println!("Inserted Capability!");
    // attach to this sec_ctx

    // time to try accessing this object
    let target = Object::<DumbBase>::map(target_id, MapFlags::READ | MapFlags::WRITE).unwrap();
    let base = target.base();

    let base_mut: *mut DumbBase = target.base_mut_ptr();

    println!("base: {:?}", base);

    unsafe {
        let mut x = *base_mut;
        x._payload = 5;
        println!("{x:#?}");
    }
}
