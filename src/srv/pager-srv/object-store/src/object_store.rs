use std::{
    collections::HashSet,
    io::Error,
    sync::{LazyLock, Mutex, MutexGuard},
};

use chacha20::{
    cipher::{KeyIvInit, StreamCipher, StreamCipherSeek},
    ChaCha20,
};
use obliviate_core::{
    crypter::{aes::Aes256Ctr, ivs::SequentialIvg},
    hasher::sha3::{Sha3_256, SHA3_256_MD_SIZE},
    kms::{
        khf::Khf, KeyManagementScheme, PersistableKeyManagementScheme, StableKeyManagementScheme,
    },
    wal::SecureWAL,
};
use rand::rngs::OsRng;

pub type MyKhf = Khf<OsRng, SequentialIvg, Aes256Ctr, Sha3_256, SHA3_256_MD_SIZE>;
type MyWal = SecureWAL<
    'static,
    crate::disk::Disk,
    <MyKhf as KeyManagementScheme>::LogEntry,
    SequentialIvg,
    Aes256Ctr,
    SHA3_256_MD_SIZE,
>;

pub fn open_khf() -> MyKhf {
    let fs = FS.get().unwrap().lock().unwrap();
    // let file = fs.root_dir().create_file("lethe/khf");
    let khf = MyKhf::load(ROOT_KEY, "lethe/khf", &fs).unwrap_or_else(|_e| MyKhf::new());
    khf
}

pub static KHF: LazyLock<Mutex<MyKhf>> = LazyLock::new(|| Mutex::new(open_khf()));
// FIXME should use a randomly generated root key for each device.
pub const ROOT_KEY: [u8; 32] = [0; 32];

fn open_wal() -> MyWal {
    FS.get()
        .unwrap()
        .lock()
        .unwrap()
        .root_dir()
        .create_dir("lethe")
        .unwrap();
    SecureWAL::open("lethe/wal".to_string(), ROOT_KEY, &FS.get().unwrap()).unwrap()
}

static WAL: LazyLock<Mutex<MyWal>> = LazyLock::new(|| Mutex::new(open_wal()));

/// To avoid dealing with race conditions I lock every external function call
/// at the entrance of the function.
static LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

use async_executor::Executor;
use fatfs::{
    DefaultTimeProvider, Dir, LossyOemCpConverter, Read as _, ReadWriteProxy, Seek, SeekFrom,
    Write as _,
};

// use obliviate_core::kms::khf::Khf;
use crate::{
    disk::DISK,
    fs::{self, PAGE_SIZE},
};
use crate::{
    disk::{Disk, EXECUTOR, FS},
    wrapped_extent::WrappedExtent,
};

pub fn init(ex: &'static Executor<'static>) {
    crate::disk::init(ex);
}

fn get_dir_path<'a>(
    fs: &'a mut fatfs::FileSystem<Disk, DefaultTimeProvider, LossyOemCpConverter>,
    encoded_obj_id: &EncodedObjectId,
) -> Result<Dir<'a, Disk, DefaultTimeProvider, LossyOemCpConverter>, Error> {
    let subdir = fs
        .root_dir()
        .create_dir("ids")?
        .create_dir(&encoded_obj_id[0..1])?;
    Ok(subdir)
}

type EncodedObjectId = String;

fn encode_obj_id(obj_id: u128) -> EncodedObjectId {
    format!("{:0>32x}", obj_id)
}

fn get_khf_locks<'a>() -> (MutexGuard<'a, MyKhf>, MutexGuard<'a, MyWal>) {
    let khf = KHF.lock().unwrap();
    let w = WAL.lock().unwrap();
    (khf, w)
}

/// Overwrites the existing disk with a new format.
///
/// WARNING: might not securely delete what used to be on the disk.
///
/// WARNING: is unlikely but it might panic
pub fn format() {
    let _unused = LOCK.lock();
    let mut disk = DISK.get().unwrap().clone();
    fs::format(&mut disk);
    drop(disk);
    let fs = FS.get().unwrap().lock().unwrap();
    crate::disk::init(EXECUTOR.get().unwrap());
    drop(fs);
    let mut khf = KHF.lock().unwrap();
    *khf = open_khf();
    drop(khf);
    let mut wal = WAL.lock().unwrap();
    *wal = open_wal();
    drop(wal);
}

/// Returns the disk length of a given object on disk.
pub fn disk_length(obj_id: u128) -> Result<u64, Error> {
    let _unused = LOCK.lock().unwrap();
    let mut fs = FS.get().unwrap().lock().unwrap();
    let id = encode_obj_id(obj_id);
    let dir = get_dir_path(&mut fs, &id)?;
    let mut file = dir.open_file(&id)?;
    let len = file.seek(SeekFrom::End(0))?;
    Ok(len)
}

