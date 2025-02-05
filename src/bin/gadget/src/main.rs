use std::{
    fs::OpenOptions,
    io::{Read, Write},
    net::Ipv4Addr,
};

use embedded_io::ErrorType;
use logboi::LogHandle;
use monitor_api::{CompartmentHandle, LibraryHandle};
use naming::{static_naming_factory, StaticNamingAPI, StaticNamingHandle as NamingHandle};
use pager::adv_lethe;
use tiny_http::{Response, StatusCode};
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
            pager::adv_lethe();
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
        "l" | "le" | "lethe" => {
            pager::show_lethe();
        }
        "c" | "comp" | "compartments" => {
            fn print_compartment(ch: CompartmentHandle) {
                let info = ch.info();
                println!(" -- {} (state: {:?})", info.name, info.flags);
                for lib in ch.libs() {
                    let libinfo = lib.info();
                    println!("     -- {:30} {}", libinfo.name, libinfo.objid,)
                }
            }

            let gadget = monitor_api::CompartmentHandle::lookup("gadget").unwrap();
            let init = monitor_api::CompartmentHandle::lookup("init").unwrap();
            let monitor = monitor_api::CompartmentHandle::lookup("monitor").unwrap();
            let namer = monitor_api::CompartmentHandle::lookup("naming").unwrap();
            let logger = monitor_api::CompartmentHandle::lookup("logboi").unwrap();
            let pager = monitor_api::CompartmentHandle::lookup("pager-srv").unwrap();
            print_compartment(monitor);
            print_compartment(init);
            print_compartment(gadget);
            print_compartment(namer);
            print_compartment(logger);
            print_compartment(pager);
        }
        "f" | "fi" | "files" => {
            let names = namer.enumerate_names().unwrap();
            for name in names {
                println!("{:<20} :: {:?}", name.name, name.entry_type);
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
    let Ok(id) = namer.get(filename) else {
        tracing::warn!("name {} not found", filename);
        return;
    };

    //let idname = id.to_string();
    let mut file = std::fs::File::open(&filename).unwrap();
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
    let Ok(id) = namer.get(filename) else {
        tracing::warn!("name {} not found", filename);
        return;
    };

    let data = format!("hello gadget from file {}", filename);
    let idname = id.to_string();
    let mut file = OpenOptions::new().write(true).open(filename).unwrap();
    tracing::warn!("for now, we just write test data: `{}'", data);
    file.write(data.as_bytes()).unwrap();

    tracing::info!("calling sync!");
    file.sync_all().unwrap();
}

fn new_file(args: &[&str], namer: &mut NamingHandle) {
    if args.len() < 2 {
        println!("usage: new <filename>");
        return;
    }
    let filename = args[1];
    if namer.get(filename).is_ok() {
        tracing::warn!("name {} already exists", filename);
        return;
    };
    /*
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
    */

    let _f = std::fs::File::create(filename).unwrap();
    tracing::debug!("created new file object {:x}", namer.get(filename).unwrap());
}

fn del_file(args: &[&str], namer: &mut NamingHandle) {
    if args.len() < 2 {
        println!("usage: write <filename>");
    }
    let filename = args[1];
    let Ok(_) = namer.get(filename) else {
        tracing::warn!("name {} not found", filename);
        return;
    };
    tracing::info!("deleting file...");
    let res = std::fs::remove_file(&filename);
    tracing::info!("got: {:?}", res);
    if res.is_err() {
        return;
    }
    tracing::info!("removing name...");
    namer.remove(filename, false).unwrap();
    tracing::info!("epoch...");
    adv_lethe();
}

fn setup_http(namer: &mut NamingHandle) {
    tracing::info!("setting up http");
    let server = tiny_http::Server::http((Ipv4Addr::new(127, 0, 0, 1), 5555)).unwrap();
    tracing::info!("server ready");
    let mut reqs = server.incoming_requests();
    tracing::info!("waiting for requests");
    while let Some(mut request) = reqs.next() {
        tracing::info!("request: {:?}", request);
        let mut buf = Vec::new();
        request.as_reader().read_to_end(&mut buf);
        let path = request.url().to_string();
        tracing::info!("path: {}", path);
        let _ = match request.method() {
            tiny_http::Method::Get => {
                let file = OpenOptions::new().read(true).open(&path);
                match file {
                    Ok(file) => request.respond(Response::from_file(file)),
                    Err(e) => request.respond(
                        Response::from_string(format!("file {} not found", path))
                            .with_status_code(404),
                    ),
                }
            }
            tiny_http::Method::Post => {
                let mut file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&path);

                match file {
                    Ok(mut file) => {
                        file.write(&buf);
                        file.sync_all();
                        request.respond(Response::empty(200))
                    }
                    Err(e) => request.respond(
                        Response::from_string(format!("file {} could not be created", path))
                            .with_status_code(500),
                    ),
                }
            }
            _ => request.respond(Response::empty(400)),
        }
        .unwrap();
    }
}

fn main() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG)
            .without_time()
            .finish(),
    )
    .unwrap();
    tracing_log::LogTracer::init().unwrap();

    let mut namer = static_naming_factory().unwrap();
    let mut logger = LogHandle::new().unwrap();
    logger.log(b"Hello Logger!\n");
    //tracing::info!("testing namer: {:?}", namer.get("initrd/gadget"));
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
            "http" => {
                setup_http(&mut namer);
            }

            _ => {
                println!("unknown command {}", split[0]);
            }
        }
    }
}
