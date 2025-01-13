use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::PathBuf,
};
use lazy_static::lazy_static;
use std::sync::Mutex;
use serde::{Deserialize, Serialize};
use tar::Header;

#[cfg(target_os = "twizzler")]
use naming::NamingHandle;
#[cfg(target_os = "twizzler")]
use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_thread_sync, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags, ThreadSync,
        ThreadSyncFlags, ThreadSyncReference, ThreadSyncWake,
    },
};
#[cfg(target_os = "twizzler")]
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};

// When the naming system gets integrated into the std::fs interface this will be removed
#[cfg(target_os = "twizzler")]
lazy_static! {
    static ref NAMER: Mutex<NamingHandle> = {
        Mutex::new(NamingHandle::new().unwrap())
    };
}

// This type indicates what type of object you want to create, with the name inside
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Copy)]
pub enum PackType {
    // Create an object that is compatible with the twizzler std::fs interface, or the unix one
    StdFile,
    // Create raw twizzler object
    TwzObj,
    // Create a persistent vector object,
    PVec,
}

pub struct Pack<T: std::io::Write> {
    tarchive: tar::Builder<T>,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct SpecialData {
    kind: PackType,
    offset: u64,
}

impl<W> Pack<W>
where
    W: std::io::Write,
{
    pub fn new(storage: W) -> Pack<W> {
        let mut tarchive = tar::Builder::new(storage);
        tarchive.mode(tar::HeaderMode::Deterministic);
        Pack { 
            tarchive: tarchive, 
        }
    }

    pub fn file_add(
        &mut self,
        path: PathBuf,
        pack_type: PackType,
        offset: u64,
    ) -> std::io::Result<()> {
        let old_path = &path;
        #[cfg(target_os = "twizzler")]
        let path = twizzler_name_create(&path.to_string_lossy()).to_string();
        let mut f = File::open(&path)?;
        let len = f.seek(SeekFrom::End(0))?;
        f.seek(SeekFrom::Start(0))?;
        let mut buf_writer = BufReader::new(f);
        let mut header = Header::new_old();
        {
            let data = bincode::serialize(&SpecialData {
                kind: pack_type,
                offset,
            })
            .unwrap();
            let custom_metadata = header.as_old_mut();
            custom_metadata.pad[0..data.len()].copy_from_slice(&data);
        }
        header.set_size(len);

        self.tarchive
            .append_data(&mut header, old_path, &mut buf_writer)?;

        Ok(())
    }

    pub fn stream_add<R: std::io::Read>(
        &mut self,
        stream: R,
        name: String,
        pack_type: PackType,
        offset: u64,
    ) -> std::io::Result<()> {
        let mut header = tar::Header::new_old();
        {
            let data = bincode::serialize(&SpecialData {
                kind: pack_type,
                offset,
            })
            .unwrap();
            let bad_idea = header.as_old_mut();
            bad_idea.pad[0..data.len()].copy_from_slice(&data);
        }
        let mut buf_writer = BufReader::new(stream);
        let mut v = vec![];
        buf_writer.read_to_end(&mut v)?;
        {
            self.tarchive.append_data(&mut header, name, v.as_slice())?;
        }
        Ok(())
    }

    pub fn build(mut self) {
        self.tarchive.finish().unwrap();
    }
}

#[cfg(target_os = "twizzler")]
pub fn create_twizzler_object() -> twizzler_object::ObjID {
    let create = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Persistent,
        None,
        ObjectCreateFlags::empty(),
    );
    let twzid = twizzler_abi::syscall::sys_object_create(create, &[], &[]).unwrap();

    twzid
}

#[cfg(target_os = "twizzler")]
pub fn twizzler_name_create(name: &str) -> u128 {
    let mut namer = NAMER.lock().unwrap();
    match namer.get(&name) {
        Some(id) => id,
        None => {
            let twzid = create_twizzler_object();
            namer.put(&name, twzid.as_u128());
            twzid.as_u128()
        },
    }
}

#[cfg(target_os = "twizzler")]
pub fn twizzler_name_get(name: &str) -> u128 {
    let mut namer = NAMER.lock().unwrap();
    namer.get(&name).unwrap()
}

