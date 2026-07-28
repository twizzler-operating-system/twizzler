//! Commit 1 is host-only: this driver runs the loop against the in-memory
//! backend so the wiring is exercised outside of QEMU. The Twizzler entry
//! point replaces it in a later commit.

use anyhow::Result;
use llm_harness::{Agent, MemEffects, RecordedSource};

/// Stands in for the recording object that will be baked in at build time.
const RECORDING: &str = include_str!("../recordings/hello.json");

const TASK: &str = "Write a greeter module and a test for it.";

fn main() -> Result<()> {
    let mut effects = MemEffects::new();
    effects.preload("task.md", TASK);

    let source = RecordedSource::from_json(RECORDING.as_bytes())?;
    let mut agent = Agent::new(source, effects);

    let stop = agent.run(TASK)?;
    println!("llm-harness: stopped ({stop:?}) after {} messages", agent.transcript().len());

    println!("\nobjects written:");
    for name in agent.effects().names() {
        let len = agent.effects().get(name).map(|b| b.len()).unwrap_or(0);
        println!("  {name} ({len} bytes)");
    }

    println!("\neffect log:");
    print!("{}", agent.log().render());

    Ok(())
}
