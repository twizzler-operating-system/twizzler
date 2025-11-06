use clap::{Args, Parser, Subcommand};
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

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    Test,
    Create,
    Access(AccessArgs),
}

#[derive(Debug, Args)]
pub struct AccessArgs {
    #[arg(short, long)]
    obj_id: String,
    #[arg(short, long)]
    sec_ctx_id: String,
}
fn main() {
    let mut builder = default_builder();
    builder.filter_level(LevelFilter::Trace);
    builder.init();

    let args = CliArgs::parse();

    match args.command {
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
                // SecCtxFlags::empty(),
            )
            .unwrap();

            sys_sctx_attach(sec_ctx.id()).unwrap();

            // we build that object
            let target_obj = ObjectBuilder::new(spec)
                .build(DumbBase {
                    _payload: 123456789,
                })
                .unwrap();

            let target_id = target_obj.id().clone();

            let mut tx = target_obj.into_tx().expect("failed to turn into tx");

            let meta_ptr = tx.meta_mut_ptr();

            unsafe {
                (*meta_ptr).default_prot = Protections::empty();
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

            let sec_ctx_id_u128 =
                u128::from_str_radix(&args.sec_ctx_id, 16).expect("failed to parse as object id");

            let obj_id = ObjID::new(obj_id_u128);
            let sec_ctx_id = ObjID::new(sec_ctx_id_u128);

            let sec_ctx = SecCtx::try_from(sec_ctx_id).unwrap();

            sys_sctx_attach(sec_ctx.id()).unwrap();
            sys_thread_set_active_sctx_id(sec_ctx.id()).unwrap();

            let active_sec_id = sys_thread_active_sctx_id();

            println!("active sec id: {active_sec_id:#?}");

            let target = Object::<DumbBase>::map(obj_id, MapFlags::READ | MapFlags::WRITE).unwrap();

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

#[derive(Debug, Clone, Copy)]
struct DumbBase {
    _payload: u128,
}

impl BaseType for DumbBase {
    fn fingerprint() -> u64 {
        11234
    }
}
