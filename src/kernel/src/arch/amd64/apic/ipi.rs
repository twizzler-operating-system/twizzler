use crate::{interrupt::Destination, processor};

use super::local::{read_lapic, write_lapic, LAPIC_ICRHI, LAPIC_ICRLO, LAPIC_ICRLO_STATUS_PEND};

pub fn send_ipi(dest: Destination, vector: u32) {
    let (dest_short, dest_val) = match dest {
        Destination::Single(id) => (0, id << 24),
        Destination::Bsp | Destination::LowestPriority => {
            (0, processor::current_processor().bsp_id() << 24)
        }
        Destination::All => (2, 0),
        Destination::AllButSelf => (3, 0),
    };
    unsafe {
        write_lapic(LAPIC_ICRHI, dest_val);
        write_lapic(LAPIC_ICRLO, vector | dest_short << 18);

        while read_lapic(LAPIC_ICRLO) & LAPIC_ICRLO_STATUS_PEND != 0 {
            core::arch::asm!("pause")
        }
    }
}
