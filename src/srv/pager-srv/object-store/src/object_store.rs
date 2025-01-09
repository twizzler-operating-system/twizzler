use std::io::{Error, Read, Write};

use fatfs::{DefaultTimeProvider, Dir, LossyOemCpConverter, Seek};

use crate::disk::{Disk, FS};
fn get_dir_path<'a>(
    fs: &'a mut fatfs::FileSystem<Disk>,
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

pub fn unlink_object(obj_id: u128) -> Result<(), Error> {
    let b64 = encode_obj_id(obj_id);
    let mut fs = FS.lock().unwrap();
    let subdir = get_dir_path(&mut fs, &b64)?;
    subdir.remove(&b64)?;
    Ok(())
}

/// Returns true if file was created and false if the file already existed.
pub fn create_object(obj_id: u128) -> Result<bool, Error> {
    let b64 = encode_obj_id(obj_id);
    let mut fs = FS.lock().unwrap();
    let subdir = get_dir_path(&mut fs, &b64)?;
    // try to open it to check if it exists.
    let res = subdir.open_file(&b64);
    match res {
        Ok(_) => Ok(false),
        Err(e) => match e {
            fatfs::Error::NotFound => {
                subdir.create_file(&b64);
                Ok(true)
            }
            _ => Err(e.into()),
        },
    }
}
pub fn read_exact(obj_id: u128, buf: &mut [u8], off: u64) -> Result<(), Error> {
    let b64 = encode_obj_id(obj_id);
    let mut fs = FS.lock().unwrap();
    let subdir = get_dir_path(&mut fs, &b64)?;
    let mut file = subdir.open_file(&b64)?;
    file.seek(fatfs::SeekFrom::Start(off))?;
    file.read_exact(buf)?;
    Ok(())
}

pub fn write_all(obj_id: u128, buf: &[u8], off: u64) -> Result<(), Error> {
    let b64 = encode_obj_id(obj_id);
    let mut fs = FS.lock().unwrap();
    let subdir = get_dir_path(&mut fs, &b64)?;
    let mut file = subdir.open_file(&b64)?;
    file.seek(fatfs::SeekFrom::Start(off))?;
    file.write_all(buf)?;
    Ok(())
}
