use super::local::{LAPIC_ICRLO, LAPIC_ICRLO_ASSERT, LAPIC_ICRLO_STATUS_PEND, get_lapic};
use crate::{interrupt::Destination, processor};

const DEST_SHORT_NONE: u32 = 0;
const _DEST_SHORT_SELF: u32 = 1;
const DEST_SHORT_ALL: u32 = 2;
const DEST_SHORT_OTHERS: u32 = 3;

const LAPIC_ICRLO_DEST_SHORT_OFFSET: u32 = 18;

/// A/B knob for skipping the delivery-status spin in x2APIC mode.
///
/// The spin is an xAPIC requirement: the ICR's Delivery Status bit says whether the last IPI has
/// been accepted, and software must not write the ICR again until it clears. **x2APIC removes that
/// bit** -- the ICR is a single 64-bit MSR and the `wrmsr` itself is what serializes, so there is
/// nothing to poll (Intel SDM Vol 3, x2APIC ICR: the delivery-status bit is reserved). The loop
/// therefore ran exactly once per send in x2APIC mode and read a reserved bit.
///
/// That one read is not free here. In x2APIC mode `Lapic::read` is an `mfence` plus a `rdmsr` of
/// 0x830, and under KVM an x2APIC MSR access traps -- so every IPI in the system paid a second vm
/// exit on top of the one the ICR write already costs. Every IPI: TLB shootdowns, thread wakeups,
/// preemption. Measured on `object_create_delete`, the IPI send inside a single unmap's shootdown
/// is 1,268 ns of an 8.1 us `sys_object_unmap`.
///
/// `false` restores the unconditional spin, which is the behaviour every measurement before this
/// was taken against.
pub const X2APIC_SKIP_ICR_WAIT: bool = false;

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
        if !X2APIC_SKIP_ICR_WAIT || !apic.is_x2() {
            while apic.read(LAPIC_ICRLO) & LAPIC_ICRLO_STATUS_PEND != 0 {
                core::arch::asm!("pause")
            }
        }
    }
}

pub fn send_ipi(dest: Destination, vector: u32) {
    raw_send_ipi(dest, vector | LAPIC_ICRLO_ASSERT);
}
