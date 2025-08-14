#![feature(error_reporter)]

use clap::Parser;
use miette::IntoDiagnostic;
use monitor_api::{CompartmentLoader, NewCompartmentFlags};
use tracing::Level;
use twizzler_abi::{
    syscall::TraceSpec,
    trace::{CONTEXT_FAULT, TraceFlags, TraceKind},
};

pub mod tracer;

#[derive(Debug, Clone, clap::Subcommand)]
enum Subcommand {
    Stat,
}

#[derive(clap::Args, Clone, Debug)]
struct RunCli {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    cmdline: Vec<String>,
}

#[derive(clap::Parser, Clone, Debug)]
struct Cli {
    #[clap(subcommand)]
    cmd: Option<Subcommand>,
    #[clap(flatten)]
    prog: RunCli,
}

fn main() -> miette::Result<()> {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .without_time()
            .with_max_level(Level::DEBUG)
            .finish(),
    )
    .unwrap();

    let cli = Cli::try_parse().into_diagnostic()?;

    run_trace_program(&cli.prog)?;

    Ok(())
}

fn run_trace_program(run_cli: &RunCli) -> miette::Result<()> {
    let name = &run_cli.cmdline[0];
    let compname = format!("trace-{}", name);

    let mut comp = CompartmentLoader::new(&compname, name, NewCompartmentFlags::DEBUG);
    comp.args(&run_cli.cmdline);
    let comp = comp.load().into_diagnostic()?;

    tracing::info!("compartment {} loaded, starting tracing monitor", compname);

    let info = comp.info();
    let spec = TraceSpec {
        kind: TraceKind::Context,
        flags: TraceFlags::empty(),
        enable_events: CONTEXT_FAULT,
        disable_events: 0,
        sctx: Some(info.id),
        mctx: None,
        thread: None,
        cpuid: None,
        extra: 0.into(),
    };
    let state = tracer::start(comp, vec![spec])?;

    tracing::info!("disconnected {}: {:?}", compname, state);

    let mut count = 0;
    for entry in state.data() {
        if entry.0.kind != TraceKind::Context {
            tracing::info!("==> {:?}", entry);
        }
        count += 1;
    }

    tracing::info!("counted {} events", count);

    Ok(())
}
