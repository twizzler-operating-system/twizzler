//! A compartment that starts and exits, and does nothing else.
//!
//! The spawn-side analogue of Linux's `fork`+`exec` of a do-nothing binary: sysbench's
//! `compartment_spawn_exit` times `Command::spawn` + `wait` against this, so whatever the number
//! is, it is compartment creation, dynamic linking, runtime entry and teardown -- not the
//! program's own work. Deliberately has no dependencies beyond the default runtime, since every
//! DSO added here lands in the measurement.
//!
//! Returning from `main` is the default because that is what the C analogue
//! (`int main(void) { return 0; }`) does: libc's exit path runs. `--exit-now` calls
//! `process::exit` instead, which skips Rust's teardown -- the difference between the two arms
//! is that teardown, and it is worth having separately because the other spawn target in the
//! suite (`leakcheck --child-exit`) takes the `process::exit` route.
fn main() {
    if std::env::args().any(|a| a == "--exit-now") {
        std::process::exit(0);
    }
}
