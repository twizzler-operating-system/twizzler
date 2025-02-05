use p256::ecdsa::{
    signature::{SignerMut, Verifier},
    Signature, SigningKey, VerifyingKey as p256VerifyingKey,
};
use sha2::{Digest, Sha256};
use twizzler_abi::object::{ObjID, Protections};

use crate::{
    flags::{CapFlags, HashingAlgo, SigningScheme},
    CapError, Gates, GatesError, Revoc, VerifyingKey,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cap {
    pub target: ObjID,
    pub accessor: ObjID,
    pub protections: Protections,
    flags: CapFlags,
    gates: Gates,
    pub revocation: Revoc,
    ///NOTE: AS BYTES
    siglen: u16,
    sig: [u8; 1024],
}

impl Cap {
    /// creating a new capability, revoc specified in expiration data in ns from unix epoch
    pub fn new(
        target: ObjID,
        accessor: ObjID,
        prots: Protections,
        target_priv_key: [u8; 32], // with this key we can?
        revocation: Revoc,
        gates: Gates,
    ) -> Result<Self, CapError> {
        let flags = CapFlags::SHA256 | CapFlags::ECDSA; // set flags
        let siglen = 64_u16; // according to how p256 ecdsa signature work,

        let hash_arr = Cap::serialize(accessor, target, prots, flags, siglen, revocation, gates);

        let mut hasher = Sha256::new();
        hasher.update(hash_arr);
        let hash = hasher.finalize();

        // hash has been generated, time to do the signing
        let mut signing_key =
            SigningKey::from_slice(&target_priv_key).map_err(|_| CapError::InvalidPrivateKey)?;

        let signature: Signature = signing_key.sign(hash.as_slice());

        let mut sig_buf: [u8; 1024] = [0; 1024];

        // this line can panic if somehow siglen is > 1024
        sig_buf[0..siglen as usize].copy_from_slice(signature.to_bytes().as_slice());

        Ok(Cap {
            accessor,
            target,
            protections: prots,
            flags,
            siglen,
            sig: sig_buf,
            revocation,
            gates,
        })
    }

    /// verifies signature inside capability
    pub fn verify_sig(&self, verifying_key: VerifyingKey) -> Result<(), CapError> {
        let (hashing_algo, signing_scheme) = self.flags.parse()?;

        // i hate how unergonomic this is but i wanted to contain all the serialization to one
        // function and this is the best way i could think of
        let hash_arr = Cap::serialize(
            self.accessor,
            self.target,
            self.protections,
            self.flags,
            self.siglen,
            self.revocation,
            self.gates,
        );

        let hash = match hashing_algo {
            HashingAlgo::Sha256 => {
                let mut hasher = Sha256::new();
                hasher.update(hash_arr);
                hasher.finalize()
            }
        };

        // sanity check
        if signing_scheme != verifying_key.scheme {
            return Err(CapError::InvalidVerifyKey);
        }

        match signing_scheme {
            SigningScheme::Ecdsa => {
                let vkey = p256VerifyingKey::from_sec1_bytes(verifying_key.as_bytes())
                    .map_err(|_| CapError::InvalidVerifyKey)?;
                let sig = Signature::from_slice(&self.sig[0..self.siglen as usize])
                    .map_err(|_| CapError::CorruptedSignature)?;

                vkey.verify(hash.as_slice(), &sig)
                    //NOTE: does the kernel have logging capabilities to log this error?
                    .map_err(|_| CapError::InvalidSignature)
            }
        }
    }

    /// pass in proposed gates values, verifies that they fall within the range
    /// specified by this capability
    pub fn check_gate(&self, offset: u64, length: u64, align: u64) -> Result<(), GatesError> {
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
        if self.gates.offset < offset || offset > self.gates.offset + length {
            return Err(GatesError::OutsideBounds);
        }

        //NOTE: not completely sure this is how you check alignment.
        if self.gates.align != align {
            return Err(GatesError::Unaligned);
        }

        Ok(())
    }

    /// returns all contents other than sig as a buffer ready to hash
    fn serialize(
        accessor: ObjID,
        target: ObjID,
        prots: Protections,
        flags: CapFlags,
        siglen: u16,
        revocation: Revoc,
        gates: Gates,
    ) -> [u8; 79] {
        let mut hash_arr: [u8; 79] = [0; 79];

        hash_arr[0..16].copy_from_slice(&accessor.as_u128().to_le_bytes());
        hash_arr[16..32].copy_from_slice(&target.as_u128().to_le_bytes());
        hash_arr[32..36].copy_from_slice(&prots.bits().to_le_bytes());
        hash_arr[36] = flags.bits();
        hash_arr[37..39].copy_from_slice(&siglen.to_le_bytes());
        hash_arr[39..55].copy_from_slice(&revocation.serialize());
        hash_arr[55..63].copy_from_slice(&gates.offset.to_le_bytes());
        hash_arr[63..71].copy_from_slice(&gates.length.to_le_bytes());
        hash_arr[71..79].copy_from_slice(&gates.align.to_le_bytes());
        hash_arr
    }
}