#[cfg(target_os = "twizzler")]
pub fn form_twizzler_object<R: std::io::Read>(
    mut stream: R,
    name: String,
    offset: u64,
) -> std::io::Result<twizzler_object::ObjID> {
    let twzid = create_twizzler_object();
    let handle =
        twizzler_rt_abi::object::twz_rt_map_object(twzid, Protections::WRITE.into()).unwrap();
    let mut stream = BufReader::new(stream);

    let offset = std::cmp::max(offset, MAX_SIZE as u64) + NULLPAGE_SIZE as u64;
    let handle_data_ptr = unsafe { handle.start().offset(offset as isize) };
    let slice =
        unsafe { std::slice::from_raw_parts_mut(handle_data_ptr, MAX_SIZE - offset as usize) };

    stream.read(slice);

    Ok(twzid)
}

pub fn form_fs_file<R: std::io::Read>(stream: R, name: String, offset: u64) -> std::io::Result<()> {
    let mut writer = File::create(name)?;
    writer.seek(SeekFrom::Start(offset))?;
    let mut stream = BufReader::new(stream);
    io::copy(&mut stream, &mut writer)?;

    Ok(())
}

// this doesn't exist yet unfortunately due to persistent vector stuff
pub fn form_persistent_vector<R: std::io::Read>(
    stream: R,
    name: String,
    offset: u64,
) -> std::io::Result<()> {
    let mut writer = File::create(name)?;
    writer.seek(SeekFrom::Start(offset))?;
    let stream: Vec<String> = BufReader::new(stream)
        .split(b'\n')
        .filter_map(|result| result.ok())
        .filter_map(|line| String::from_utf8(line).ok())
        .collect();

    Ok(())
}

pub struct Unpack<T: std::io::Read> {
    tarchive: tar::Archive<T>,
}

impl<T> Unpack<T>
where
    T: std::io::Read,
{
    pub fn new(stream: T) -> std::io::Result<Unpack<T>> {
        Ok(Unpack {
            tarchive: tar::Archive::new(stream),
        })
    }

    pub fn unpack(mut self) -> std::io::Result<()> {
        for e in self.tarchive.entries().unwrap() {
            if let Ok(entry) = e {
                let path = entry
                    .path()
                    .unwrap()
                    .to_owned()
                    .into_owned()
                    .to_str()
                    .unwrap()
                    .to_owned();
                let bad_idea: SpecialData =
                    bincode::deserialize(&entry.header().as_old().pad).unwrap();
                
                println!("unpacked {}", path);
                #[cfg(target_os = "twizzler")]
                let path = twizzler_name_create(&path).to_string();
                match bad_idea.kind {
                    PackType::StdFile => {
                        form_fs_file(entry, path, bad_idea.offset)?;
                    }
                    PackType::TwzObj => {
                        #[cfg(target_os = "twizzler")]
                        form_twizzler_object(entry, path, bad_idea.offset);
                        #[cfg(not(target_os = "twizzler"))]
                        form_fs_file(entry, path, bad_idea.offset)?;
                    }
                    PackType::PVec => {
                        form_persistent_vector(entry, path, bad_idea.offset)?;
                    }
                }
            } else if let Err(E) = e {
                println!("{}", E);
            }
        }

        Ok(())
    }

    pub fn inspect<W: std::io::Write>(mut self, write_stream: &mut W) -> std::io::Result<()> {
        for e in self.tarchive.entries().unwrap() {
            if let Ok(entry) = e {
                let path = entry.path().unwrap().to_owned().into_owned();
                let bad_idea: SpecialData =
                    bincode::deserialize(&entry.header().as_old().pad).unwrap();
                write_stream.write(
                    format!(
                        "name: {:?}, type: {:?}, offset: {}\n",
                        path, bad_idea.kind, bad_idea.offset
                    )
                    .as_bytes(),
                )?;
                let mut read_stream = BufReader::new(entry);
                std::io::copy(&mut read_stream, write_stream)?;
            }
        }

        Ok(())
    }

    pub fn read<W: std::io::Write>(
        mut self,
        write_stream: &mut W,
        search: String,
    ) -> std::io::Result<()> {
        for e in self.tarchive.entries().unwrap() {
            if let Ok(entry) = e {
                let path = entry.path().unwrap().into_owned();
                let str_path = path.to_str().unwrap();
                if str_path == search {
                    let bad_idea: SpecialData =
                        bincode::deserialize(&entry.header().as_old().pad).unwrap();
                    write_stream.write(
                        format!(
                            "name: {:?}, type: {:?}, offset: {}",
                            path, bad_idea.kind, bad_idea.offset
                        )
                        .as_bytes(),
                    )?;
                    let mut read_stream = BufReader::new(entry);
                    std::io::copy(&mut read_stream, write_stream)?;
                }
            }
        }

        Ok(())
    }
}
