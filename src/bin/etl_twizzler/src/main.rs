#[cfg(target_os = "twizzler")]
extern crate twizzler_abi;
#[cfg(target_os = "twizzler")]
use std::sync::atomic::{AtomicU64, Ordering};

use clap::{Parser, Subcommand};
use etl_twizzler::etl::{Pack, PackType, Unpack};
#[cfg(target_os = "twizzler")]
use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_thread_sync, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags, ThreadSync,
        ThreadSyncFlags, ThreadSyncReference, ThreadSyncWake,
    },
};
#[cfg(target_os = "twizzler")]
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};

// To signal main that the program is done running.
// Bug: If the program crashes, it hangs forever
// Isn't there a process table somewhere for main to look at it to cap this thing?
#[cfg(target_os = "twizzler")]
#[allow(non_snake_case)]
fn SIGNAL_INIT() -> Option<()> {
    let id = std::env::var("booger").ok()?;
    let id = id
        .parse::<u128>()
        .unwrap_or_else(|_| panic!("failed to parse object ID string {}", id));
    let id = ObjID::new(id);
    let obj = Object::<AtomicU64>::init_id(
        id,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    )
    .unwrap();
    let pt = unsafe { obj.base_mut_unchecked() };

    pt.store(1, Ordering::SeqCst);
    let op = ThreadSync::new_wake(ThreadSyncWake::new(
        ThreadSyncReference::Virtual(pt as *const AtomicU64),
        usize::MAX,
    ));
    let _ = twizzler_abi::syscall::sys_thread_sync(&mut [op], None);
    Some(())
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Pack {
        #[arg(long)]
        make_file: bool,

        #[arg(long)]
        make_obj: bool,

        #[arg(long)]
        make_vector: bool,

        #[arg(long)]
        offset: Option<u64>,

        #[arg(long)]
        archive_name: Option<String>,

        file_list: Vec<String>,
    },
    Unpack {
        archive_path: String,
    },
    Inspect {
        archive_path: String,
    },
    Read {
        archive_path: String,

        query: String,
    },
}

#[cfg(target_os = "twizzler")]
fn create_twizzler_object() -> twizzler_object::ObjID {
    let create = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Persistent,
        None,
        ObjectCreateFlags::empty(),
    );
    let twzid = twizzler_abi::syscall::sys_object_create(create, &[], &[]).unwrap();

    twzid
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Pack {
            make_file,
            make_obj,
            make_vector,
            archive_name,
            file_list,
            offset,
        } => {
            let archive_stream = if let Some(archive_name) = archive_name {
                #[cfg(target_os = "twizzler")]
                let archive_name = {
                    let twzid = create_twizzler_object();
                    println!("twzid created: {}", twzid);
                    twzid.as_u128().to_string()
                };

                let archive = std::fs::File::create(archive_name).unwrap();
                Box::new(archive) as Box<dyn std::io::Write>
            } else {
                let stdout = std::io::stdout().lock();
                Box::new(stdout) as Box<dyn std::io::Write>
            };

            let mut pack = Pack::new(archive_stream);

            let pack_type = if make_file {
                PackType::StdFile
            } else {
                match (make_obj, make_vector) {
                    (true, true) => PackType::StdFile,
                    (true, false) => PackType::TwzObj,
                    (false, true) => PackType::PVec,
                    (false, false) => PackType::StdFile,
                }
            };

            let offset = offset.unwrap_or(0);
            if file_list.len() == 0 {
                pack.stream_add(
                    std::io::stdin().lock(),
                    "stdin".to_owned(),
                    pack_type,
                    offset,
                )
                .unwrap();
            }
            for file in file_list {
                pack.file_add(file.into(), pack_type, offset).unwrap();
            }
        }
        Commands::Unpack { archive_path } => {
            let archive = std::fs::File::open(archive_path).unwrap();
            let unpack = Unpack::new(archive).unwrap();
            unpack.unpack().unwrap();
        }
        Commands::Inspect { archive_path } => {
            let archive = std::fs::File::open(archive_path).unwrap();
            let unpack = Unpack::new(archive).unwrap();
            let mut stdout = std::io::stdout().lock();
            unpack.inspect(&mut stdout).unwrap()
        }
        Commands::Read {
            archive_path,
            query,
        } => {
            let archive = std::fs::File::open(archive_path).unwrap();
            let unpack = Unpack::new(archive).unwrap();
            let mut stdout = std::io::stdout().lock();
            unpack.read(&mut stdout, query).unwrap()
        }
    }

    #[cfg(target_os = "twizzler")]
    SIGNAL_INIT();
}
