#[cfg(feature = "log")]
use log::debug;
use twizzler_abi::object::{ObjID, Protections};

use crate::{
    flags::{HashingAlgo, SecFlags},
    Gate, Revoc, SecurityError, Signature, SigningKey, VerifyingKey,
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
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cap {
    /// Object ID this capability grants access to
    pub target: ObjID,

    /// Security context ID in which this capability resides
    pub accessor: ObjID,

    /// Specific access rights this capability grants
    pub prots: Protections,

    /// Cryptographic configuration for capability validation
    flags: SecFlags,

    /// Additional constraints on when this capability can be used
    gate: Gate,

    /// Specifies when this capability is invalid, i.e. expiration.
    pub revocation: Revoc,

    /// The signature inside the capability
    pub(super) sig: Signature,
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
        gates: Gate,
        hashing_algo: HashingAlgo,
    ) -> Result<Self, SecurityError> {
        let flags: SecFlags = hashing_algo.clone().into();

        #[cfg(feature = "log")]
        debug!(
            "Using flags: {} to create capability for target: {:?}",
            flags, target
        );

        let hash_arr = Cap::serialize(accessor, target, prots, flags, revocation, gates);

        let hash = hashing_algo.hash(&hash_arr);
        let sig = target_priv_key.sign(&hash)?;

        Ok(Cap {
            accessor,
            target,
            prots,
            flags,
            revocation,
            gate: gates,
            sig,
        })
    }

    /// verifies signature inside capability

    pub fn verify_sig(&self, verifying_key: &VerifyingKey) -> Result<(), SecurityError> {
        let hash_arr = self.into_array();

        let hash_algo: HashingAlgo = self.flags.try_into()?;

        let hash = hash_algo.hash(&hash_arr);
        verifying_key.verify(&hash, &self.sig)
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
        if ptr_offset < self.gate.offset {
            return Err(SecurityError::GateDenied);
        }

        // the access is beyond the "end" of the gate
        if self.gate.offset + self.gate.length < ptr_offset {
            return Err(SecurityError::GateDenied);
        }

        //NOTE: not completely sure this is how you check alignment.
        if self.gate.align != align {
            return Err(SecurityError::GateDenied);
        }

        Ok(())
    }

    /// Serializes this `Capability`
    pub(super) fn into_array(&self) -> [u8; CAP_SERIALIZED_LEN] {
        Self::serialize(
            self.accessor,
            self.target,
            self.prots,
            self.flags,
            self.revocation,
            self.gate,
        )
    }

    /// returns all contents other than sig as a buffer ready to hash
    fn serialize(
        accessor: ObjID,
        target: ObjID,
        prots: Protections,
        flags: SecFlags,
        revocation: Revoc,
        gate: Gate,
    ) -> [u8; CAP_SERIALIZED_LEN] {
        let mut hash_arr: [u8; CAP_SERIALIZED_LEN] = [0; CAP_SERIALIZED_LEN];
        hash_arr[0..16].copy_from_slice(&accessor.raw().to_le_bytes());
        hash_arr[16..32].copy_from_slice(&target.raw().to_le_bytes());
        hash_arr[32..34].copy_from_slice(&prots.bits().to_le_bytes());
        hash_arr[34..36].copy_from_slice(&flags.bits().to_le_bytes());
        hash_arr[36..52].copy_from_slice(&revocation.to_bytes());
        hash_arr[52..60].copy_from_slice(&gate.offset.to_le_bytes());
        hash_arr[60..68].copy_from_slice(&gate.length.to_le_bytes());
        hash_arr[68..76].copy_from_slice(&gate.align.to_le_bytes());
        hash_arr
    }
}

/// A builder for constructing a [`Cap`].
#[derive(Debug, Clone)]
pub struct CapBuilder {
    target: ObjID,
    accessor: ObjID,
    prots: Protections,
    revocation: Revoc,
    gates: Gate,
    hashing_algo: HashingAlgo,
}

impl CapBuilder {
    /// Create a new `CapBuilder` for a capability granting access to `target`, meant to live in
    /// the `accessor` security context.
    pub fn new(target: ObjID, accessor: ObjID) -> Self {
        Self {
            target,
            accessor,
            prots: Protections::empty(),
            revocation: Revoc::default(),
            gates: Gate::default(),
            hashing_algo: HashingAlgo::Sha256,
        }
    }

    /// The specific access rights this capability grants.
    pub fn protections(mut self, prots: Protections) -> Self {
        self.prots = prots;
        self
    }

    /// When this capability is invalid.
    pub fn revocation(mut self, revocation: Revoc) -> Self {
        self.revocation = revocation;
        self
    }

    /// Which reigion of the `target` object this capability is vaild for.
    pub fn gate(mut self, gate: Gate) -> Self {
        self.gates = gate;
        self
    }

    /// The hashing algorithm used to form the capability's signature.
    pub fn hashing_algo(mut self, hashing_algo: HashingAlgo) -> Self {
        self.hashing_algo = hashing_algo;
        self
    }

    /// Build and sign the `Cap` with `signing_key`.
    pub fn build(self, signing_key: &SigningKey) -> Result<Cap, SecurityError> {
        Cap::new(
            self.target,
            self.accessor,
            self.prots,
            signing_key,
            self.revocation,
            self.gates,
            self.hashing_algo,
        )
    }
}

#[cfg(test)]
#[cfg(feature = "user")]
#[allow(unused_imports)]
mod tests {

    use crate::*;

    extern crate test;
    use twizzler::object::TypedObject;
    use twizzler_abi::{object::Protections, syscall::ObjectCreate};
    /// Create a default capability
    fn default_capability(s_key: &SigningKey) -> Cap {
        Cap::new(
            0x123.into(),
            0x321.into(),
            Protections::all(),
            s_key,
            Revoc::default(),
            Gate::default(),
            HashingAlgo::Sha256,
        )
        .expect("Capability should have been created.")
    }

    #[test]
    fn test_capability_creation() {
        let (s, _v) = SigningKey::new_keypair(&SigningScheme::Ecdsa, ObjectCreate::default())
            .expect("keypair creation should not have errored!");
        let _cap = default_capability(s.base());
    }

    #[test]
    fn test_capability_verification() {
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
            capability_gates: Gate,
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
                    capability_gates: Gate::new(0, 100, 1),
                    ptr_offset: 3,
                    align: 1,
                },
                Pass,
            ),
            (
                Input {
                    capability_gates: Gate::new(0, 100, 1),
                    ptr_offset: 100,
                    align: 1,
                },
                Pass,
            ),
            (
                Input {
                    capability_gates: Gate::new(0, 10_000, 1),
                    ptr_offset: 5_000,
                    align: 1,
                },
                Pass,
            ),
            (
                Input {
                    capability_gates: Gate::new(0, 100, 1),
                    ptr_offset: 50,
                    align: 1,
                },
                Pass,
            ),
            (
                Input {
                    capability_gates: Gate::new(5, 10000, 1),
                    ptr_offset: 0, // ptr_offset too small
                    align: 1,
                },
                Fail,
            ),
            (
                Input {
                    capability_gates: Gate::new(0, 100, 1),
                    ptr_offset: 105, // ptr_offset too large
                    align: 1,
                },
                Fail,
            ),
            (
                Input {
                    capability_gates: Gate::new(0, 100, 1),
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
