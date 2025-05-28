// use ed25519_dalek::SIGNATURE_LENGTH;
#[cfg(feature = "log")]
use log::debug;
use sha2::{Digest, Sha256};
use twizzler_abi::object::{ObjID, Protections};

use crate::{
    flags::{CapFlags, HashingAlgo},
    Gates, Revoc, SecurityError, Signature, SigningKey, VerifyingKey,
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
        target_priv_key: &SigningKey,
        revocation: Revoc,
        gates: Gates,
        hashing_algo: HashingAlgo,
    ) -> Result<Self, SecurityError> {
        let flags: CapFlags = hashing_algo.clone().into();

        #[cfg(feature = "log")]
        debug!(
            "Using flags: {} to create capability for target: {:?}",
            flags, target
        );

        let hash_arr = Cap::serialize(accessor, target, prots, flags, revocation, gates);

        let sig = match hashing_algo {
            HashingAlgo::Blake3 => {
                // unimplemented!("running into problems with blake3 compilation on aarch64");
                let hash = blake3::hash(&hash_arr);
                target_priv_key.sign(hash.as_bytes())?
            }
            HashingAlgo::Sha256 => {
                let mut hasher = Sha256::new();
                hasher.update(hash_arr);
                let hash = hasher.finalize();
                target_priv_key.sign(hash.as_slice())?
            }
        };

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

    pub fn verify_sig(&self, verifying_key: &VerifyingKey) -> Result<(), SecurityError> {
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
                // #[cfg(feature = "log")]
                // error!("running into problems with blake3 compilation on aarch64");
                // unimplemented!("running into problems with blake3 compilation on aarch64");
                let hash = blake3::hash(&hash_arr);
                let bind = hash.as_bytes();
                verifying_key.verify(bind.as_slice(), &self.sig)
            }
            HashingAlgo::Sha256 => {
                #[cfg(feature = "log")]
                debug!("Hashing via Sha256");
                let mut hasher = sha2::Sha256::new();
                hasher.update(&hash_arr);
                let result = hasher.finalize();
                verifying_key.verify(result.as_slice(), &self.sig)
            }
        }
    }

    /// checks to see if the specified ptr_offset falls in the capability's gate.
    pub fn check_gate(&self, ptr_offset: u64, align: u64) -> Result<(), SecurityError> {
        // The `offset` and `length` fields specify a region within the object. When the
        // kernel switches a thread's active context, in addition to the validity checks described
        // in section 3.x, it checks to see if the instruction pointer is in a valid gate
        // for the object it points to. The instruction pointer must reside within the
        // region specified by `offset` and `length`, and must be aligned on a value specified
        // by `align`. If either of these is not true, the kernel will not consider that security
        // context valid to switch to. Note that we can recover the original sematics where we did
        // not perform this check by setting `offset` and `length` to cover the entire object, and
        // `align` to 1.

        // the pointer is less than the actual offset
        if ptr_offset < self.gates.offset {
            return Err(SecurityError::GateDenied);
        }

        // the access is beyond the "end" of the gate
        if self.gates.offset + self.gates.length < ptr_offset {
            return Err(SecurityError::GateDenied);
        }

        //NOTE: not completely sure this is how you check alignment.
        if self.gates.align != align {
            return Err(SecurityError::GateDenied);
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
        hash_arr[32..34].copy_from_slice(&prots.bits().to_le_bytes());
        hash_arr[34..36].copy_from_slice(&flags.bits().to_le_bytes());
        hash_arr[36..52].copy_from_slice(&revocation.to_bytes());
        hash_arr[52..60].copy_from_slice(&gates.offset.to_le_bytes());
        hash_arr[60..68].copy_from_slice(&gates.length.to_le_bytes());
        hash_arr[68..76].copy_from_slice(&gates.align.to_le_bytes());
        hash_arr
    }
}

#[cfg(feature = "user")]
mod tests {

