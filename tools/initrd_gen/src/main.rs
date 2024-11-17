use std::{
    fs::{metadata, File},
    io::{Cursor, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use clap::{Arg, Command};
use tar::{Builder, Header};

#[repr(C)]
#[derive(Clone, Copy)]
struct FileMetadata {
    magic: u64,
    size: u64,
    direct: [u128; 255],
}

unsafe fn any_as_u8_slice<T: Sized>(p: &T) -> &[u8] {
    ::core::slice::from_raw_parts((p as *const T) as *const u8, ::core::mem::size_of::<T>())
}

fn main() {
    let app = Command::new("initrd_gen")
        .about("Create an initrd for Twizzler")
        .author("Daniel Bittman <danielbittman1@gmail.com>")
        .arg(
            Arg::new("output")
                .long("output")
                .short('o')
                .value_name("FILE")
                .help("Output file, the final initrd")
                .num_args(1)
                .required(true),
        )
        .arg(
            Arg::new("data")
                .long("data")
                .short('d')
                .value_name("FILE")
                .help("Data files to add to initrd")
                .default_value("./target/data")
                .num_args(1),
        )
        .arg(
            Arg::new("files")
                .help("Program files to add to initrd")
                .num_args(1..),
        );
    let matches = app.get_matches();
    let initrd_output = matches
        .get_one::<String>("output")
        .map(|s| s.as_str())
        .unwrap();

    let data_dir = matches
        .get_one::<String>("data")
        .map(|s| s.as_str())
        .unwrap();

    let files = matches.get_many::<String>("files");

    let outfile = File::create(initrd_output).unwrap();

    let mut archive = Builder::new(outfile);

    for file in files.unwrap_or_default().map(|s| s.as_str()) {
        let mut f = File::open(file).unwrap();
        archive
            .append_file(
                Path::new(file)
                    .file_name()
                    .map(|s| {
                        // TODO: HACK
                        if s.to_str().unwrap().starts_with("libstd") {
                            "libstd.so"
                        } else {
                            s.to_str().unwrap()
                        }
                    })
                    .unwrap(),
                &mut f,
            )
            .unwrap();
    }

    let mut data_files: Vec<PathBuf> = vec![];
    if let Ok(md) = metadata(&data_dir) {
        if md.is_dir() {
            let f: Vec<PathBuf> = walkdir::WalkDir::new(data_dir)
                .min_depth(1)
                .max_depth(3)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    if e.file_type().is_file() {
                        Some(e)
                    } else {
                        None
                    }
                })
                .map(|x| x.path().to_owned())
                .map(|x| x.to_path_buf())
                .collect();

            data_files.extend(f)
        } else if md.is_file() {
            data_files.push(PathBuf::from(data_dir));
        }
    }

    for file in data_files {
        let mut f = File::open(file.clone()).unwrap();
        let md = f.metadata().unwrap();

        let file_metadata = FileMetadata {
            magic: 0xBEEFDEAD,
            size: md.size(),
            direct: [0; 255],
        };

        let mut data: Vec<u8> = vec![];

        let fmd_bytes: &[u8] = unsafe { any_as_u8_slice(&file_metadata) };
        data.extend(fmd_bytes);

        f.read_to_end(&mut data).unwrap();
        let mut header = Header::new_old();

        header.set_size(data.len().try_into().unwrap());
        header
            .set_path(
                Path::new(&file)
                    .file_name()
                    .map(|s| s.to_str().unwrap())
                    .unwrap(),
            )
            .unwrap();
        header.set_uid(md.uid().into());
        header.set_gid(md.gid().into());
        header.set_mode(md.mode());
        header.set_cksum();

        archive
            .append_data(
                &mut header,
                Path::new(&file)
                    .file_name()
                    .map(|s| s.to_str().unwrap())
                    .unwrap(),
                Cursor::new(data),
            )
            .unwrap();
    }
}
