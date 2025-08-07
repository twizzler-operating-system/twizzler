use std::time::Instant;

use clap::Parser;
use miette::IntoDiagnostic;
use naming::{GetFlags, dynamic_naming_factory};
use tracing::Level;
use twizzler::object::{MapFlags, ObjID};

#[derive(clap::Args, Clone, Debug)]
struct TrailingArgs {
    #[arg(
        help("List of names or object IDs"),
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    args: Vec<String>,
}

#[derive(clap::Subcommand, Clone, Debug)]
enum Command {
    #[clap(about = "Hold an object.")]
    Hold(TrailingArgs),
    #[clap(about = "Drop holds on an object.")]
    Drop(TrailingArgs),
    #[clap(about = "Preload an object.")]
    Preload(TrailingArgs),
    #[clap(about = "Print stats about an object.")]
    Stat(TrailingArgs),
    #[clap(about = "List held objects.")]
    List,
}

#[derive(clap::Parser, Clone)]
struct Args {
    #[clap(subcommand)]
    cmd: Command,
}

fn do_hold(id: ObjID) -> twizzler::Result<()> {
    tracing::info!("do hold: {}", id);
    cache_srv::hold(id, MapFlags::READ)?;
    cache_srv::hold(id, MapFlags::READ | MapFlags::WRITE)?;
    cache_srv::hold(id, MapFlags::READ | MapFlags::WRITE | MapFlags::PERSIST)?;
    Ok(())
}

fn do_drop(id: ObjID) -> twizzler::Result<()> {
    tracing::info!("do drop: {}", id);
    cache_srv::drop(id, MapFlags::READ)?;
    cache_srv::drop(id, MapFlags::READ | MapFlags::WRITE)?;
    cache_srv::drop(id, MapFlags::READ | MapFlags::WRITE | MapFlags::PERSIST)?;
    Ok(())
}

fn do_preload(id: ObjID) -> twizzler::Result<()> {
    tracing::info!("do preload: {}", id);
    cache_srv::preload(id)
}

fn do_stat(id: ObjID) -> twizzler::Result<()> {
    tracing::info!("do stat: {}", id);
    cache_srv::stat(id)
}

fn per_arg(arg: &str, cb: fn(ObjID) -> twizzler::Result<()>) -> twizzler::Result<()> {
    if let Ok(id) = u128::from_str_radix(arg, 16) {
        match cb(id.into()) {
            Ok(_) => return twizzler::Result::Ok(()),
            Err(e) => tracing::debug!(
                "failed to operate on parsed object ID {} ({}), trying again with name",
                e,
                id
            ),
        }
    }
    match dynamic_naming_factory()
        .unwrap()
        .get(arg, GetFlags::FOLLOW_SYMLINK)
    {
        Err(e) => {
            tracing::warn!("could not resolve {}: {}", arg, e);
        }
        Ok(ns) => match ns.kind {
            naming::NsNodeKind::Namespace => {
                tracing::debug!("enumerating directory {}", arg);
                match dynamic_naming_factory()
                    .unwrap()
                    .enumerate_names_nsid(ns.id)
                {
                    Ok(nodes) => {
                        for node in nodes {
                            if let Ok(name) = node.name() {
                                if name != "." && name != ".." {
                                    let name = format!("{}/{}", arg, name);
                                    let _ = per_arg(name.as_str(), cb);
                                }
                                // TODO: track error for ret val
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("could not enumerate directory {}: {}", arg, e);
                    }
                }
            }
            naming::NsNodeKind::Object => {
                if let Err(e) = cb(ns.id) {
                    tracing::warn!("could not operate on {}: {}", arg, e);
                }
            }
            naming::NsNodeKind::SymLink => {
                tracing::warn!("cannot hold / preload symlinks")
            }
        },
    }
    Ok(())
}

fn main() -> miette::Result<()> {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG)
            .finish(),
    )
    .unwrap();

    let args = Args::try_parse().into_diagnostic()?;

    let mut errs = false;
    match args.cmd {
        Command::Hold(args) => {
            for arg in args.args {
                if per_arg(&arg, do_hold).is_err() {
                    errs = true;
                }
            }
        }
        Command::Drop(args) => {
            for arg in args.args {
                if per_arg(&arg, do_drop).is_err() {
                    errs = true;
                }
            }
        }
        Command::Preload(args) => {
            for arg in args.args {
                if per_arg(&arg, do_preload).is_err() {
                    errs = true;
                }
            }
        }
        Command::Stat(args) => {
            for arg in args.args {
                if per_arg(&arg, do_stat).is_err() {
                    errs = true;
                }
            }
        }
        Command::List => {
            let mut i = 0;
            while let Some(info) = cache_srv::list_nth(i).into_diagnostic()? {
                println!(
                    "{} {:?} {} seconds old",
                    info.id,
                    info.flags,
                    (Instant::now() - info.start).as_secs_f32()
                );
                i += 1;
            }
        }
    }

    if errs {
        miette::bail!("errors occurred")
    }
    Ok(())
}
