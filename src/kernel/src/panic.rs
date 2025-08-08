use alloc::{borrow::Cow, boxed::Box, vec::Vec};
use core::panic::PanicInfo;

use addr2line::{gimli::EndianSlice, Context};
use object::{read::elf::ElfFile64, Object, ObjectSection};

use crate::{interrupt::disable, once::Once};

type ElfSlice = addr2line::gimli::read::EndianSlice<'static, addr2line::gimli::RunTimeEndian>;

struct DebugCtx {
    ctx: Context<ElfSlice>,
}

unsafe impl Send for DebugCtx {}
unsafe impl Sync for DebugCtx {}

static DEBUG_CTX: Once<DebugCtx> = Once::new();

fn load_debug_context(
    file: &'static object::read::elf::ElfFile64,
) -> Option<addr2line::Context<ElfSlice>> {
    let endian = addr2line::gimli::RunTimeEndian::Little; //TODO
    fn load_section(
        id: addr2line::gimli::SectionId,
        file: &'static object::read::elf::ElfFile64,
        endian: addr2line::gimli::RunTimeEndian,
    ) -> Option<ElfSlice> {
        let data = file
            .section_by_name(id.name())
            .and_then(|section| {
                let data = section.uncompressed_data().ok()?;
                Some(match data {
                    Cow::Borrowed(data) => data,
                    Cow::Owned(data) => Box::leak(data.into_boxed_slice()),
                })
            })
            .unwrap_or_else(|| Box::leak(Vec::new().into_boxed_slice()));
        Some(EndianSlice::new(data, endian))
    }

    let result = addr2line::gimli::Dwarf::load(|id| load_section(id, file, endian).ok_or(()));
    match result {
        Ok(dwarf) => match addr2line::Context::from_dwarf(dwarf) {
            Ok(dwarf) => Some(dwarf),
            Err(e) => {
                logln!("loading debug information failed: {:?}", e);
                None
            }
        },
        Err(_) => {
            logln!("loading debug information failed");
            None
        }
    }
}

pub fn init(kernel_image: &'static [u8]) {
    static IMAGE: Once<ElfFile64> = Once::new();
    let image =
        object::read::elf::ElfFile64::parse(kernel_image).expect("failed to parse kernel image");
    let image = IMAGE.call_once(|| image);
    if let Some(ctx) = load_debug_context(&image) {
        DEBUG_CTX.call_once(|| DebugCtx { ctx });
    }
}

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
                if let Some(ctx) = DEBUG_CTX.poll().map(|d| &d.ctx) {
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

static DID_PANIC: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
#[panic_handler]
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

#[lang = "eh_personality"]
pub extern "C" fn rust_eh_personality() {}

pub fn is_panicing() -> bool {
    DID_PANIC.load(core::sync::atomic::Ordering::SeqCst)
}