    use crate::*;

    extern crate test;

    use twizzler::object::TypedObject;
    use twizzler_abi::{object::Protections, syscall::ObjectCreate};
    fn default_capability(s_key: &SigningKey) -> Cap {
        Cap::new(
            0x123.into(),
            0x321.into(),
            Protections::all(),
            s_key,
            Revoc::default(),
            Gates::default(),
            HashingAlgo::Sha256,
        )
        .expect("Capability should have been created.")
    }

    #[test]
    // NOTE: would be nice to do table testing here
    fn test_capability_creation() {
        // just simple thang
        let (s, v) = SigningKey::new_keypair(&SigningScheme::Ecdsa, ObjectCreate::default())
            .expect("keypair creation should not have errored!");
        let cap = default_capability(s.base());
    }

    #[test]
    fn test_capability_verification() {
        // just simple thang
        let (s, v) = SigningKey::new_keypair(&SigningScheme::Ecdsa, ObjectCreate::default())
            .expect("keypair creation should not have errored!");

        let cap = default_capability(s.base());

        cap.verify_sig(v.base())
            .expect("capability should have been verified.")
    }

    #[test]
    fn test_capability_gates() {
        struct Input {
            /// gates that the capability will hold
            capability_gates: Gates,
            /// values you test
            ptr_offset: u64,
            align: u64,
        }

        // yeah i dont need an enum for this but honestly just makes it clear when im writing
        // the table / makes it clear when reading the table.
        #[derive(PartialEq, PartialOrd, Ord, Eq, Debug)]
        enum Expected {
            Fail,
            Pass,
        }

        use Expected::*;

        let table: [(Input, Expected); 7] = [
            (
                Input {
                    capability_gates: Gates::new(0, 100, 1),
                    ptr_offset: 3,
                    align: 1,
                },
                Pass,
            ),
            (
                Input {
                    capability_gates: Gates::new(0, 100, 1),
                    ptr_offset: 100,
                    align: 1,
                },
                Pass,
            ),
            (
                Input {
                    capability_gates: Gates::new(0, 10_000, 1),
                    ptr_offset: 5_000,
                    align: 1,
                },
                Pass,
            ),
            (
                Input {
                    capability_gates: Gates::new(0, 100, 1),
                    ptr_offset: 50,
                    align: 1,
                },
                Pass,
            ),
            (
                Input {
                    capability_gates: Gates::new(5, 10000, 1),
                    ptr_offset: 0, // ptr_offset too small
                    align: 1,
                },
                Fail,
            ),
            (
                Input {
                    capability_gates: Gates::new(0, 100, 1),
                    ptr_offset: 105, // ptr_offset too large
                    align: 1,
                },
                Fail,
            ),
            (
                Input {
                    capability_gates: Gates::new(0, 100, 1),
                    ptr_offset: 66,
                    align: 4, // bad alignment
                },
                Fail,
            ),
        ];

        let (s, _v) = SigningKey::new_keypair(&SigningScheme::Ecdsa, ObjectCreate::default())
            .expect("keypair creation should not have errored!");

        for (test_number, (input, expected)) in table.into_iter().enumerate() {
            let cap = Cap::new(
                0x123.into(),
                0x321.into(),
                Protections::all(),
                s.base(),
                Revoc::default(),
                input.capability_gates,
                HashingAlgo::Sha256,
            )
            .expect("Capability should have been created properly.");

            let actual = match cap.check_gate(input.ptr_offset, input.align).is_ok() {
                true => Pass,
                false => Fail,
            };

            assert_eq!(
                actual,
                expected,
                "
                 \n Test {:?}
                 expected: {:?}
                 actual: {:?},
                 Failed for capability gates = {:#?}, where
                 testing against: ptr_offset = {}, align = {})",
                test_number,
                expected,
                actual,
                input.capability_gates,
                input.ptr_offset,
                input.align
            )
        }
    }
}
