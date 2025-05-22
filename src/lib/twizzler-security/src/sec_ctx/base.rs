// security contexts may limit permissions by including mask entries
//
//
/*

MASK := {
    target: ObjID, // refers to an object whose permissions
                   // are being masked in this context using the `mask` field
    mask: BitField
    ovrmask : BitField // mask entries can also indicicate on
                       // a per-object basis which permissions are exempt
                       // forom the context's global mask.
    flags: BitField
}
*/

use twizzler::object::ObjID;
use twizzler_abi::object::Protections;

use super::map::SecCtxMap;

/// completely arbitrary amount of mask entries in a security context
const MASKS_MAX: usize = 15;

struct Mask {
    /// object whose permissions will be masked.
    target: ObjID,
    /// Specifies a mask on the permissions granted by capabilties and delegations in this security
    /// context.
    mask: Protections,
    /// an override mask on the context's global mask.
    ovrmask: Protections,
    // not sure what these flags are for?
    // flags: BitField
}

impl Mask {
    fn new(target: ObjID, mask: Protections, ovrmask: Protections) -> Self {
        Mask {
            target,
            mask,
            ovrmask,
        }
    }
}

// holds the masks
// ideally should be a `HashMap` but we dont have those yet so this will have to do...
struct MaskMap {
    buf: [Mask; MASKS_MAX],
    len: usize,
}

/// The base of a Security Context, holding a map to the capabilities and delegations stored inside,
/// masks on targets
#[derive(Debug)]
pub struct SecCtxBase {
    map: SecCtxMap,
    masks: [Mask; MASKS_MAX],
    global_mask: Protections,
}

impl Default for SecCtxBase {
    fn default() -> Self {
        let masks = [MASKS_MAX];
        Self {
            map: Default::default(),
            masks: [],

            global_mask: Protections::all,
        }
    }
}
