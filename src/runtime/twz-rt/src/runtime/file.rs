use atomic::Atomic;
use dynlink::engines::twizzler;
use twizzler_runtime_api::RustFsRuntime;

use super::ReferenceRuntime;

use twizzler_abi::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_object_map,
        UnmapFlags,
    },
    thread::{ExecutionState, ThreadRepr},
};
use twizzler_runtime_api::{InternalHandleRefs, MapError, ObjectHandle, ObjectRuntime, JoinError};
use core::{intrinsics::size_of, ptr::NonNull};

use std::{collections::BTreeMap, sync::atomic::AtomicU32};
use std::boxed::Box;
use std::borrow::ToOwned;
use std::sync::Arc;
use std::sync::Mutex;
use atomic::Ordering;

use twizzler_abi::slot;
use twizzler_runtime_api::{OwnedFd, FsError, SeekFrom};

struct FileDesc {
    slot_id: u32,
    pos: u64,
    handle: ObjectHandle
}

#[repr(C)]
struct FileMetadata {
    magic: u64,
    size: u64,
    direct: [u128; 10]
}

const MAGIC_NUMBER: u64 = 0xBEEFDEAD;

static FD_SLOTS: Mutex<BTreeMap<u32, Arc<Mutex<FileDesc>>>> = Mutex::new(BTreeMap::new());
static FD_ID_COUNTER: AtomicU32 = AtomicU32::new(1);

fn get_fd_slots() -> &'static Mutex<BTreeMap<u32, Arc<Mutex<FileDesc>>>> {
    &FD_SLOTS
}

impl RustFsRuntime for ReferenceRuntime {
    fn open(&self, path: &core::ffi::CStr) -> Result<OwnedFd, FsError> {
        let obj_id = ObjID::new(
            path
            .to_str()
            .map_err(|err| (FsError::InvalidPath))?
            .parse::<u128>()
            .map_err(|err| (FsError::InvalidPath))?
        );
        let flags = twizzler_runtime_api::MapFlags::READ | twizzler_runtime_api::MapFlags::WRITE;

        let handle = self.map_object(obj_id, flags).unwrap();

        let mut metadata_handle = unsafe{ &mut *handle.start.cast::<FileMetadata>() };
        if metadata_handle.magic != MAGIC_NUMBER {
            metadata_handle = &mut FileMetadata {
                magic: MAGIC_NUMBER,
                size: 0,
                direct: [0; 10],
            };
        }

        let fd = FD_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
        get_fd_slots()
            .lock()
            .unwrap()
            .insert(fd, Arc::new(Mutex::new(FileDesc {
                slot_id: 0,
                pos: 0,
                handle: handle
            })));
        
        Ok (OwnedFd{ 
            internal_fd: fd
        })
    }

    fn read(&self, fd: OwnedFd, buf: *mut u8, len: usize) -> Result<usize, FsError> {
        let binding = get_fd_slots()
            .lock()
            .unwrap();
        let mut file_desc = 
            binding
                .get(&fd.internal_fd)
                .ok_or(FsError::LookupError)?;

        let mut binding = file_desc.lock().unwrap();

        unsafe { buf.copy_from(binding.handle.start.offset(binding.pos as isize + size_of::<FileMetadata>() as isize), len) }

        binding.pos += len as u64;

        Ok(len)
    }

    fn write(&self, fd: OwnedFd, buf: *const u8, len: usize) -> Result<usize, FsError> {
        let binding = get_fd_slots()
            .lock()
            .unwrap();
        let file_desc = 
            binding
                .get(&fd.internal_fd)
                .ok_or(FsError::LookupError)?;
        
        let mut binding = file_desc.lock().unwrap();
            
        unsafe { 
            binding.handle.start
                .offset(binding.pos as isize + size_of::<FileMetadata>() as isize)
                .copy_from(buf, len) 
        }

        binding.pos += len as u64;

        Ok(len)
    }

    fn close(&self, fd: OwnedFd) -> Result<(), FsError> {
        let file_desc = 
            get_fd_slots()
                .lock()
                .unwrap()
                .remove(&fd.internal_fd)
                .ok_or(FsError::LookupError)?;

        let mut binding = file_desc
            .lock()
            .unwrap();
        
        self.release_handle(&mut binding.handle);
        
        Ok(())
    }

    fn seek(&self, fd: OwnedFd, pos: SeekFrom) -> Result<usize, FsError> {
        let binding = get_fd_slots()
            .lock()
            .unwrap();

        let file_desc = 
            binding
                .get(&fd.internal_fd)
                .ok_or(FsError::LookupError)?;
        
        let mut binding = file_desc.lock().unwrap();
        let mut metadata_handle = unsafe{ &mut *binding.handle.start.cast::<FileMetadata>() };

        let new_pos: i64 = match pos {
            SeekFrom::Start(x) => x as i64,
            SeekFrom::End(x) => (metadata_handle.size as i64) - x,
            SeekFrom::Current(x) => (binding.pos as i64) + x,
        };

        if new_pos > metadata_handle.size.try_into().unwrap() || new_pos < 0 {
            Err(FsError::LookupError)
        } else {
            binding.pos = new_pos as u64;
            Ok(binding.pos.try_into().unwrap()) 
        }
    }
}