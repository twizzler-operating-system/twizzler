use core::ptr::NonNull;

use acpi::AcpiTables;
use x86_64::PhysAddr;

use crate::once::Once;

use super::memory::phys_to_virt;

#[derive(Clone, Copy, Debug)]
pub struct AcpiHandlerImpl {}

impl acpi::AcpiHandler for AcpiHandlerImpl {
    unsafe fn map_physical_region<T>(
        &self,
        physical_address: usize,
        size: usize,
    ) -> acpi::PhysicalMapping<Self, T> {
        let virtual_address = phys_to_virt(PhysAddr::new(physical_address as u64));
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

static ACPI: Once<acpi::AcpiTables<AcpiHandlerImpl>> = Once::new();
static HANDLER: AcpiHandlerImpl = AcpiHandlerImpl {};

pub fn init(rsdp: u64) {
    ACPI.call_once(|| unsafe { acpi::AcpiTables::from_rsdp(HANDLER, rsdp as usize).unwrap() });
}

pub fn get_acpi_root() -> &'static AcpiTables<AcpiHandlerImpl> {
    ACPI.poll()
        .expect("need to call acpi::init before get_acpi_root")
}
