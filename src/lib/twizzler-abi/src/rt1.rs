use crate::object::ObjID;

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
    align: usize,
}

static mut TLS_INFO: Option<TlsInfo> = None;
static mut EXEC_ID: ObjID = ObjID::new(0);
static mut PHDR_INFO: Option<&'static [Phdr]> = None;

// TODO: this is a hack
pub(crate) unsafe fn get_exec_id() -> Option<ObjID> {
    let id = EXEC_ID;
    if id == 0.into() {
        None
    } else {
        Some(id)
    }
}

// TODO: this is a hack
pub(crate) fn get_load_seg(nr: usize) -> Option<(usize, usize)> {
    if let Some(phdrs) = unsafe { PHDR_INFO } {
        if nr < phdrs.len() {
            Some((phdrs[nr].vaddr as usize, phdrs[nr].memsz as usize))
        } else {
            None
        }
    } else {
        None
    }
}

const MIN_TLS_ALIGN: usize = 16;
use core::alloc::Layout;
#[allow(named_asm_labels)]
fn init_tls() -> Option<u64> {
    unsafe {
        TLS_INFO.as_ref().map(|info| {
            let mut tls_size = info.memsz;
            tls_size += (((!tls_size) + 1) - (info.template_start as usize)) & (info.align - 1);
            let offset = tls_size;
            let tls_align = core::cmp::max(info.align, MIN_TLS_ALIGN);
            let full_tls_size =
                core::mem::size_of::<*const u8>() + tls_size + tls_align + MIN_TLS_ALIGN - 1
                    & ((!MIN_TLS_ALIGN) + 1);

            let layout = Layout::from_size_align(full_tls_size, MIN_TLS_ALIGN).unwrap(); //TODO
            let tls = crate::alloc::global_alloc(layout);
            /* TODO: oom */
            ptr::write_bytes(tls, 0x00, layout.size());
            let mem = tls.add(tls_size).sub((tls as usize) & (tls_align - 1));
            core::ptr::copy_nonoverlapping(info.template_start, mem.sub(offset), info.filsz);
            *(mem as *mut u64) = mem as u64;
            mem as u64
        })
    }
}

#[allow(unreachable_code)]
#[allow(unused_variables)]
#[allow(unused_mut)]
fn process_phdrs(phdrs: &'static [Phdr]) {
    for ph in phdrs {
        if ph.ty == 7 {
            unsafe {
                TLS_INFO = Some(TlsInfo {
                    template_start: ph.vaddr as *const u8,
                    memsz: ph.memsz as usize,
                    filsz: ph.filesz as usize,
                    align: ph.align as usize,
                })
            }
        }
    }
    unsafe {
        PHDR_INFO = Some(phdrs);
    }
}

/* This is essentially just a hook to get us out of the arch-specific code before calling into std.
 * I don't know if we have a panic runtime yet, so I'm not going to try doing a catch panic kind of
 * deal. Instead, we expect the runtime start function to return an exit code, and we'll deal with
 * exiting the thread using that code.
 */
use crate::aux::AuxEntry;
use core::ptr;
#[allow(named_asm_labels)]
#[allow(unreachable_code)]
#[allow(unused_variables)]
#[allow(unused_mut)]
pub(crate) extern "C" fn twz_runtime_start(mut aux_array: *const AuxEntry) -> ! {
    let null_env: [*const i8; 4] = [
        b"RUST_BACKTRACE=full\0".as_ptr() as *const i8,
        ptr::null(),
        ptr::null(),
        ptr::null(),
    ];
    unsafe {
        while !aux_array.is_null() && *aux_array != AuxEntry::Null {
            match *aux_array {
                AuxEntry::ProgramHeaders(paddr, pnum) => {
                    process_phdrs(core::slice::from_raw_parts(paddr as *const Phdr, pnum))
                }
                AuxEntry::ExecId(id) => {
                    EXEC_ID = id;
                }
                _ => {}
            }
            aux_array = aux_array.offset(1);
        }
    }
    let tls = init_tls();
    if let Some(tls) = tls {
        crate::syscall::sys_thread_settls(tls);
    }
    /* it's unsafe because it's an extern C function. */
    /* TODO: pass env and args */
    unsafe { std_runtime_start(&null_env as *const *const i8) };
    // TODO: exit value
    crate::syscall::sys_thread_exit(0, ptr::null_mut())
}
