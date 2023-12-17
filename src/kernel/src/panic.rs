use core::panic::PanicInfo;

use addr2line::Context;
use object::{Object, ObjectSection};

use crate::interrupt::disable;

static mut DEBUG_CTX: Option<
    Context<addr2line::gimli::EndianReader<addr2line::gimli::RunTimeEndian, alloc::rc::Rc<[u8]>>>,
> = None;

fn load_debug_context(
    file: &object::read::elf::ElfFile64,
) -> Option<
    addr2line::Context<addr2line::gimli::read::EndianRcSlice<addr2line::gimli::RunTimeEndian>>,
> {
    let endian = addr2line::gimli::RunTimeEndian::Little; //TODO
    fn load_section(
        id: addr2line::gimli::SectionId,
        file: &object::read::elf::ElfFile64,
        endian: addr2line::gimli::RunTimeEndian,
    ) -> Result<addr2line::gimli::read::EndianRcSlice<addr2line::gimli::RunTimeEndian>, object::Error>
    {
        let data = file
            .section_by_name(id.name())
            .and_then(|section| section.uncompressed_data().ok())
            .unwrap_or(alloc::borrow::Cow::Borrowed(&[]));
        Ok(addr2line::gimli::EndianRcSlice::new(
            alloc::rc::Rc::from(&*data),
            endian,
        ))
    }

    let result = addr2line::gimli::Dwarf::load(|id| load_section(id, file, endian));
    match result {
        Ok(dwarf) => match addr2line::Context::from_dwarf(dwarf) {
            Ok(dwarf) => Some(dwarf),
            Err(e) => {
                logln!("loading debug information failed: {:?}", e);
                None
            }
        },
        Err(e) => {
            logln!("loading debug information failed: {:?}", e);
            None
        }
    }
}

pub fn init(kernel_image: &'static [u8]) {
    let image =
        object::read::elf::ElfFile64::parse(kernel_image).expect("failed to parse kernel image");
    let ctx = load_debug_context(&image);
    unsafe { DEBUG_CTX = ctx };
}
#[cfg(feature = "std")]
const MAX_FRAMES: usize = 100;
pub fn backtrace(symbolize: bool, entry_point: Option<backtracer_core::EntryPoint>) {
    let mut frame_nr = 0;
    let trace_callback = |frame: &backtracer_core::Frame| {
        let ip = frame.ip();

        if !symbolize {
            emerglogln!("{:4} - {:18p}", frame_nr, ip);
        } else {
            // Resolve this instruction pointer to a symbol name
            let _ = backtracer_core::resolve(
                if let Some(ref ctx) = unsafe { &DEBUG_CTX } {
                    Some(ctx)
                } else {
                    None
                },
                0,
                ip,
                |symbol| {
                    let name = symbol.name();
                    if let Some(addr) = symbol.addr() {
                        emerglogln!(
                            "{:4}: {:18p} - {}",
                            frame_nr,
                            addr,
                            if let Some(ref name) = name {
                                name
                            } else {
                                "??"
                            }
                        )
                    } else {
                        emerglogln!(
                            "{:4}:                 ?? - {}",
                            frame_nr,
                            if let Some(ref name) = name {
                                name
                            } else {
                                "??"
                            }
                        )
                    }
                    if let Some(filename) = symbol.filename() {
                        if let Some(linenr) = symbol.lineno() {
                            emerglogln!(
                                "                               at {}:{}",
                                filename,
                                linenr
                            );
                        }
                    }
                },
            );
        }
        frame_nr += 1;

        if frame_nr > MAX_FRAMES {
            return false;
        }

        true // keep going to the next frame
    };

    if let Some(entry_point) = entry_point {
        backtracer_core::trace_from(entry_point, trace_callback);
    } else {
        backtracer_core::trace(trace_callback);
    }
}

#[cfg(feature = "std")]
static DID_PANIC: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
#[panic_handler]
#[cfg(feature = "std")]
fn panic(info: &PanicInfo) -> ! {
    disable();
    let second_panic = DID_PANIC.swap(true, core::sync::atomic::Ordering::SeqCst);
    if second_panic {
        loop {}
    }
    emerglogln!("[error] {}", info);
    if second_panic {
        emerglogln!("we've had one, yes, but what about second panic?");
    }

    emerglogln!("starting backtrace...");

    backtrace(!second_panic, None);

    emerglogln!("unrecoverable, halting processor.");

    if crate::is_test_mode() {
        emerglogln!("!!! TEST MODE PANIC -- RESETTING");
        //crate::arch::debug_shutdown(42);
    }

    loop {}
}
#[cfg(feature = "std")]
#[lang = "eh_personality"]
pub extern "C" fn rust_eh_personality() {}
