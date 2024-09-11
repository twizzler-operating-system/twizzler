use super::local::{get_lapic, LAPIC_ICRHI, LAPIC_ICRLO, LAPIC_ICRLO_STATUS_PEND};
use crate::{interrupt::Destination, processor};

const DEST_SHORT_NONE: u32 = 0;
const _DEST_SHORT_SELF: u32 = 1;
const DEST_SHORT_ALL: u32 = 2;
const DEST_SHORT_OTHERS: u32 = 3;

const LAPIC_ICRHI_ID_OFFSET: u32 = 24;
const LAPIC_ICRLO_DEST_SHORT_OFFSET: u32 = 18;

pub fn send_ipi(dest: Destination, vector: u32) {
    let (dest_short, dest_val) = match dest {
        Destination::Single(id) => (DEST_SHORT_NONE, id << LAPIC_ICRHI_ID_OFFSET),
        Destination::Bsp | Destination::LowestPriority => (
            DEST_SHORT_NONE,
            processor::current_processor().bsp_id() << LAPIC_ICRHI_ID_OFFSET,
        ),
        Destination::All => (DEST_SHORT_ALL, 0),
        Destination::AllButSelf => (DEST_SHORT_OTHERS, 0),
    };
    unsafe {
        let apic = get_lapic();
        apic.write_icr(
            dest_val,
            vector | dest_short << LAPIC_ICRLO_DEST_SHORT_OFFSET,
        );
        while apic.read(LAPIC_ICRLO) & LAPIC_ICRLO_STATUS_PEND != 0 {
            core::arch::asm!("pause")
        }
    }
}
