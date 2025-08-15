#![feature(error_reporter)]

use clap::Parser;
use miette::IntoDiagnostic;
use monitor_api::{CompartmentLoader, NewCompartmentFlags};
use tracer::TracingState;
use tracing::Level;
use twizzler_abi::{
    syscall::TraceSpec,
    trace::{
        CONTEXT_FAULT, CONTEXT_INVALIDATION, CONTEXT_SHOOTDOWN, THREAD_SYSCALL_ENTRY, TraceFlags,
        TraceKind,
    },
};

pub mod stat;
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
    #[clap(long, short, help = "List of events to traces, one per flag.")]
    events: Vec<String>,
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

    let state = run_trace_program(&cli.prog, &cli.events)?;

    match cli.cmd {
        None | Some(Subcommand::Stat) => {
            stat::stat(state);
        }
    }

    Ok(())
}

fn run_trace_program(run_cli: &RunCli, events: &[String]) -> miette::Result<TracingState> {
    let name = &run_cli.cmdline[0];
    let compname = format!("trace-{}", name);

    let mut comp = CompartmentLoader::new(&compname, name, NewCompartmentFlags::DEBUG);
    comp.args(&run_cli.cmdline);
    let comp = comp.load().into_diagnostic()?;

    tracing::info!("compartment {} loaded, starting tracing monitor", compname);

    let info = comp.info();

    let specs = events
        .iter()
        .map(|event| match event.as_str() {
            "page-faults" | "pf" | "faults" | "page-fault" => TraceSpec {
                kind: TraceKind::Context,
                flags: TraceFlags::empty(),
                enable_events: CONTEXT_FAULT,
                disable_events: 0,
                sctx: Some(info.id),
                mctx: None,
                thread: None,
                cpuid: None,
                extra: 0.into(),
            },
            "tlb" | "tlb-shootdowns" | "tlb-shootdown" | "shootdown" => TraceSpec {
                kind: TraceKind::Context,
                flags: TraceFlags::empty(),
                enable_events: CONTEXT_SHOOTDOWN | CONTEXT_INVALIDATION,
                disable_events: 0,
                sctx: Some(info.id),
                mctx: None,
                thread: None,
                cpuid: None,
                extra: 0.into(),
            },
            "sys" | "syscall" | "syscalls" => TraceSpec {
                kind: TraceKind::Thread,
                flags: TraceFlags::empty(),
                enable_events: THREAD_SYSCALL_ENTRY,
                disable_events: 0,
                sctx: Some(info.id),
                mctx: None,
                thread: None,
                cpuid: None,
                extra: 0.into(),
            },
            _ => panic!("unknown event type: {}", event),
        })
        .collect();

    let state = tracer::start(comp, specs)?;

    tracing::info!(
        "disconnected {}: {} bytes of trace data",
        compname,
        state.total
    );

    tracing::info!("counted {} events", state.data().count());

    Ok(state)
}
