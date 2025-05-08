use clap::{Parser, Subcommand};
use colog::default_builder;
use log::LevelFilter;
use twizzler::object::{Object, ObjectBuilder};
use twizzler_abi::object::Protections;
use twizzler_rt_abi::object::MapFlags;
use twizzler_security::{
    sec_ctx::{
        map::{CtxMapItemType, SecCtxMap},
        SecCtx,
    },
    Cap, SigningKey,
};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub enum Commands {
    Read {
        #[arg(short, long, value_parser)]
        id: String,
    },
    /// Search various aspects within the service.
    Write {
        #[arg(short, long, value_parser)]
        id: String,
    },
}

// sec-test read --id 0x274b9675a25837a7446a419c68df8fc7
//
fn main() {
    let args = Args::parse();
    let mut builder = default_builder();
    builder.filter_level(LevelFilter::Trace);
    builder.init();

    match args.command {
        Some(command) => match command {
            Commands::Read { id } => {
                let id = id.trim_start_matches("0x");

                let sec_ctx_id = u128::from_str_radix(id, 16).unwrap().into();

                let id: u128 = 0x1000000000000a;

                let map =
                    Object::<SecCtxMap>::map(sec_ctx_id, MapFlags::READ | MapFlags::WRITE).unwrap();

                println!("Object Id: {:#?}", map.id());

                let res = SecCtxMap::lookup(&map, id.into());
                println!("lookup results {:#?}", res);
            }

            Commands::Write { id } => {
                // how to build a persistent object
                let id: u128 = id.parse().expect("id should be valid u128");
                let vobj = ObjectBuilder::<SecCtxMap>::default()
                    // .persist()
                    .build(SecCtxMap::default())
                    .unwrap();

                println!("SecCtxObjId: {}", vobj.id());

                let vobj_id = vobj.id();

                let cap_ptr = SecCtxMap::insert(&vobj, id.into(), CtxMapItemType::Cap);

                println!("Ptr: {:#?}", cap_ptr);
                println!("SecCtxObjId: {}", vobj_id);

                let res = SecCtxMap::lookup(&vobj, id.into());
                println!("lookup results {:#?}", res);

                println!("\n\n\n============================\n\n\n");

                let map =
                    Object::<SecCtxMap>::map(vobj_id, MapFlags::READ | MapFlags::WRITE).unwrap();

                println!("Object Id: {:#?}", map.id());

                let res = SecCtxMap::lookup(&map, id.into());
                println!("lookup results {:#?}", res);
            }
        },

        None => {
            let sec_ctx = SecCtx::default();

            let target = 0x123.into();
            let accessor = 0x321.into();
            let prots = Protections::all();
            let target_priv_key =
                SigningKey::from_slice(&rand_32(), Default::default()).expect("should work");

            let cap = Cap::new(
                target,
                accessor,
                prots,
                target_priv_key,
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

            let sec_ctx: SecCtx = id.try_into().unwrap();

            println!("just read: {}", sec_ctx)
        }
    }
}

pub fn rand_32() -> [u8; 32] {
    let mut dest = [0 as u8; 32];
    getrandom::getrandom(&mut dest).unwrap();
    dest
}
