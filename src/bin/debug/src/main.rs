use clap::Parser;
use gdb::{TwizzlerConn, TwizzlerGdb, TwizzlerTarget};
use gdbstub::stub::GdbStub;
use miette::IntoDiagnostic;
use monitor_api::{CompartmentLoader, NewCompartmentFlags};

mod gdb;

#[derive(clap::Subcommand, Clone, Debug)]
enum Commands {
    #[clap(about = "Run a program and debug it.")]
    Run(RunCli),
    #[clap(about = "Attach to an existing compartment.")]
    Attach,
}

#[derive(clap::Args, Clone, Debug)]
struct RunCli {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    cmdline: Vec<String>,
}

#[derive(clap::Parser, Clone, Debug)]
#[clap(name = "debug", author = "Daniel Bittman <danielbittman1@gmail.com>", version = "1.0", about = "Debugger stub for Twizzler", long_about = None)]
struct Cli {
    #[clap(subcommand)]
    cmd: Commands,
}

fn main() -> miette::Result<()> {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .finish(),
    )
    .into_diagnostic()?;
    tracing_log::LogTracer::init().into_diagnostic()?;

    let cli = Cli::parse();
    tracing::info!("Twizzler Debugging Starting");

    match cli.cmd {
        Commands::Run(run_cli) => {
            run_debug_program(&run_cli)?;
        }
        Commands::Attach => todo!(),
    }

    Ok(())
}

fn run_debug_program(run_cli: &RunCli) -> miette::Result<()> {
    let name = &run_cli.cmdline[0];
    let compname = format!("debug-{}", name);

    let mut comp = CompartmentLoader::new(compname, name, NewCompartmentFlags::empty());
    comp.args(&run_cli.cmdline);
    let comp = comp.load().into_diagnostic()?;

    let (send, recv) = std::sync::mpsc::channel();
    let gdb = GdbStub::new(TwizzlerConn::new(recv));
    let mut target = TwizzlerTarget::new(comp, send);
    let r = gdb
        .run_blocking::<TwizzlerGdb>(&mut target)
        .into_diagnostic()?;

    tracing::info!("disconnected: {:?}", r);
    Ok(())
}
