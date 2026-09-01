use twizzler_rt_abi::{error::TwzError, object::ObjID};

use super::{DevFs, Namespace, NsNode, ParentInfo, NSID_DEV};
use crate::Result;

/// Every device, in enumeration order. Adding a device is an entry here plus a dispatch arm in
/// the runtime's `open_path`.
const DEV_NODES: &[(&str, DevFs)] = &[
    ("null", DevFs::Null),
    ("zero", DevFs::Zero),
    ("urandom", DevFs::URandom),
    ("tty", DevFs::Tty),
    ("stdin", DevFs::Stdin),
    ("stdout", DevFs::Stdout),
    ("stderr", DevFs::Stderr),
];

/// `/dev`: a read-only namespace synthesized from [`DEV_NODES`], backed by nothing.
pub struct DevNamespace {
    parent_info: Option<ParentInfo>,
}

impl Namespace for DevNamespace {
    fn open(_id: ObjID, _persist: bool, parent_info: Option<ParentInfo>) -> Result<Self> {
        Ok(Self { parent_info })
    }

    fn find(&self, name: &str) -> Option<NsNode> {
        if name == "." {
            return NsNode::ns(".", NSID_DEV).ok();
        }
        DEV_NODES
            .iter()
            .find(|(n, _)| *n == name)
            .and_then(|(n, d)| NsNode::dev(n, *d).ok())
    }

    fn insert(&self, _node: NsNode) -> Result<()> {
        Err(TwzError::NOT_SUPPORTED)
    }

    fn replace(&self, _node: NsNode) -> Result<()> {
        Err(TwzError::NOT_SUPPORTED)
    }

    fn remove(&self, _name: &str) -> Result<NsNode> {
        Err(TwzError::NOT_SUPPORTED)
    }

    fn create_file(&self, _name: &str) -> Result<NsNode> {
        Err(TwzError::NOT_SUPPORTED)
    }

    // Overridden: the default `Ok(None)` would have `mkns` create a namespace object and only
    // then fail to bind it here, orphaning it.
    fn create_ns(&self, _name: &str) -> Result<Option<NsNode>> {
        Err(TwzError::NOT_SUPPORTED)
    }

    fn parent(&self) -> Option<&ParentInfo> {
        self.parent_info.as_ref()
    }

    fn id(&self) -> ObjID {
        NSID_DEV
    }

    fn items(&self, skip: usize, count: usize) -> Vec<NsNode> {
        DEV_NODES
            .iter()
            .skip(skip)
            .take(count)
            .filter_map(|(n, d)| NsNode::dev(n, *d).ok())
            .collect()
    }

    fn persist(&self) -> bool {
        false
    }
}
