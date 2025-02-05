use std::{
    fs::OpenOptions,
    io::{Read, Write},
    net::Ipv4Addr,
    time::Duration,
};

use embedded_io::ErrorType;
use monitor_api::CompartmentHandle;
use naming::{static_naming_factory, EntryType, ErrorKind, StaticNamingHandle as NamingHandle};
use pager::adv_lethe;
use tiny_http::Response;
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

fn lethe_cmd(args: &[&str], _namer: &mut NamingHandle) {
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
                if let EntryType::Object(id) = name.entry_type {
                    println!("{:<20} :: {:x}", name.name, id);
                }
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
    let Ok(_id) = namer.get(filename) else {
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
    let Ok(_id) = namer.get(filename) else {
        tracing::warn!("name {} not found", filename);
        return;
    };

    let data = format!("hello gadget from file {}", filename);
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

    tracing::info!("creating new file: {}", filename);
    let _f = std::fs::File::create(filename).unwrap();
    tracing::info!("created new file object {:x}", namer.get(filename).unwrap());
}

fn del_file(args: &[&str], namer: &mut NamingHandle) {
    if args.len() < 2 {
        println!("usage: write <filename>");
    }
    let filename = args[1];
    let Ok(id) = namer.get(filename) else {
        tracing::warn!("name {} not found", filename);
        return;
    };
    tracing::info!("deleting file {}, objid: {}", filename, id);
    std::fs::remove_file(&filename).unwrap();
    //tracing::info!("removing name...");
    namer.remove(filename, false).unwrap();
    tracing::info!("This now requires we issue a lethe epoch, since keys have changed.");
    tracing::info!("Epoch...");
    adv_lethe();
}

fn setup_http(namer: &mut NamingHandle) {
    tracing::info!("setting up http");
    let server = tiny_http::Server::http((Ipv4Addr::new(127, 0, 0, 1), 5555)).unwrap();
    tracing::info!("server ready");
    let mut reqs = server.incoming_requests();
    while let Some(mut request) = reqs.next() {
        if let Some(ra) = request.remote_addr() {
            tracing::info!("connection from: {}", ra);
        }
        let mut buf = Vec::new();
        let path = request.url().to_string();
        tracing::info!("serving {} {}", request.method(), path);
        request.as_reader().read_to_end(&mut buf).unwrap();
        let _ = match request.method() {
            tiny_http::Method::Get => match namer.change_namespace(&path) {
                Ok(_) => {
                    let names = namer.enumerate_names().unwrap();
                    let mut html = String::from(
                        "<!DOCTYPE html><html><head><title>Index</title></head><body><ul>",
                    );

                    for entry in names {
                        match entry.entry_type {
                            EntryType::Object(_) => {
                                html.push_str(&format!(
                                    r#"<li><a href="{}/">{}/</a></li>"#,
                                    entry.name.as_str(),
                                    entry.name.as_str()
                                ));
                            }
                            EntryType::Namespace => {
                                html.push_str(&format!(
                                    r#"<li><a href="{}/">{}/</a></li>"#,
                                    entry.name.as_str(),
                                    entry.name.as_str()
                                ));
                            }
                            _ => {}
                        }
                    }

                    html.push_str("</ul></body></html>");

                    let header =
                        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html"[..])
                            .unwrap();
                    request.respond(Response::from_string(html).with_header(header))
                }
                Err(ErrorKind::NotNamespace) => {
                    let file = OpenOptions::new().read(true).open(&path);
                    match file {
                        Ok(file) => request.respond(Response::from_file(file)),
                        Err(e) => request.respond(
                            Response::from_string(format!("file {} not found: {}", path, e))
                                .with_status_code(500),
                        ),
                    }
                }
                Err(ErrorKind::NotFound) => request.respond(
                    Response::from_string(format!("file {} not found", path)).with_status_code(404),
                ),
                Err(e) => request.respond(
                    Response::from_string(format!("error: {:?}", e)).with_status_code(500),
                ),
            },
            tiny_http::Method::Post => {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&path);
                tracing::info!("created new file object {:x}", namer.get(&path).unwrap());

                match file {
                    Ok(mut file) => {
                        tracing::info!("writing...");
                        file.write(&buf).unwrap();
                        tracing::info!("syncing...");
                        file.sync_all().unwrap();
                        request.respond(Response::empty(200))
                    }
                    Err(e) => request.respond(
                        Response::from_string(format!("file {} could not be created: {}", path, e))
                            .with_status_code(500),
                    ),
                }
            }
            tiny_http::Method::Delete => {
                match std::fs::remove_file(&path) {
                    Ok(()) => {
                        namer.remove(&path, false).unwrap();
                        pager::adv_lethe();
                        request.respond(Response::empty(200))
                    }
                    Err(e) => {
                        request.respond(
                                    Response::from_string(format!("error: {:?}", e))
                                        .with_status_code(500), // internal error
                                )
                    }
                }
            }
            _ => request.respond(Response::empty(400)),
        }
        .unwrap();
    }
}

fn banner() -> &'static str {
    r"
 ___  _ _ _  _  __  ___  ___  __
|_ _|| | | || |/ _||_ _|| __||  \
 | | | V V || |\_ \ | | | _| | o )
 |_|  \_n_/ |_||__/ |_| |___||__/"
}

fn main() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .without_time()
            .finish(),
    )
    .unwrap();
    tracing_log::LogTracer::init().unwrap();

    let mut namer = static_naming_factory().unwrap();
    //let mut logger = LogHandle::new().unwrap();
    //logger.log(b"Hello Logger!\n");

    std::thread::spawn(|| {
        let mut namer = static_naming_factory().unwrap();
        setup_http(&mut namer);
    });

    //tracing::info!("testing namer: {:?}", namer.get("initrd/gadget"));

    std::thread::sleep(Duration::from_millis(500));
    println!("{}", banner());
    println!("       TWISTED GADGET DEMO");

    let mut io = TwzIo;
    let mut buffer = [0; 1024];
    let mut editor = noline::builder::EditorBuilder::from_slice(&mut buffer)
        .build_sync(&mut io)
        .unwrap();
    loop {
        let line = editor.readline("gadget> ", &mut io).unwrap();
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
            //"http" => {
            //    setup_http(&mut namer);
            //}
            _ => {
                println!("unknown command {}", split[0]);
            }
        }
    }
}
