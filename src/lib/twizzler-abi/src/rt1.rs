extern "C" {
    fn std_runtime_start(env: *const *const i8);
}

#[repr(C)]
struct Phdr {
    ty: u64,
    flags: u64,
    off: u64,
    vaddr: u64,
    paddr: u64,
    filesz: u64,
    memsz: u64,
    align: u64,
}

fn process_phdrs(phdrs: &[Phdr]) {
    for _ph in phdrs {}
}

/* This is essentially just a hook to get us out of the arch-specific code before calling into std.
 * I don't know if we have a panic runtime yet, so I'm not going to try doing a catch panic kind of
 * deal. Instead, we expect the runtime start function to return an exit code, and we'll deal with
 * exiting the thread using that code.
 */
use crate::aux::AuxEntry;
use core::ptr;
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
    /* it's unsafe because it's an extern C function. */
    /* TODO: pass env and args */
    unsafe { std_runtime_start(&null_env as *const *const i8) };
    // TODO: exit value
    crate::syscall::sys_thread_exit(0, ptr::null_mut())
}
