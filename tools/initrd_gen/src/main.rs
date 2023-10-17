use std::{fs::File, path::Path};

use clap::{Command, Arg};
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
            Arg::new("files")
                .help("File to add to initrd")
                .num_args(1..),
        );
    let matches = app.get_matches();
    let initrd_output = matches.get_one::<String>("output").map(|s| s.as_str()).unwrap();
    let files = matches.get_many::<String>("files");
    let outfile = File::create(initrd_output).unwrap();
    let mut archive = Builder::new(outfile);
    for file in files.unwrap_or_default().map(|s| s.as_str()) {
        let mut f = File::open(file).unwrap();
        archive
            .append_file(Path::new(file).file_name().unwrap(), &mut f)
            .unwrap();
    }
}
