use clap::{Args, Parser, Subcommand};
use colog::default_builder;
use log::{LevelFilter, info};
use twizzler::{
    marker::BaseType,
    object::{ObjID, Object, ObjectBuilder, RawObject, TypedObject},
};
use twizzler_abi::{
    object::Protections,
    syscall::{ObjectCreate, ObjectCreateFlags, sys_sctx_attach, sys_thread_set_active_sctx_id},
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

            // we build that object
            let target_obj = ObjectBuilder::new(spec)
                .build(DumbBase {
                    _payload: 123456789,
                })
                .unwrap();

            // get that target id and chill
            let target_id = target_obj.id().clone();

            info!("object id of created object: {target_id:#?}");
        }

        Commands::Access(args) => {
            let obj_id_u128 =
                u128::from_str_radix(&args.obj_id, 16).expect("failed to parse as object id");

            let obj_id = ObjID::new(obj_id_u128);

            let target = Object::<DumbBase>::map(obj_id, MapFlags::READ | MapFlags::WRITE).unwrap();

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
