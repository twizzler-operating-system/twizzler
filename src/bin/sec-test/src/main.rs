use std::fs::File;

use clap::{Parser, Subcommand, ValueEnum};
use twizzler::object::{Object, ObjectBuilder, RawObject};
use twizzler_abi::{
    marker::BaseType,
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

// sec-test read --id 0xf5befa7a3560c49face6a8f63d97d487
fn main() {
    let args = Args::parse();

    match args.command {
        Some(command) => match command {
            Commands::Read { id } => {
                let id = id.trim_start_matches("0x");

                let sec_ctx_id = u128::from_str_radix(id, 16).unwrap().into();
                // let sec_ctx_id = id.parse::<u128>().unwrap().into();

                let ptr = SecCtxMap::parse(sec_ctx_id);
                println!("ptr: {:#?}", ptr);

                let (len, buf) = SecCtxMap::lookup(ptr, sec_ctx_id);
                println!("lookup results {:#?}", buf);
            }

            Commands::Write { id } => {
                todo!()
            }
        },

        None => {
            // some fantasy object we want to create a cap for
            let id: u128 = 0x1000000000000a;

            // how to build a persistent object
            let vobj = ObjectBuilder::<SecCtxMap>::new(ObjectCreate::new(
                BackingType::Normal,
                LifetimeType::Persistent,
                Default::default(),
                Default::default(),
            ))
            .build(SecCtxMap::new())
            .unwrap();

            println!("SecCtxObjId: {}", vobj.id());

            let ptr = SecCtxMap::parse(vobj.id());
            println!("ptr: {:#?}", ptr);

            let writeable_offset = SecCtxMap::insert(ptr, id.into(), CtxMapItemType::Cap, 100);

            unsafe {
                println!("map: {:#?}", (*ptr));
            }

            let (len, buf) = SecCtxMap::lookup(ptr, id.into());
            println!("lookup results {:#?}", buf);
        }
    }
}
