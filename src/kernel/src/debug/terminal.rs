use limine::{LimineTerminalRequest, LimineTerminalResponse};
use core::fmt::Write;

#[used]
static TERMINAL: LimineTerminalRequest = LimineTerminalRequest::new(0);

#[link_section = ".limine_reqs"]
#[used]
static LR3: &'static LimineTerminalRequest = &TERMINAL;

use crate::once::Once;

static TERM: Once<Option<&'static LimineTerminalResponse>> = Once::new();

// The terminal feature in Limine provides a service to send serial
// output to an external display.
struct LimineTerminal;

impl Write for LimineTerminal {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        print(s);
        Ok(())
    }
}

pub fn print(msg: &str) {

    // initialize terminal
    TERM.call_once(|| {
        TERMINAL
            .get_response() // limine ptr
            .get() // option
    });

    if let Some(termref) = *TERM.wait() {
        let out = termref.write().unwrap();
        let terminals = termref.terminals();

        for term in terminals {
            out(&term[0], msg);
        }
    } 
    #[cfg(machine = "virt")]
    crate::machine::serial::EARLY_SERIAL.write_str(msg);
}

pub fn _print_terminal(args: ::core::fmt::Arguments) {
    LimineTerminal
            .write_fmt(args)
            .expect("printing to terminal failed");
}

#[macro_export]
macro_rules! term {
    ($($arg:tt)*) => {
        $crate::debug::_print_terminal(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! terminal {
    () => {
        $crate::term!("\n")
    };
    ($fmt:expr) => {
        $crate::term!(concat!($fmt, "\n"))
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::term!(concat!($fmt, "\n"), $($arg)*)
    };
}
