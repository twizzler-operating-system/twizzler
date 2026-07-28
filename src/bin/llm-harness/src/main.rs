//! Harness entry point.
//!
//! On Twizzler this exercises `TwizzlerEffects` against real objects. On the
//! host it runs the same loop against the in-memory stub, so the wiring stays
//! runnable outside QEMU.

use anyhow::Result;

/// The recorded completions, baked in at build time.
const RECORDING: &str = include_str!("../recordings/hello.json");

/// Seeded into the task object on first run, then read back through `Effects`
/// like any other object.
const TASK: &str = "Write a greeter module and a test for it.";

const TASK_OBJECT: &str = "task.md";

fn main() -> Result<()> {
    println!("=== llm-harness: up ===");
    run()
}

#[cfg(target_os = "twizzler")]
fn run() -> Result<()> {
    use llm_harness::{effects::Effects, twz::DEFAULT_PREFIX, Agent, RecordedSource, TwizzlerEffects};

    let mut effects = TwizzlerEffects::new(DEFAULT_PREFIX)?;
    println!("effects: objects under {}", effects.prefix());

    // Seed the task object so there is something to read. On a later run it is
    // already there; either way the loop only ever sees it through Effects.
    let task_handle = effects.open(TASK_OBJECT)?;
    if effects.read(task_handle)?.is_empty() {
        effects.write(task_handle, TASK.as_bytes())?;
        println!("seeded {TASK_OBJECT}");
    }
    let task = String::from_utf8(effects.read(task_handle)?)?;
    println!("task object: {task:?}");

    let source = RecordedSource::from_json(RECORDING.as_bytes())?;
    let mut agent = Agent::new(source, effects);
    let stop = agent.run(&task)?;
    println!("loop stopped: {stop:?} ({} messages)", agent.transcript().len());

    println!("\neffect log:");
    print!("{}", agent.log().render());

    // Read the generated files back out of their objects, to show the writes
    // landed somewhere real rather than just being logged.
    println!("generated objects:");
    let mut effects = agent.into_effects();
    for name in ["src/greet.rs", "tests/greet.rs"] {
        let h = effects.open(name)?;
        let body = String::from_utf8(effects.read(h)?)?;
        println!("--- {name} ({} bytes) ---", body.len());
        for line in body.lines() {
            println!("  {line}");
        }
    }

    println!("=== llm-harness: end to end OK ===");
    Ok(())
}

#[cfg(not(target_os = "twizzler"))]
fn run() -> Result<()> {
    use llm_harness::{Agent, MemEffects, RecordedSource};

    let mut effects = MemEffects::new();
    effects.preload(TASK_OBJECT, TASK);

    let source = RecordedSource::from_json(RECORDING.as_bytes())?;
    let mut agent = Agent::new(source, effects);

    let stop = agent.run(TASK)?;
    println!(
        "llm-harness: stopped ({stop:?}) after {} messages",
        agent.transcript().len()
    );

    println!("\nobjects written:");
    for name in agent.effects().names() {
        let len = agent.effects().get(name).map(|b| b.len()).unwrap_or(0);
        println!("  {name} ({len} bytes)");
    }

    println!("\neffect log:");
    print!("{}", agent.log().render());

    Ok(())
}
