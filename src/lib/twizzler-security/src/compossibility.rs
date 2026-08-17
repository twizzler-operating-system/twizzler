use twizzler_abi::object::{ObjID, Protections};

/// I have no idea how to define this thing just yet. Lets just keep having vibes.
#[derive(Clone, Copy, Debug)]
pub struct Compossibility {
    // The protections any other capabilities / delegations in this context must adhere to.
    pub gcmask: Protections,

    /// The target that this `Compossibility` applies to.
    pub target: ObjID,

    /// Compossibility mask for the maximum protections the target object can have.
    /// Note that the target object does not get the `gcmask` applied to it.
    pub c_mask: Protections,
}

pub(crate) const COMPOSSIBILITY_SERIALIZED_LEN: usize = 20;

impl Compossibility {
    pub fn new(gcmask: Protections, target: ObjID, c_mask: Protections, del_offset: usize) -> Self {
        Self {
            gcmask,
            target,
            c_mask,
        }
    }

    /// returns all contents as a buffer ready to hash
    pub(crate) fn serialize(&self) -> [u8; COMPOSSIBILITY_SERIALIZED_LEN] {
        let mut hash_arr: [u8; COMPOSSIBILITY_SERIALIZED_LEN] = [0; COMPOSSIBILITY_SERIALIZED_LEN];
        hash_arr[0..2].copy_from_slice(&self.gcmask.bits().to_le_bytes());
        hash_arr[2..18].copy_from_slice(&self.target.raw().to_le_bytes());
        hash_arr[18..20].copy_from_slice(&self.c_mask.bits().to_le_bytes());
        hash_arr
    }
}
