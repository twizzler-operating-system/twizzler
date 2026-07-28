//! Harness entry point.
//!
//! On Twizzler this exercises `TwizzlerEffects` against real objects. On the
//! host it runs the same loop against the in-memory stub, so the wiring stays
//! runnable outside QEMU.

use anyhow::Result;

fn main() -> Result<()> {
    println!("=== llm-harness: up ===");
    run()
}

#[cfg(target_os = "twizzler")]
fn run() -> Result<()> {
    use llm_harness::{effects::Effects, twz::DEFAULT_PREFIX, TwizzlerEffects};

    // Commit 3: prove the object backend round-trips before wiring the loop to it.
    let mut effects = TwizzlerEffects::new(DEFAULT_PREFIX)?;
    println!("effects: objects under {}", effects.prefix());

    let payload = b"twizzler effects round-trip";
    let h = effects.open("smoke.txt")?;
    println!("opened smoke.txt");

    effects.write(h, payload)?;
    println!("wrote {} bytes", payload.len());

    let got = effects.read(h)?;
    println!("read back {} bytes: {:?}", got.len(), String::from_utf8_lossy(&got));

    if got != payload {
        anyhow::bail!("round-trip mismatch: wrote {payload:?}, read {got:?}");
    }
    println!("=== llm-harness: object round-trip OK ===");
    Ok(())
}

#[cfg(not(target_os = "twizzler"))]
fn run() -> Result<()> {
    use llm_harness::{Agent, MemEffects, RecordedSource};

    /// Stands in for the recording object that will be baked in at build time.
    const RECORDING: &str = include_str!("../recordings/hello.json");
    const TASK: &str = "Write a greeter module and a test for it.";

    let mut effects = MemEffects::new();
    effects.preload("task.md", TASK);

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
