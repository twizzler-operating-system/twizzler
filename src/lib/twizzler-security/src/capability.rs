// use ed25519_dalek::SIGNATURE_LENGTH;
use sha2::Digest;
use twizzler_abi::object::{ObjID, Protections};

use crate::{
    flags::{CapFlags, HashingAlgo, SigningScheme},
    Gates, GatesError, Revoc, SecError, Signature, SigningKey, VerifyingKey,
};

/// A capability that represents authorization for a [Security Context](`crate::sec_ctx::SecCtx`) to
/// access an object.
///
/// Capabilities are stored inside [`crate::sec_ctx::SecCtx`], and are authenticated
/// using cryptographic signatures. When accessing an object for the first time,
/// the kernel searches through the attached [Security Context](`crate::sec_ctx::SecCtx`) for
/// a usable capability. If none found it will look through inactive contexts for a valid
/// capability and then procedes to verify its signature in order to grant access rights.
///
///
/// # Fields
///
/// * `target` - The object ID this capability grants access to
/// * `accessor` - The security context ID in which this capability resides
/// * `protections` - The specific access rights this capability grants
/// * `flags` - Specifies the cryptographic primitives used to form the signature
/// * `gates` - Allows access into an object in a specified range
/// * `revocation` - Specifies when the capability is invalid
/// * `signature` - the signature of the capability
///
/// # Examples
///
/// ```
/// // Example of creating and using a capability
/// todo
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cap {
    /// Object ID this capability grants access to
    pub target: ObjID,

    /// Security context ID in which this capability resides
    pub accessor: ObjID,

    /// Specific access rights this capability grants
    pub protections: Protections,

    /// Cryptographic configuration for capability validation
    flags: CapFlags,

    /// Additional constraints on when this capability can be used
    gates: Gates,

    /// Specifies when this capability is invalid, i.e. expiration.
    pub revocation: Revoc,

    /// The signature inside the capability
    sig: Signature,
}

const CAP_SERIALIZED_LEN: usize = 78;

impl Cap {
    /// creating a new capability, revoc specified in expiration data in ns from unix epoch
    pub fn new(
        target: ObjID,
        accessor: ObjID,
        prots: Protections,
        target_priv_key: SigningKey,
        revocation: Revoc,
        gates: Gates,
        hashing_algo: HashingAlgo,
        signing_scheme: SigningScheme,
    ) -> Result<Self, SecError> {
        let flags = CapFlags::Blake3 | CapFlags::Ed25519; // set flags

        let hash_arr = Cap::serialize(accessor, target, prots, flags, revocation, gates);

        let hash = blake3::hash(&hash_arr);

        let sig = target_priv_key.sign(hash.as_bytes())?;

        Ok(Cap {
            accessor,
            target,
            protections: prots,
            flags,
            revocation,
            gates,
            sig,
        })
    }

    /// verifies signature inside capability
    pub fn verify_sig(&self, verifying_key: VerifyingKey) -> Result<(), SecError> {
        let hash_arr = Self::serialize(
            self.accessor,
            self.target,
            self.protections,
            self.flags,
            self.revocation,
            self.gates,
        );

        let hash_algo: HashingAlgo = self.flags.try_into()?;

        match hash_algo {
            HashingAlgo::Blake3 => {
                let bind = blake3::hash(&hash_arr);
                let bind = bind.as_bytes();
                verifying_key.verify(bind.as_slice(), &self.sig)
            }
            HashingAlgo::Sha256 => {
                let mut hasher = sha2::Sha256::new();
                hasher.update(&hash_arr);
                let result = hasher.finalize();
                verifying_key.verify(result.as_slice(), &self.sig)
            }
        }
    }

    /// pass in proposed gates values, verifies that they fall within the range
    /// specified by this capability
    pub fn check_gate(&self, offset: u64, length: u64, align: u64) -> Result<(), SecError> {
        // the offset and length fields specify a region within the object. when the kernel switches
        // a threads active context in addition to the validity checks described in sec 3.1,
        // it checks to see if the instruction pointer is in a valid gate for the object it points
        // to. The instruction pointer must reside within the region specified by offset and
        // length and must be aligned on a value specified by align.

        //  assuming the layout is something like
        // ||||||||||||||||||||||||||||||||||||||||||||||||||||
        // offset |                                       | length
        //        {                                       }
        // the proposed offset must lay in this region

        //TODO: this needs to be fixed so that any 'chunk' inside of the reigion is valid too
        // if self.gates.offset < offset || offset > self.gates.offset + length {
        //     return Err(SecError::OutsideBounds);
        // }

        //TODO: make sure this is correct
        if !(offset + length < self.gates.length && offset > self.gates.offset) {
            return Err(SecError::OutsideBounds);
        }

        //NOTE: not completely sure this is how you check alignment.
        if self.gates.align != align {
            return Err(SecError::Unaligned);
        }

        Ok(())
    }

    /// returns all contents other than sig as a buffer ready to hash
    fn serialize(
        accessor: ObjID,
        target: ObjID,
        prots: Protections,
        flags: CapFlags,
        revocation: Revoc,
        gates: Gates,
    ) -> [u8; CAP_SERIALIZED_LEN] {
        let mut hash_arr: [u8; CAP_SERIALIZED_LEN] = [0; CAP_SERIALIZED_LEN];

        hash_arr[0..16].copy_from_slice(&accessor.raw().to_le_bytes());
        hash_arr[16..32].copy_from_slice(&target.raw().to_le_bytes());
        hash_arr[32..36].copy_from_slice(&prots.bits().to_le_bytes());
        hash_arr[36..38].copy_from_slice(&flags.bits().to_le_bytes());
        hash_arr[38..54].copy_from_slice(&revocation.to_bytes());
        hash_arr[54..62].copy_from_slice(&gates.offset.to_le_bytes());
        hash_arr[62..70].copy_from_slice(&gates.length.to_le_bytes());
        hash_arr[70..78].copy_from_slice(&gates.align.to_le_bytes());
        hash_arr
    }
}
