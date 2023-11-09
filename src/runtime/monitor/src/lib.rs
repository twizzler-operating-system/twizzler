use tracing::{debug, trace, Level};
use tracing_subscriber::FmtSubscriber;

mod init;
mod runtime;
mod state;

pub fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    trace!("monitor entered, discovering dynlink context");
    let init =
        init::bootstrap_dynlink_context().expect("failed to discover initial dynlink context");
    let state = state::MonitorState::new(init);
    debug!(
        "found dynlink context, with {} root libraries",
        state.roots.len()
    );

    panic!("test panic");
    loop {}
}
