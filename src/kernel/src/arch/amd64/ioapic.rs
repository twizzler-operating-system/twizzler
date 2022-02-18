use acpi::{madt::Madt, sdt::Signature, InterruptModel};
use alloc::vec::Vec;
use x86_64::PhysAddr;

use crate::{
    interrupt::{Destination, PinPolarity, TriggerMode},
    spinlock::Spinlock,
};

use super::{acpi::get_acpi_root, memory::phys_to_virt, processor::get_bsp_id};

struct IOApic {
    address: PhysAddr,
    gsi_base: u32,
    id: u8,
}

static IOAPICS: Spinlock<Vec<IOApic>> = Spinlock::new(alloc::vec![]);

impl IOApic {
    unsafe fn write(&self, reg: u32, val: u32) {
        let base: *mut u32 = phys_to_virt(self.address).as_mut_ptr();
        base.write_volatile(reg);
        base.add(4).write_volatile(val);
    }

    unsafe fn read(&self, reg: u32) -> u32 {
        let base: *mut u32 = phys_to_virt(self.address).as_mut_ptr();
        base.write_volatile(reg);
        base.add(4).read_volatile()
    }

    fn new(id: u8, address: PhysAddr, gsi_base: u32) -> Self {
        Self {
            id,
            address,
            gsi_base,
        }
    }

    unsafe fn write_vector_data(&self, regnum: u32, data: u64) {
        self.write(regnum * 2 + 0x10, 0x10000);
        self.write(regnum * 2 + 0x10 + 1, (data >> 32) as u32);
        self.write(regnum * 2 + 0x10, (data & 0xffffffff) as u32);
    }

    fn gsi_to_reg(&self, gsi: u32) -> Option<u32> {
        if gsi >= self.gsi_base && gsi < self.gsi_base + 24 {
            Some(gsi - self.gsi_base)
        } else {
            None
        }
    }
}

fn construct_interrupt_data(
    vector: u32,
    masked: bool,
    trigger: TriggerMode,
    polarity: PinPolarity,
    destination: Destination,
) -> u64 {
    let vector = vector as u64;
    let delmod = match destination {
        Destination::LowestPriority => 1,
        _ => 0,
    } << 8;
    let intpol = match polarity {
        PinPolarity::ActiveHigh => 0,
        PinPolarity::ActiveLow => 1,
    } << 13;
    let inttrg = match trigger {
        TriggerMode::Edge => 0,
        TriggerMode::Level => 1,
    } << 15;
    let mask = if masked { 1 } else { 0 } << 16;
    let destfield: u64 = (match destination {
        Destination::Bsp => get_bsp_id(None),
        Destination::Single(id) => id,
        Destination::LowestPriority => 0,
        _ => panic!("unsupported destination mode {:?} for IOAPIC", destination),
    } as u64)
        << 56;

    vector | delmod | intpol | inttrg | mask | destfield
}

pub(super) fn set_interrupt(
    gsi: u32,
    vector: u32,
    masked: bool,
    trigger: TriggerMode,
    polarity: PinPolarity,
    destination: Destination,
) {
    let ioapics = IOAPICS.lock();
    for ioapic in &*ioapics {
        if let Some(reg) = ioapic.gsi_to_reg(gsi) {
            unsafe {
                logln!("setting {} {} masked={}", gsi, vector, masked);
                ioapic.write_vector_data(
                    reg,
                    construct_interrupt_data(vector, masked, trigger, polarity, destination),
                )
            }
        }
    }
}

pub fn init() {
    let acpi = get_acpi_root();

    let madt = unsafe {
        acpi.get_sdt::<Madt>(Signature::MADT)
            .expect("unable to get MADT ACPI table")
            .expect("unable to find MADT ACPI table")
            .virtual_start()
            .as_ref()
    };
    let model = madt.parse_interrupt_model();
    let model = if let InterruptModel::Apic(model) = model.unwrap().0 {
        model
    } else {
        unimplemented!("failed to parse model")
    };
    /* TODO: unsure if it's safe to skip this if the interrupt model reports not having the PICs */
    //if model.also_has_legacy_pics {
    disable_pic();
    //}
    if model.io_apics.is_empty() {
        unimplemented!("no IOAPIC found");
    }
    for ioapic in &model.io_apics {
        let ioapic = IOApic::new(
            ioapic.id,
            PhysAddr::new(ioapic.address as u64),
            ioapic.global_system_interrupt_base,
        );
        for i in 0..24 {
            unsafe {
                ioapic.write_vector_data(
                    i,
                    construct_interrupt_data(
                        32 + i + ioapic.gsi_base,
                        true,
                        TriggerMode::Edge,
                        PinPolarity::ActiveHigh,
                        Destination::Bsp,
                    ),
                );
            }
        }
        IOAPICS.lock().push(ioapic);
    }

    for iso in &model.interrupt_source_overrides {
        // TODO: verify these mappings
        let trigger = match iso.trigger_mode {
            acpi::platform::interrupt::TriggerMode::SameAsBus => TriggerMode::Edge,
            acpi::platform::interrupt::TriggerMode::Edge => TriggerMode::Edge,
            acpi::platform::interrupt::TriggerMode::Level => TriggerMode::Level,
        };
        let polarity = match iso.polarity {
            acpi::platform::interrupt::Polarity::SameAsBus => PinPolarity::ActiveHigh,
            acpi::platform::interrupt::Polarity::ActiveHigh => PinPolarity::ActiveHigh,
            acpi::platform::interrupt::Polarity::ActiveLow => PinPolarity::ActiveLow,
        };

        logln!(
            "remap {} {}",
            iso.global_system_interrupt,
            iso.isa_source + 32
        );
        set_interrupt(
            iso.global_system_interrupt,
            iso.isa_source as u32 + 32,
            false,
            trigger,
            polarity,
            Destination::Bsp,
        );
    }
}

const PIC1: u16 = 0x20;
const PIC2: u16 = 0xA0;
const PIC1_DATA: u16 = PIC1 + 1;
const PIC2_DATA: u16 = PIC2 + 1;
const PIC1_CMD: u16 = PIC1;
const PIC2_CMD: u16 = PIC2;
const ICW1_ICW4: u8 = 0x01;
const ICW1_INIT: u8 = 0x10;
const ICW4_8086: u8 = 0x01;
fn disable_pic() {
    unsafe fn iowait() {
        x86::io::outb(0x80, 0);
    }
    /* let's first set the PIC into a known state */
    unsafe {
        let mask1 = x86::io::inb(PIC1_DATA);
        let mask2 = x86::io::inb(PIC2_DATA);

        x86::io::outb(PIC1_CMD, ICW1_INIT | ICW1_ICW4);
        iowait();
        x86::io::outb(PIC2_CMD, ICW1_INIT | ICW1_ICW4);
        iowait();
        x86::io::outb(PIC1_DATA, 32);
        iowait();
        x86::io::outb(PIC2_DATA, 40);
        iowait();
        x86::io::outb(PIC1_DATA, 4);
        iowait();
        x86::io::outb(PIC2_DATA, 2);
        iowait();
        x86::io::outb(PIC1_DATA, ICW4_8086);
        iowait();
        x86::io::outb(PIC2_DATA, ICW4_8086);
        iowait();
        x86::io::outb(PIC1_DATA, mask1);
        iowait();
        x86::io::outb(PIC2_DATA, mask2);

        x86::io::outb(PIC2_DATA, 0xff);
        x86::io::outb(PIC1_DATA, 0xff);
    }
}
