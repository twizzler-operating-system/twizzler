use std::sync::Arc;

use tracing::{debug, info, trace, Level};
use tracing_subscriber::FmtSubscriber;

use crate::runtime::init_actions;

mod init;
mod runtime;
mod state;

pub fn main() {
    std::env::set_var("RUST_BACKTRACE", "full");
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    trace!("monitor entered, discovering dynlink context");
    let init =
        init::bootstrap_dynlink_context().expect("failed to discover initial dynlink context");
    let state = Arc::new(state::MonitorState::new(init));
    debug!(
        "found dynlink context, with {} root libraries",
        state.roots.len()
    );

    init_actions(state);
    std::env::set_var("RUST_BACKTRACE", "1");

    let thread = std::thread::spawn(|| {
        info!("hello from thread main");
        0
    });

    let r = thread.join();

    info!("==> {:?}", r);

    loop {}
}
