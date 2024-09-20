extern crate twizzler_abi;

const MAX_SIZE: u16 = 32768;

/* This marks a buffer as continuing via the next field. */
const VIRTQ_DESC_F_NEXT: u16 = 1;
/* This marks a buffer as device write-only (otherwise device read-only). */
const VIRTQ_DESC_F_WRITE: u16 = 2;
/* This means the buffer contains a list of buffer descriptors. */
const VIRTQ_DESC_F_INDIRECT: u16 = 4;
struct VirtqDesc {
    // Address (guest-physical).
    addr: u64,  
    // Length.
    len: u32,
    // The flags as indicated above.
    flags: u16,
    // Next field if flags & NEXT.
    next: u16,
}

struct VirqAvail {
    flags: u16,
    idx: u16,
    ring: [u16; MAX_SIZE as usize],
    used_event: u16,
}

struct VirtqUsedElem {
    id: u32,
    len: u32,
}

struct VirtqUsed {
    flags: u16,
    idx: u16,
    ring: [VirtqUsedElem; MAX_SIZE as usize],
    avail_event: u16,
}

struct Virtq {
    // The actual descriptors (16 bytes each)
    desc: vec<VirtqDesc>,
    avail: VirqAvail,
    used: VirtqUsed,
}

impl Virtq {
    fn new(size: usize) -> Virtq {
        Virtq {
            desc: [VirtqDesc {
                addr: 0,
                len: 0,
                flags: 0,
                next: 0,
            }; size],
            avail: VirqAvail {
                flags: 0,
                idx: 0,
                ring: [0; MAX_SIZE as usize],
                used_event: 0,
            },
            used: VirtqUsed {
                flags: 0,
                idx: 0,
                ring: [VirtqUsedElem { id: 0, len: 0 }; MAX_SIZE as usize],
                avail_event: 0,
            },
        }
    }
}

impl VirtqUsed {

}
