use clap::{Parser, Subcommand};
use etl_twizzler::etl::{Pack, PackType, Unpack};

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

            pack.build();
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
}
