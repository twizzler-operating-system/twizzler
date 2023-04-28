use crate::Error;
use hasher::Hasher;
use khf::Khf;
use kms::KeyManagementScheme;
use rand::{CryptoRng, RngCore};
use std::collections::HashMap;

const DEFAULT_MASTER_KHF_FANOUTS: &[u64] = &[4, 4, 4, 4];
const DEFAULT_INODE_KHF_FANOUTS: &[u64] = &[4, 4, 4, 4];

type ObjId = u128;
type BlkId = u64;
type Key<const N: usize> = [u8; N];

pub struct Lethe<R, H, const N: usize> {
    // TODO: decide where the master key goes.
    _master_key: Key<N>,
    master_khf: Khf<R, H, N>,
    object_khfs: HashMap<ObjId, Khf<R, H, N>>,
    rng: R,
}

impl<R, H, const N: usize> Lethe<R, H, N>
where
    R: RngCore + CryptoRng + Clone,
    H: Hasher<N>,
{
    pub fn new(mut rng: R) -> Self {
        Self {
            _master_key: Self::random_key(&mut rng),
            master_khf: Khf::new(rng.clone(), DEFAULT_MASTER_KHF_FANOUTS),
            object_khfs: HashMap::new(),
            rng,
        }
    }

    fn random_key(rng: &mut R) -> Key<N> {
        let mut key = [0; N];
        rng.fill_bytes(&mut key);
        key
    }

    fn load_object_khf(&mut self, objid: ObjId) -> Result<(), Error> {
        if self.object_khfs.contains_key(&objid) {
            return Ok(());
        }

        // TODO: generate/issue IO to load existing object KHF.
        self.object_khfs
            .insert(objid, Khf::new(self.rng.clone(), DEFAULT_INODE_KHF_FANOUTS));

        Ok(())
    }
}

impl<R, H, const N: usize> KeyManagementScheme for Lethe<R, H, N>
where
    R: RngCore + CryptoRng + Clone,
    H: Hasher<N>,
{
    type Key = Key<N>;
    type KeyId = (ObjId, BlkId);
    type Error = Error;

    fn derive(&mut self, (objid, blkid): Self::KeyId) -> Result<Self::Key, Self::Error> {
        self.load_object_khf(objid)?;
        Ok(self.object_khfs.get_mut(&objid).unwrap().derive(blkid)?)
    }

    // TODO: fix object id in KHF.
    fn update(&mut self, (objid, blkid): Self::KeyId) -> Result<Self::Key, Self::Error> {
        self.master_khf.update(objid as u64)?;
        self.load_object_khf(objid)?;
        Ok(self.object_khfs.get_mut(&objid).unwrap().update(blkid)?)
    }

    fn commit(&mut self) -> Vec<Self::KeyId> {
        let mut changes = vec![];

        for (objid, khf) in self.object_khfs.iter_mut() {
            changes.extend(khf.commit().into_iter().map(|blkid| (*objid, blkid)));
        }

        self.master_khf.commit();

        changes
    }
}
