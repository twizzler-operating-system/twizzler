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

/// Upper bound on allocatable packet ids: `allocate_packet` scans the bitmap by bit.
pub const MAX_PACKETS: usize = MAX_PACKET_BITS * 8;

#[derive(Invariant, BaseType)]
pub struct PacketBufferBase {
    nr_packets: usize,
    packet_size: usize,
    bitmap: UnsafeCell<[u8; MAX_PACKET_BITS]>,
    /// Bytes of each slot that are real frame, written by the sender and read by the receiver.
    ///
    /// Without this the protocol carries no length at all: the receiving `RxToken` handed its
    /// closure the entire `packet_size` slot, and every consumer had to re-derive the length from
    /// the headers. That worked only because every frame in practice was IPv4 or ARP; anything
    /// whose length is not derivable that way had no length source, and net-srv forwarded whole
    /// 2048-byte slots to the NIC on a 1514-byte MTU as a result.
    ///
    /// Zero means "not set" and callers fall back to the full slot, so any path that writes packet
    /// memory without going through a `TxToken` keeps its old behaviour rather than truncating.
    lengths: UnsafeCell<[u16; MAX_PACKETS]>,
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
        self.get_lengths_mut()[packet] = 0;
    }

    fn get_lengths_mut(&self) -> &mut [u16; MAX_PACKETS] {
        unsafe { self.lengths.get().as_mut().unwrap() }
    }
}

#[derive(Clone)]
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

    pub fn object(&self) -> &Object<PacketBufferBase> {
        &self.obj
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
                lengths: UnsafeCell::new([0; _]),
            },
        )?))
    }

    pub fn packet_size(&self) -> usize {
        self.obj.base().packet_size.max(MIN_PACKET_SIZE)
    }

    /// Slots in this pool -- the count of network buffers behind one endpoint.
    pub fn nr_packets(&self) -> usize {
        self.obj.base().nr_packets
    }

    /// Record how much of slot `id` is real frame. See `PacketBufferBase::lengths`.
    pub fn set_packet_len(&self, id: u32, len: usize) {
        let base = self.obj.base();
        if (id as usize) < MAX_PACKETS {
            base.get_lengths_mut()[id as usize] = len.min(self.packet_size()) as u16;
        }
    }

    /// Real frame bytes in slot `id`, or the whole slot if the sender recorded nothing.
    pub fn packet_len(&self, id: u32) -> usize {
        let base = self.obj.base();
        if (id as usize) >= MAX_PACKETS {
            return self.packet_size();
        }
        match base.get_lengths_mut()[id as usize] {
            0 => self.packet_size(),
            n => (n as usize).min(self.packet_size()),
        }
    }

    pub fn packet_offset(&self, id: u32) -> usize {
        let offset =
            self.obj.base().packet_mem_offset_from_base() + (id as usize * self.packet_size());
        offset + NULLPAGE_SIZE
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
