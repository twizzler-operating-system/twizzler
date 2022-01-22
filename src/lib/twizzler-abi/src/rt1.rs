extern "C" {
    fn std_runtime_start(env: *const *const i8);
}

#[repr(C)]
struct Phdr {
    ty: u32,
    flags: u32,
    off: u64,
    vaddr: u64,
    paddr: u64,
    filesz: u64,
    memsz: u64,
    align: u64,
}

struct TlsInfo {
    template_start: *const u8,
    memsz: usize,
    filsz: usize,
}

static mut TLS_INFO: Option<TlsInfo> = None;

use core::alloc::Layout;
fn init_tls() -> Option<u64> {
    unsafe {
        TLS_INFO.as_ref().map(|info| {
            let tls_size = info.memsz + core::mem::size_of::<*const u8>() + 16;
            let layout = Layout::from_size_align(tls_size, 16).unwrap();
            let tls = crate::alloc::global_alloc(layout);
            if !tls.is_null() {
                ptr::write_bytes(tls, 0x00, layout.size());
            }
            /* TODO: oom */
            let tls = tls.add(16 - (info.memsz % 16));
            core::ptr::copy_nonoverlapping(info.template_start, tls, info.filsz);
            let tcb_base = tls as u64 + info.memsz as u64;
            *(tcb_base as *mut u64) = tcb_base;
            tcb_base
        })
    }
}

#[allow(unreachable_code)]
#[allow(unused_variables)]
#[allow(unused_mut)]
fn process_phdrs(phdrs: &[Phdr]) {
    for ph in phdrs {
        if ph.ty == 7 {
            unsafe {
                TLS_INFO = Some(TlsInfo {
                    template_start: ph.vaddr as *const u8,
                    memsz: ph.memsz as usize,
                    filsz: ph.filesz as usize,
                })
            }
        }
    }
}

/* This is essentially just a hook to get us out of the arch-specific code before calling into std.
 * I don't know if we have a panic runtime yet, so I'm not going to try doing a catch panic kind of
 * deal. Instead, we expect the runtime start function to return an exit code, and we'll deal with
 * exiting the thread using that code.
 */
use crate::aux::AuxEntry;
use core::ptr;
#[allow(unreachable_code)]
#[allow(unused_variables)]
#[allow(unused_mut)]
pub(crate) extern "C" fn twz_runtime_start(mut aux_array: *const AuxEntry) -> ! {
    let null_env: [*const i8; 4] = [ptr::null(), ptr::null(), ptr::null(), ptr::null()];
    unsafe {
        while !aux_array.is_null() && *aux_array != AuxEntry::Null {
            match *aux_array {
                AuxEntry::ProgramHeaders(paddr, pnum) => {
                    process_phdrs(core::slice::from_raw_parts(paddr as *const Phdr, pnum))
                }
                _ => {}
            }
            aux_array = aux_array.offset(1);
        }
    }
    let tls = init_tls();
    if let Some(_tls) = tls {
        unsafe {
            asm!("wrfsbase {}", in(reg) _tls);
        }
    }
    /* it's unsafe because it's an extern C function. */
    /* TODO: pass env and args */
    unsafe { std_runtime_start(&null_env as *const *const i8) };
    // TODO: exit value
    crate::syscall::sys_thread_exit(0, ptr::null_mut())
}
