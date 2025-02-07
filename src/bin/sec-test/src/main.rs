use std::{
    fs::File,
    io::{BufRead, Read},
};

use clap::{Parser, Subcommand, ValueEnum};
use twizzler::object::{Object, ObjectBuilder, RawObject};
use twizzler_abi::{
    marker::BaseType,
    object::ObjID,
    syscall::{BackingType, LifetimeType},
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
        obj_id: String,
    },
    /// Search various aspects within the service.
    Write {
        #[arg(short, long, value_parser)]
        obj_id: String,
    },
}

fn main() {
    let args = Args::parse();

    match args.command {
        Some(command) => match command {
            Read(obj_id) => {}
            Write(obj_id) => {}
        },
        None => {
            // some fantasy object we want to create a cap for
            let id: u128 = 0x1000000000000a;

            let vobj = ObjectBuilder::<SecCtxMap>::default()
                .build(SecCtxMap::new())
                .unwrap();

            let ptr = SecCtxMap::parse(vobj.id());
            println!("ptr: {:#?}", ptr);
            let ptr = SecCtxMap::parse(vobj.id());
            println!("again ptr: {:#?}", ptr);

            let writeable_offset = SecCtxMap::insert(ptr, id.into(), CtxMapItemType::Cap, 100);

            unsafe {
                println!("map: {:#?}", (*ptr));
            }

            let (len, buf) = SecCtxMap::lookup(ptr, id.into());
            println!("lookup results {:#?}", buf);
        }
    }
}
