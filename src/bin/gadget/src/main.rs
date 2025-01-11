use std::io::{Read, Write};

use embedded_io::ErrorType;
use logboi::LogHandle;
use naming::NamingHandle;
use tracing::Level;
use twizzler_abi::{
    object::ObjID,
    syscall::{sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
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

fn lethe_cmd(args: &[&str], namer: &mut NamingHandle) {
    if args.len() <= 1 {
        println!("usage: lethe <cmd>");
        println!("possible cmds: adv");
        return;
    }
    match args[1] {
        "a" | "adv" => {
            tracing::warn!("unimplemented: lethe adv (advance epoch)");
        }
        _ => {
            println!("unknown lethe cmd: {}", args[1]);
        }
    }
}

fn show(args: &[&str], namer: &mut NamingHandle) {
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
        "f" | "fi" | "files" => {
            let names = namer.enumerate_names();
            for name in names {
                println!("{:<20} :: {}", name.0, name.1);
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

    tracing::info!("deleting file...");
    std::fs::remove_file(&name).unwrap();
}

fn read_file(args: &[&str], namer: &mut NamingHandle) {
    if args.len() < 2 {
        println!("usage: read <filename>");
    }
    let filename = args[1];
    let Some(id) = namer.get(filename) else {
        tracing::warn!("name {} not found", filename);
        return;
    };

    let idname = id.to_string();
    let mut file = std::fs::File::open(&idname).unwrap();
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).unwrap();
    let s = String::from_utf8(buf);
    if let Ok(s) = s {
        println!("{}", s);
    } else {
        tracing::warn!("UTF-8 error when reading {}", filename);
    }
}

fn write_file(args: &[&str], namer: &mut NamingHandle) {
    if args.len() < 2 {
        println!("usage: write <filename>");
    }
    let filename = args[1];
    let Some(id) = namer.get(filename) else {
        tracing::warn!("name {} not found", filename);
        return;
    };

    let data = format!("hello gadget from file {}", filename);
    let idname = id.to_string();
    let mut file = std::fs::File::open(&idname).unwrap();
    tracing::warn!("for now, we just write test data: `{}'", data);
    file.write(data.as_bytes()).unwrap();

    tracing::info!("calling sync!");
    file.sync_all().unwrap();
}

fn new_file(args: &[&str], namer: &mut NamingHandle) {
    if args.len() < 2 {
        println!("usage: new <filename>");
    }
    let filename = args[1];
    if namer.get(filename).is_some() {
        tracing::warn!("name {} already exists", filename);
        return;
    };
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
    namer.put(filename, file_id.raw());
}

fn del_file(args: &[&str], namer: &mut NamingHandle) {
    if args.len() < 2 {
        println!("usage: write <filename>");
    }
    let filename = args[1];
    let Some(id) = namer.get(filename) else {
        tracing::warn!("name {} not found", filename);
        return;
    };
    tracing::info!("deleting file...");
    let idname = id.to_string();
    std::fs::remove_file(&idname).unwrap();
    tracing::info!("removing name...");
    namer.remove(filename);
}

fn main() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG)
            .finish(),
    )
    .unwrap();
    let mut namer = NamingHandle::new().unwrap();
    let mut logger = LogHandle::new().unwrap();
    logger.log(b"Hello Logger!\n");
    tracing::info!("testing namer: {:?}", namer.get("gadget"));
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
                show(&split, &mut namer);
            }
            "quit" => {
                break;
            }
            "demo" => {
                demo(&split);
            }
            "new" => {
                new_file(&split, &mut namer);
            }
            "write" => {
                write_file(&split, &mut namer);
            }
            "read" => {
                read_file(&split, &mut namer);
            }
            "del" => {
                del_file(&split, &mut namer);
            }
            "lethe" => {
                lethe_cmd(&split, &mut namer);
            }

            _ => {
                println!("unknown command {}", split[0]);
            }
        }
    }
}
