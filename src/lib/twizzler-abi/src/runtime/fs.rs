use twizzler_runtime_api::RustFsRuntime;

use super::MinimalRuntime;

use crate::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    runtime::{idcounter::IdCounter, simple_mutex::Mutex},
    rustc_alloc::collections::BTreeMap,
    rustc_alloc::boxed::Box,
    runtime::object::slot::global_allocate,
    syscall::{
        sys_object_map,
        UnmapFlags
    },
    thread::{ExecutionState, ThreadRepr},
};
use twizzler_runtime_api::{InternalHandleRefs, MapError, ObjectHandle, ObjectRuntime, JoinError};
use core::{intrinsics::size_of, ptr::NonNull};
use rustc_alloc::{borrow::ToOwned, sync::Arc};
use crate::runtime::object::slot;

use twizzler_runtime_api::{OwnedFd, FsError, SeekFrom};

struct FileDesc {
    slot_id: u32,
    pos: u64,
    handle: ObjectHandle
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FileMetadata {
    magic: u64,
    size: u64,
    direct: [u128; 10]
}

const MAGIC_NUMBER: u64 = 0xBEEFDEAD;

static FD_SLOTS: Mutex<BTreeMap<u32, Arc<Mutex<FileDesc>>>> = Mutex::new(BTreeMap::new());
static FD_ID_COUNTER: IdCounter = IdCounter::new_one();

fn get_fd_slots() -> &'static Mutex<BTreeMap<u32, Arc<Mutex<FileDesc>>>> {
    &FD_SLOTS
}

impl RustFsRuntime for MinimalRuntime {
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

        let mut metadata_handle = unsafe{ handle.start.offset(NULLPAGE_SIZE as isize).cast::<FileMetadata>() };
        if (unsafe { *metadata_handle }).magic != MAGIC_NUMBER {
            unsafe { *metadata_handle = FileMetadata {
                magic: MAGIC_NUMBER,
                size: 0,
                direct: [0; 10],
            } };
        }

        let fd = FD_ID_COUNTER.fresh();
        get_fd_slots()
            .lock()
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
            .lock();
        let mut file_desc = 
            binding
                .get(&fd.internal_fd)
                .ok_or(FsError::LookupError)?;

        let mut binding = file_desc.lock();

        unsafe { buf.copy_from(binding.handle.start.offset(NULLPAGE_SIZE as isize + binding.pos as isize + size_of::<FileMetadata>() as isize), len) }

        binding.pos += len as u64;

        Ok(len)
    }

    fn write(&self, fd: OwnedFd, buf: *const u8, len: usize) -> Result<usize, FsError> {
        let binding = get_fd_slots()
            .lock();
        let file_desc = 
            binding
                .get(&fd.internal_fd)
                .ok_or(FsError::LookupError)?;
        
        let mut binding = file_desc.lock();
            
        unsafe { 
            binding.handle.start
                .offset(NULLPAGE_SIZE as isize + binding.pos as isize + size_of::<FileMetadata>() as isize)
                .copy_from(buf, len) 
        }

        binding.pos += len as u64;

        Ok(len)
    }

    fn close(&self, fd: OwnedFd) -> Result<(), FsError> {
        let file_desc = 
            get_fd_slots()
                .lock()
                .remove(&fd.internal_fd)
                .ok_or(FsError::LookupError)?;

        let mut binding = file_desc.lock();
        
        self.release_handle(&mut binding.handle);
        
        Ok(())
    }

    fn seek(&self, fd: OwnedFd, pos: SeekFrom) -> Result<usize, FsError> {
        let binding = get_fd_slots()
            .lock();

        let file_desc = 
            binding
                .get(&fd.internal_fd)
                .ok_or(FsError::LookupError)?;
        
        let mut binding = file_desc.lock();
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