/// Either gets a previously set config_id from disk or returns None
pub fn get_config_id() -> Result<Option<u128>, Error> {
    let _unused = LOCK.lock().unwrap();
    let fs = FS.get().unwrap().lock().unwrap();
    let file = fs.root_dir().open_file("config_id");
    let mut file = match file {
        Ok(file) => file,
        Err(fatfs::Error::NotFound) => return Ok(None),
        err => err?,
    };
    let mut buf = [0u8; 16];
    file.read_exact(&mut buf)?;
    Ok(Some(u128::from_le_bytes(buf)))
}

pub fn set_config_id(id: u128) -> Result<(), Error> {
    let _unused = LOCK.lock().unwrap();
    let fs = FS.get().unwrap().lock().unwrap();
    let mut file = fs.root_dir().create_file("config_id")?;
    file.truncate()?;
    let bytes = id.to_le_bytes();
    file.write_all(&bytes)?;
    Ok(())
}

pub fn clear_config_id() -> Result<(), Error> {
    let _unused = LOCK.lock().unwrap();
    let fs = FS.get().unwrap().lock().unwrap();
    let _file = fs.root_dir().remove("config_id")?;
    Ok(())
}

/// Returns true if file was created and false if the file already existed.
pub fn create_object(obj_id: u128) -> Result<bool, Error> {
    let _unused = LOCK.lock().unwrap();
    let b64 = encode_obj_id(obj_id);
    let mut fs = FS.get().unwrap().lock().unwrap();
    let subdir = get_dir_path(&mut fs, &b64)?;
    // try to open it to check if it exists.
    let res = subdir.open_file(&b64);
    match res {
        Ok(_) => Ok(false),
        Err(e) => match e {
            fatfs::Error::NotFound => {
                // khf.derive_mut(&wal, hash_obj_id(obj_id))
                //     .expect("shouldn't panic since khf implementation doesn't panic");
                subdir.create_file(&b64)?;
                Ok(true)
            }
            _ => Err(e.into()),
        },
    }
}

pub fn unlink_object(obj_id: u128) -> Result<(), Error> {
    let _unused = LOCK.lock().unwrap();
    let b64 = encode_obj_id(obj_id);
    let (mut khf, wal) = get_khf_locks();
    // khf.delete(&wal, hash_obj_id(obj_id))
    //     .map_err(Error::other)?;
    let mut fs = FS.get().unwrap().lock().unwrap();
    let subdir = get_dir_path(&mut fs, &b64)?;
    let mut file = subdir.open_file(&b64)?;
    for extent in file.extents() {
        let id = extent?.offset / PAGE_SIZE as u64;
        khf.delete(&wal, id).map_err(Error::other)?;
    }
    subdir.remove(&b64)?;
    Ok(())
}

pub fn get_all_object_ids() -> Result<Vec<u128>, Error> {
    let _unused = LOCK.lock().unwrap();
    let fs = FS.get().unwrap().lock().unwrap();
    let id_root = fs.root_dir().create_dir("ids")?;
    let mut out = Vec::new();
    for folder in id_root.iter() {
        let folder = folder?;
        for file in folder.to_dir().iter() {
            let file = file?;
            let name = file.file_name();
            if name.len() != 32 {
                continue; // ., ..
            }
            let id = u128::from_str_radix(&name, 16);
            if let Ok(id) = id {
                out.push(id);
            }
        }
    }
    Ok(out)
}
fn get_symmetric_cipher(disk_offset: u64) -> Result<ChaCha20, Error> {
    twizzler_abi::klog_println!("0");
    let (mut khf, wal) = get_khf_locks();
    twizzler_abi::klog_println!("1");
    let chunk_id = disk_offset / (PAGE_SIZE as u64);
    let key = khf.derive_mut(&wal, chunk_id).map_err(Error::other)?;
    get_symmetric_cipher_from_key(disk_offset, key)
}

fn get_symmetric_cipher_from_key(disk_offset: u64, key: [u8; 32]) -> Result<ChaCha20, Error> {
    let chunk_id = disk_offset / (PAGE_SIZE as u64);
    let offset = disk_offset % (PAGE_SIZE as u64);
    let bytes = chunk_id.to_le_bytes();
    let nonce: [u8; 12] = [
        0, 0, 0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ];

    let mut cipher = ChaCha20::new(&key.into(), &nonce.into());
    cipher.seek(offset);
    Ok(cipher)
}

