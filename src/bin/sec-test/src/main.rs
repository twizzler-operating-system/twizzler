use std::fs::File;

use clap::{Parser, Subcommand, ValueEnum};
use colog::{basic_builder, default_builder};
use log::LevelFilter;
use twizzler::{
    marker::{BaseType, StoreCopy},
    object::{Object, ObjectBuilder, RawObject, TypedObject},
    tx::TxObject,
};
use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::{BackingType, LifetimeType, ObjectCreate},
};
use twizzler_rt_abi::object::MapFlags;
use twizzler_security::{
    sec_ctx::{
        map::{CtxMapItemType, SecCtxMap},
        SecCtx,
    },
    Cap, SigningKey, SigningScheme,
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
                // some fantasy object we want to create a cap for
                let id: u128 = 0x1000000000000a;

                // how to build a persistent object
                let vobj = ObjectBuilder::<SecCtxMap>::default()
                    // .persist()
                    .build(SecCtxMap::default())
                    .unwrap();

                println!("SecCtxObjId: {}", vobj.id());

                // let ptr = SecCtxMap::parse(vobj.id());
                // println!("ptr: {:#?}", ptr);
                //
                let vobj_id = vobj.id();

                let writeable_offset = SecCtxMap::insert(&vobj, id.into(), CtxMapItemType::Cap);

                println!("SecCtxObjId: {}", vobj_id);

                // unsafe {
                //     println!("map: {:#?}", *vobj.base_ptr::<SecCtxMap>());
                // }

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
