use std::fs::File;

use clap::{App, Arg};
use tar::Builder;

fn main() {
    let app = App::new("initrd_gen")
        .about("Create an initrd for Twizzler")
        .author("Daniel Bittman <danielbittman1@gmail.com>")
        .arg(
            Arg::with_name("output")
                .long("output")
                .short("o")
                .value_name("FILE")
                .help("Output file, the final initrd")
                .takes_value(true)
                .required(true),
        )
        .arg(
            Arg::with_name("files")
                .help("File to add to initrd")
                .multiple(true),
        );
    let matches = app.get_matches();
    let initrd_output = matches.value_of("output").unwrap();
    let files = matches.values_of("files");
    let outfile = File::create(initrd_output).unwrap();
    let mut archive = Builder::new(outfile);
    for file in files.unwrap_or(clap::Values::default()) {
        archive.append_path(file).unwrap();
    }
}
