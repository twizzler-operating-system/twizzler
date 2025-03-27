use core::ptr::NonNull;

use acpi::AcpiTables;

use super::memory::phys_to_virt;
use crate::{memory::PhysAddr, once::Once};

#[derive(Clone, Copy, Debug)]
pub struct AcpiHandlerImpl {}

impl acpi::AcpiHandler for AcpiHandlerImpl {
    unsafe fn map_physical_region<T>(
        &self,
        physical_address: usize,
        size: usize,
    ) -> acpi::PhysicalMapping<Self, T> {
        let phys_frame = physical_address & (!0xfff);
        let phys_off = physical_address % 0x1000;
        let virtual_address = phys_to_virt(PhysAddr::new(phys_frame as u64).unwrap())
            .offset(phys_off)
            .unwrap();

        acpi::PhysicalMapping::new(
            physical_address,
            NonNull::new(virtual_address.as_mut_ptr()).unwrap(),
            size,
            size,
            *self,
        )
    }

    fn unmap_physical_region<T>(_region: &acpi::PhysicalMapping<Self, T>) {}
}

struct AcpiWrap(acpi::AcpiTables<AcpiHandlerImpl>);

pub struct AcpiSliceOwner {}

// TODO: lock the ACPI tables, etc
unsafe impl Send for AcpiWrap {}
unsafe impl Sync for AcpiWrap {}

static ACPI: Once<AcpiWrap> = Once::new();
static HANDLER: AcpiHandlerImpl = AcpiHandlerImpl {};

pub fn init(rsdp: u64) {
    ACPI.call_once(|| {
        AcpiWrap(unsafe { acpi::AcpiTables::from_rsdp(HANDLER, rsdp as usize).unwrap() })
    });
}

pub fn get_acpi_root() -> &'static AcpiTables<AcpiHandlerImpl> {
    &ACPI
        .poll()
        .expect("need to call acpi::init before get_acpi_root")
        .0
}