pub fn read_exact(obj_id: u128, buf: &mut [u8], off: u64) -> Result<(), Error> {
    let _unused = LOCK.lock().unwrap();

    {
        // ensure these init now.
        let (_x, _y) = std::hint::black_box(get_khf_locks());
    }
    let b64 = encode_obj_id(obj_id);
    let mut fs = FS.get().unwrap().lock().unwrap();
    let subdir = get_dir_path(&mut fs, &b64)?;
    let mut file = subdir.open_file(&b64)?;
    file.seek(fatfs::SeekFrom::Start(off))?;
    let mut rw_proxy = ReadWriteProxy::new(
        &mut file,
        |disk: &mut Disk,
         disk_offset: u64,
         buffer: &mut [u8]|
         -> Result<usize, fatfs::Error<Error>> {
            twizzler_abi::klog_println!("A");
            let out = disk.read(buffer)?;
            twizzler_abi::klog_println!("B");
            let mut cipher = get_symmetric_cipher(disk_offset).map_err(|e| Error::other(e))?;
            twizzler_abi::klog_println!("C");
            cipher.apply_keystream(buffer);
            twizzler_abi::klog_println!("D");
            Ok(out)
        },
        || {},
    );
    fatfs::Read::read_exact(&mut rw_proxy, buf)?;
    Ok(())
}

pub fn write_all(obj_id: u128, buf: &[u8], off: u64) -> Result<(), Error> {
    let _unused = LOCK.lock().unwrap();
    let b64 = encode_obj_id(obj_id);
    // call to get_khf_locks to make sure that khf is already initialized for
    // the later "get_symmetric_cipher" call
    let _ = get_khf_locks();
    let mut fs = FS.get().unwrap().lock().unwrap();
    let subdir = get_dir_path(&mut fs, &b64)?;
    let mut file = subdir.open_file(&b64)?;
    let _new_pos = file.seek(fatfs::SeekFrom::Start(off))?;
    let extents_before: HashSet<WrappedExtent> = file
        .extents()
        .map(|v| v.map(WrappedExtent::from))
        .try_collect()?;
    let mut rw_proxy = ReadWriteProxy::new(
        &mut file,
        || {},
        |disk: &mut Disk, offset: u64, buffer: &[u8]| -> Result<usize, fatfs::Error<Error>> {
            let mut cipher = get_symmetric_cipher(offset)?;
            let mut encrypted = vec![0u8; buffer.len()];
            cipher
                .apply_keystream_b2b(buffer, &mut encrypted)
                .map_err(|e| Error::other(e))?;
            let out = disk.write(&encrypted)?;
            Ok(out)
        },
    );
    fatfs::Write::write_all(&mut rw_proxy, buf)?;
    let extents_after: HashSet<WrappedExtent> = file
        .extents()
        .map(|v| v.map(WrappedExtent::from))
        .try_collect()?;
    // Should never add extents to a file after writing to a file.
    assert!(extents_before.difference(&extents_after).next() == None);
    Ok(())
}

pub fn advance_epoch() -> Result<(), Error> {
    let _unused = LOCK.lock().unwrap();
    let (mut khf, wal) = get_khf_locks();
    let updated_keys = khf.update(&wal).map_err(Error::other)?;
    drop((khf, wal));
    for (id, key) in updated_keys {
        let mut buf = vec![0; PAGE_SIZE];
        let mut disk = DISK.get().unwrap().clone();
        let disk_offset = id * super::fs::PAGE_SIZE as u64;
        disk.seek(SeekFrom::Start(disk_offset))?;
        disk.read_exact(buf.as_mut_slice())?;
        let mut cipher =
            get_symmetric_cipher_from_key(disk_offset, key).map_err(|e| Error::other(e))?;
        cipher.apply_keystream(&mut buf);
        disk.seek(SeekFrom::Start(disk_offset))?;
        let mut cipher = get_symmetric_cipher(disk_offset).map_err(|e| Error::other(e))?;
        cipher.apply_keystream(&mut buf);
        disk.write_all(&mut buf)?;
    }
    let (mut khf, wal) = get_khf_locks();
    let fs = FS.get().unwrap().lock().unwrap();
    fs.root_dir().create_dir("tmp/")?;
    khf.persist(ROOT_KEY, "tmp/khf", &fs)
        .map_err(Error::other)?;
    let lethe = fs.root_dir().create_dir("lethe/")?;
    // Should be atomic from here:
    let res = lethe.remove("khf");
    match res {
        Err(fatfs::Error::NotFound) => {}
        r => r?,
    };
    fs.root_dir().rename("tmp/khf", &lethe, "khf")?;
    let mut file = fs.root_dir().open_file("lethe/khf")?;
    let len_serialized = file.seek(fatfs::SeekFrom::End(0)).unwrap();
    assert!(len_serialized != 0);
    drop(file);
    // to here.
    // needs to drop lethe to let fs be dropped.
    drop(lethe);
    // needs to drop fs so that wal can clear the file off the directory.
    drop(fs);
    wal.clear().map_err(Error::other)?;
    Ok(())
}
