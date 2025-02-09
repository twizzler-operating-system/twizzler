use std::fs::File;

use clap::{Parser, Subcommand, ValueEnum};
use twizzler::{
    marker::{BaseType, StoreCopy},
    object::{Object, ObjectBuilder, RawObject, TypedObject},
    tx::TxObject,
};
use twizzler_abi::{
    object::ObjID,
    syscall::{BackingType, LifetimeType, ObjectCreate},
};
use twizzler_object::{CreateSpec, Object as TwizObj};
use twizzler_rt_abi::object::MapFlags;
use twizzler_security::sec_ctx::map::{CtxMapItemType, SecCtxMap};

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

// sec-test read --id 0xd4b0930f0bbc0bb745af3a196e4014ed
//
fn main() {
    let args = Args::parse();

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
                todo!()
            }
        },

        None => {
            // some fantasy object we want to create a cap for
            let id: u128 = 0x1000000000000a;

            // how to build a persistent object
            let vobj = ObjectBuilder::<SecCtxMap>::default()
                // .persist()
                .build(SecCtxMap::new())
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

            let map = Object::<SecCtxMap>::map(vobj_id, MapFlags::READ | MapFlags::WRITE).unwrap();

            println!("Object Id: {:#?}", map.id());

            let res = SecCtxMap::lookup(&map, id.into());
            println!("lookup results {:#?}", res);
        }
    }
}
