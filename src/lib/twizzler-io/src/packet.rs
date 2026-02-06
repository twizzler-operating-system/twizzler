use std::cell::UnsafeCell;

use bitset_core::BitSet;
use twizzler::{
    BaseType, Invariant,
    error::TwzError,
    object::{ObjID, Object, ObjectBuilder, RawObject, TypedObject},
};
use twizzler_abi::{object::NULLPAGE_SIZE, syscall::ObjectCreate};

pub const MAX_PACKET_BITS: usize = 1024;
pub const MIN_PACKET_SIZE: usize = 32;

#[derive(Invariant, BaseType)]
pub struct PacketBufferBase {
    nr_packets: usize,
    packet_size: usize,
    bitmap: UnsafeCell<[u8; MAX_PACKET_BITS]>,
}

impl PacketBufferBase {
    fn packet_mem_offset_from_base(&self) -> usize {
        (size_of::<PacketBufferBase>()).next_multiple_of(MIN_PACKET_SIZE.max(self.packet_size))
    }

    fn get_bitmap_mut(&self) -> &mut [u8; MAX_PACKET_BITS] {
        unsafe { self.bitmap.get().as_mut().unwrap() }
    }

    fn allocate_packet(&self) -> Option<usize> {
        let bm = self.get_bitmap_mut();
        for i in 0..bm.bit_len().min(self.nr_packets) {
            if !bm.bit_test(i) {
                bm.bit_set(i);
                return Some(i);
            }
        }
        None
    }

    fn release_packet(&self, packet: usize) {
        let bm = self.get_bitmap_mut();
        assert!(bm.bit_test(packet));
        bm.bit_reset(packet);
    }
}

pub struct PacketObject {
    obj: Object<PacketBufferBase>,
}

impl From<Object<PacketBufferBase>> for PacketObject {
    fn from(obj: Object<PacketBufferBase>) -> Self {
        Self { obj }
    }
}

impl PacketObject {
    pub fn id(&self) -> ObjID {
        self.obj.id()
    }

    pub fn new(
        spec: ObjectCreate,
        nr_packets: usize,
        packet_size: usize,
    ) -> Result<Self, TwzError> {
        Ok(Self::from(ObjectBuilder::new(spec).build(
            PacketBufferBase {
                nr_packets,
                packet_size,
                bitmap: UnsafeCell::new([0; _]),
            },
        )?))
    }

    pub fn packet_size(&self) -> usize {
        self.obj.base().packet_size.max(MIN_PACKET_SIZE)
    }

    pub fn packet_mem(&self, id: u32) -> &[u8] {
        let offset =
            self.obj.base().packet_mem_offset_from_base() + (id as usize * self.packet_size());
        let ptr = self
            .obj
            .lea(offset + NULLPAGE_SIZE, self.packet_size())
            .unwrap();
        unsafe { core::slice::from_raw_parts(ptr, self.packet_size()) }
    }

    pub fn packet_mem_mut(&self, id: u32) -> &mut [u8] {
        let offset =
            self.obj.base().packet_mem_offset_from_base() + (id as usize * self.packet_size());
        let ptr = self
            .obj
            .lea_mut(offset + NULLPAGE_SIZE, self.packet_size())
            .unwrap();
        unsafe { core::slice::from_raw_parts_mut(ptr, self.packet_size()) }
    }

    pub fn allocate_packet(&self) -> Option<u32> {
        self.obj
            .base()
            .allocate_packet()
            .map(|x| x.try_into().ok())
            .flatten()
    }

    pub fn release_packet(&self, id: u32) {
        if let Ok(id) = id.try_into() {
            self.obj.base().release_packet(id);
        }
    }
}
