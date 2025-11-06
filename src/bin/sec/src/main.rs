use clap::Parser;
use colog::default_builder;
use log::{LevelFilter, info};
use twizzler::{
    marker::BaseType,
    object::{ObjID, Object, ObjectBuilder, RawObject, TypedObject},
};
use twizzler_abi::{
    object::Protections,
    syscall::{
        ObjectCreate, ObjectCreateFlags, sys_sctx_attach, sys_thread_active_sctx_id,
        sys_thread_set_active_sctx_id,
    },
};
use twizzler_rt_abi::object::MapFlags;
use twizzler_security::{Cap, SecCtx, SecCtxFlags, SigningKey, SigningScheme};

mod args;
use args::*;

fn main() {
    let mut builder = default_builder();
    builder.filter_level(LevelFilter::Trace);
    builder.init();

    let args = CliArgs::parse();

    match args.command {
        Commands::Obj(commands) => match commands {
            ObjCommands::Inspect(args) => {
                if let Some(sec_ctx_id) = args.sec_ctx_id {
                    let sec_ctx = SecCtx::try_from(sec_ctx_id).unwrap();
                    sys_sctx_attach(sec_ctx.id()).unwrap();
                    sys_thread_set_active_sctx_id(sec_ctx.id()).unwrap();
                    println!("attached to SecCtx: {sec_ctx_id:#?}");
                }

                let target =
                    Object::<MessageStoreObj>::map(args.obj_id, MapFlags::READ | MapFlags::WRITE)
                        .unwrap();

                let obj = target.base();

                println!("{target:#?}\n{obj:#?}");
            }
            ObjCommands::New(args) => {
                // by default an object has empty permissions
                let spec = ObjectCreate::new(
                    Default::default(),
                    Default::default(),
                    Some(args.verifying_key_id),
                    Default::default(),
                    Protections::READ | Protections::WRITE,
                );

                // println!("creating target object with spec: {:?}", spec);

                let target_obj = ObjectBuilder::new(spec)
                    .build(MessageStoreObj {
                        // message: args.message,
                        message: heapless::String::<256>::try_from(args.message.as_str())
                            .expect("message was longer than 256 characters!!"),
                    })
                    .unwrap();

                // seal the object
                let obj = if args.seal {
                    let mut tx = target_obj.into_tx().expect("failed to turn into tx");
                    // NOTE: you shouldnt have to do all this to change the default
                    // protections...,     I honestly think it should be a part of
                    // the object     creation spec?
                    //
                    // i.e when the object is created, its always created with READ | WRITE, and
                    // then after the base is written the default prots get
                    // changed to what the user specified
                    let meta_ptr = tx.meta_mut_ptr();

                    unsafe {
                        (*meta_ptr).default_prot = Protections::empty();
                    }

                    tx.into_object().expect("failed to save ")
                } else {
                    target_obj
                };

                // let mut tx = target_obj.into_tx().expect("failed to turn into tx");

                //NOTE: you shouldnt have to do all this to change the default protections...,
                // I honestly think it should be a part of the object
                // creation spec?
                //
                // i.e when the object is created, its always created with READ | WRITE, and
                // then after the base is written the default prots get
                // changed to what the user specified
                // let meta_ptr = tx.meta_mut_ptr();

                // let prots = if args.seal {
                //     Protections::empty()
                // } else {
                //     Protections::READ | Protections::WRITE
                // };

                // unsafe {
                //     (*meta_ptr).default_prot = prots;
                // }

                // let obj = tx.into_object().expect("failed to save ");

                unsafe {
                    println!(
                        "created Object with id: {:#?}\n{:#?}",
                        obj.id(),
                        *obj.meta_ptr()
                    );
                }
            }
        },
        Commands::Key(KeyCommands::NewPair) => {
            let (s_key, v_key) = SigningKey::new_keypair(&SigningScheme::Ecdsa, Default::default())
                .expect("should have worked");

            println!(
                "Keypair created!\nSigning Key: {:#?}\nVerifying Key: {:#?}",
                s_key.id(),
                v_key.id()
            );
        }
        Commands::Ctx(ctxcommands) => match ctxcommands {
            CtxCommands::New(args) => {
                let flags = if args.undetachable {
                    SecCtxFlags::UNDETACHABLE
                } else {
                    SecCtxFlags::empty()
                };

                let sec_ctx = SecCtx::new(
                    ObjectCreate::new(
                        Default::default(),
                        Default::default(),
                        None,
                        Default::default(),
                        Protections::all(),
                    ),
                    Protections::all(),
                    flags,
                )
                .unwrap();

                let id = sec_ctx.id();

                let base = sec_ctx.base();

                println!("Created SecCtx: {id:#?}\n{base:#?}");
            }

            CtxCommands::Inspect(args) => {}
        },

        Commands::Create => {
            let (s_key, v_key) = SigningKey::new_keypair(&SigningScheme::Ecdsa, Default::default())
                .expect("should have worked");

            // by default an object has empty permissions
            let spec = ObjectCreate::new(
                Default::default(),
                Default::default(),
                Some(v_key.id()),
                Default::default(),
                Protections::READ | Protections::WRITE,
            );

            info!("creating target object with spec: {:?}", spec);

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
                SecCtxFlags::UNDETACHABLE,
            )
            .unwrap();

            sys_sctx_attach(sec_ctx.id()).unwrap();

            // we build that object
            let target_obj = ObjectBuilder::new(spec)
                .build(MessageStoreObj {
                    message: heapless::String::<256>::try_from("lol").expect("should have worked"),
                })
                .unwrap();

            let target_id = target_obj.id().clone();

            let mut tx = target_obj.into_tx().expect("failed to turn into tx");

            //NOTE: you shouldnt have to do all this to change the default protections..., I
            // honestly think it should be a part of the object creation spec?
            //
            // i.e when the object is created, its always created with READ | WRITE, and then
            // after the base is written the default prots get changed to what the
            // user specified
            let meta_ptr = tx.meta_mut_ptr();

            unsafe {
                // (*meta_ptr).default_prot = Protections::empty();
                (*meta_ptr).default_prot = Protections::all();
            }

            let updated_obj = tx.into_object().expect("failed to save ");

            let meta = updated_obj.meta_ptr();

            unsafe {
                let meta = meta.read();
                println!("metadata: {meta:#?}");
            }

            // get that target id and chill
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

            info!("object id of created object: {target_id:#?}");
            info!("object id of sec ctx: {:#?}", sec_ctx.id());
        }

        Commands::Access(args) => {
            let obj_id_u128 =
                u128::from_str_radix(&args.obj_id, 16).expect("failed to parse as object id");

            let sec_ctx_id_u128 = u128::from_str_radix(&args.sec_ctx_id, 16).expect(
                "failed to parse as object
            id",
            );

            let obj_id = ObjID::new(obj_id_u128);
            let sec_ctx_id = ObjID::new(sec_ctx_id_u128);

            let sec_ctx = SecCtx::try_from(sec_ctx_id).unwrap();

            sys_sctx_attach(sec_ctx.id()).unwrap();
            sys_thread_set_active_sctx_id(sec_ctx.id()).unwrap();

            let active_sec_id = sys_thread_active_sctx_id();

            println!("active sec id: {active_sec_id:#?}");

            let target =
                Object::<MessageStoreObj>::map(obj_id, MapFlags::READ | MapFlags::WRITE).unwrap();

            let meta = target.meta_ptr();

            unsafe {
                let meta = *meta;

                println!("metadata: {meta:#?}");
            }

            let obj = target.base();

            println!("obj: {obj:#?}");
        }

        Commands::Test => {}
    }

    // println!("args:{args:?}");
    // println!("Hello, world!");
}

#[derive(Debug, Clone)]
struct MessageStoreObj {
    message: heapless::String<256>,
}

impl BaseType for MessageStoreObj {
    fn fingerprint() -> u64 {
        11234
    }
}
