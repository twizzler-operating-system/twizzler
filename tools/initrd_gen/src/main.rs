use std::{
    fs::{self, File},
    io::Seek,
    path::{Path, PathBuf},
};

use clap::{Arg, Command};
use tar::Builder;

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
    archive.sparse(false);
    archive.mode(tar::HeaderMode::Deterministic);

    for file in files.unwrap_or_default().map(|s| s.as_str()) {
        let mut f = File::open(file).expect(&format!("failed to open file: {}", file));
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
        if file.contains("libstd") {
            f.rewind().unwrap();
            archive
                .append_file(Path::new(file).file_name().unwrap(), &mut f)
                .unwrap();
        }
    }
    // Add data files (raw bytes — kernel sets MEXT_SIZED, runtime uses RawFile fallback)
    if let Ok(md) = fs::metadata(data_dir) {
        let mut data_files: Vec<PathBuf> = vec![];
        if md.is_dir() {
            data_files = walkdir::WalkDir::new(data_dir)
                .min_depth(1)
                .max_depth(3)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .map(|x| x.path().to_path_buf())
                .collect();
        } else if md.is_file() {
            data_files.push(PathBuf::from(data_dir));
        }

        for file in data_files {
            let mut f = File::open(&file).unwrap();
            archive
                .append_file(file.file_name().unwrap().to_str().unwrap(), &mut f)
                .unwrap();
        }
    }

    let _ = archive.into_inner().unwrap();
}
