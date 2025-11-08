// Virtio vendor specific PCI Capability
#[repr(C)]
pub struct VirtioPciCap {
    pub cap_vndr: u8,            /* Generic PCI field: PCI_CAP_ID_VNDR */
    pub cap_next: u8,            /* Generic PCI field: next ptr. */
    pub cap_len: u8,             /* Generic PCI field: capability length */
    pub cfg_type: VirtioCfgType, /* Identifies the structure. */
    pub bar: u8,                 /* Where to find it. */
    pub id: u8,                  /* Multiple capabilities of the same type */
    pub padding: [u8; 2],        /* Pad to full dword. */
    pub offset: u32,             /* Offset within bar. */
    pub length: u32,             /* Length of the structure, in bytes. */
}

#[derive(Copy, Clone)]
#[allow(dead_code)]
pub enum VirtioCfgType {
    CommonCfg = 1,
    NotifyCfg = 2,
    IsrCfg = 3,
    DeviceCfg = 4,
    PciCfg = 5,
    SharedMemoryCfg = 8,
    VendorCfg = 9,
}
#[repr(C)]
pub struct VirtioPciNotifyCap {
    pub virtio_pci_cap: VirtioPciCap,
    pub notify_off_multiplier: u32, /* Multiplier for queue_notify_off. */
}

#[repr(C)]
pub struct VirtioCommonCfg {
    pub device_feature_select: u32,
    pub device_feature: u32,
    pub driver_feature_select: u32,
    pub driver_feature: u32,
    pub config_msix_vector: u16,
    pub num_queues: u16,
    pub device_status: u8,
    pub config_generation: u8,

    pub queue_select: u16,
    pub queue_size: u16,
    pub queue_msix_vector: u16,
    pub queue_enable: u16,
    pub queue_notify_off: u16,
    pub queue_desc: u64,
    pub queue_driver: u64,
    pub queue_device: u64,
    pub queue_notify_data: u16,
    pub queue_reset: u16,
}

pub type VirtioIsrStatus = u8;

pub struct CfgLocation {
    pub bar: usize,
    pub offset: usize,
    pub length: usize,
}
