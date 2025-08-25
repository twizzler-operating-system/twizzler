use super::local::{get_lapic, LAPIC_ICRLO, LAPIC_ICRLO_ASSERT, LAPIC_ICRLO_STATUS_PEND};
use crate::{interrupt::Destination, processor};

const DEST_SHORT_NONE: u32 = 0;
const _DEST_SHORT_SELF: u32 = 1;
const DEST_SHORT_ALL: u32 = 2;
const DEST_SHORT_OTHERS: u32 = 3;

const LAPIC_ICRLO_DEST_SHORT_OFFSET: u32 = 18;

pub fn raw_send_ipi(dest: Destination, vector: u32) {
    let (dest_short, dest_val) = match dest {
        Destination::Single(id) => (DEST_SHORT_NONE, id),
        Destination::Bsp | Destination::LowestPriority => {
            (DEST_SHORT_NONE, processor::mp::current_processor().bsp_id())
        }
        Destination::All => (DEST_SHORT_ALL, 0xffffffff),
        Destination::AllButSelf => (DEST_SHORT_OTHERS, 0xffffffff),
    };
    unsafe {
        let apic = get_lapic();
        apic.write_icr(
            dest_val,
            vector | dest_short << LAPIC_ICRLO_DEST_SHORT_OFFSET,
        );
        // TODO: intel docs say this isn't needed for x2 mode. Confirm, and make conditional.
        while apic.read(LAPIC_ICRLO) & LAPIC_ICRLO_STATUS_PEND != 0 {
            core::arch::asm!("pause")
        }
    }
}

pub fn send_ipi(dest: Destination, vector: u32) {
    raw_send_ipi(dest, vector | LAPIC_ICRLO_ASSERT);
}
