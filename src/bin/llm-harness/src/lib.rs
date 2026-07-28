//! A minimal coding-agent harness.
//!
//! The loop in [`agent`] is the only thing that orchestrates; everything it
//! depends on is a trait so the backing implementation can change without the
//! loop noticing:
//!
//! - [`source::TokenSource`] — where model turns come from. Today only
//!   [`source::RecordedSource`] (completions baked in at build time).
//! - [`effects::Effects`] — the only route to the outside world. Today only
//!   [`effects::MemEffects`] (in-memory, host tests); `TwizzlerEffects` lands
//!   in a later commit.
//! - [`action::Action`] — the closed set of steps a turn may request, parsed
//!   by serde rather than by hand.

pub mod action;
pub mod agent;
pub mod effects;
pub mod log;
pub mod source;
pub mod transcript;

#[cfg(target_os = "twizzler")]
pub mod twz;

pub use action::Action;
pub use agent::{Agent, Stop};
pub use effects::{Effects, Handle, MemEffects};
pub use log::{EffectEntry, EffectLog, Outcome};
pub use source::{RecordedSource, TokenSource};
pub use transcript::{Msg, Role, Transcript};

#[cfg(target_os = "twizzler")]
pub use twz::TwizzlerEffects;
