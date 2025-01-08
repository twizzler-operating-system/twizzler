use std::io::{Read, Write};

use embedded_io::ErrorType;
use tracing::Level;
use twizzler_abi::syscall::{
    sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags,
};

struct TwzIo;

impl ErrorType for TwzIo {
    type Error = std::io::Error;
}

impl embedded_io::Read for TwzIo {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let len = std::io::stdin().read(buf)?;

        Ok(len)
    }
}

impl embedded_io::Write for TwzIo {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        std::io::stdout().write(buf)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        std::io::stdout().flush()
    }
}

fn show(args: &[&str]) {
    if args.len() <= 1 {
        println!("usage: show <item>");
        println!("possible items: compartments, files, lethe");
        return;
    }
    match args[1] {
        "c" | "comp" | "compartments" => {
            let curr = monitor_api::CompartmentHandle::current();
            let info = curr.info();
            println!("current compartment: {:?}", info);
            println!("dependencies:");
            for comp in curr.deps() {
                let info = comp.info();
                println!(" -- {:?}", info);
            }
        }
        _ => {
            println!("unknown show item: {}", args[1]);
        }
    }
}

fn demo(_args: &[&str]) {
    tracing::info!("starting gadget file create demo");
    let file_id = sys_object_create(
        ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Persistent,
            None,
            ObjectCreateFlags::empty(),
        ),
        &[],
        &[],
    )
    .unwrap();
    tracing::debug!("created new file object {}", file_id);
    let name = file_id.raw().to_string();
    tracing::debug!("creating file with data \"test string!\"");
    let mut file = std::fs::File::create(&name).unwrap();
    file.write(b"test string!").unwrap();

    tracing::debug!("flushing file...");
    file.flush().unwrap();
    //file.sync_all().unwrap();
    drop(file);
    let mut buf = Vec::new();
    tracing::debug!("reading it back...");
    let mut file = std::fs::File::open(&name).unwrap();
    file.read_to_end(&mut buf).unwrap();
    assert_eq!(&buf, b"test string!");
    let s = String::from_utf8(buf);
    tracing::debug!("got: {:?}", s);
}

fn main() {
    println!("GADGET DEMO\n");
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG)
            .finish(),
    )
    .unwrap();
    let mut io = TwzIo;
    let mut buffer = [0; 1024];
    let mut editor = noline::builder::EditorBuilder::from_slice(&mut buffer)
        .build_sync(&mut io)
        .unwrap();
    loop {
        let line = editor.readline("gadget> ", &mut io).unwrap();
        println!("got: {}", line);
        let split = line.split_whitespace().collect::<Vec<_>>();
        if split.len() == 0 {
            continue;
        }
        match split[0] {
            "show" => {
                show(&split);
            }
            "quit" => {
                break;
            }
            "demo" => {
                demo(&split);
            }

            _ => {
                println!("unknown command {}", split[0]);
            }
        }
    }
}
