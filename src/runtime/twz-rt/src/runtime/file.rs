use core::{intrinsics::size_of, ptr::NonNull};
use std::{
    borrow::ToOwned,
    boxed::Box,
    collections::BTreeMap,
    sync::{atomic::AtomicU32, Arc, Mutex},
};

use atomic::{Atomic, Ordering};
use dynlink::engines::twizzler;
use lazy_static::lazy_static;
use stable_vec::{self, StableVec};
use twizzler_abi::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    slot,
    syscall::{sys_object_map, UnmapFlags},
    thread::{ExecutionState, ThreadRepr},
};
use twizzler_runtime_api::{
    FsError, InternalHandleRefs, JoinError, MapError, ObjectHandle, ObjectRuntime, RawFd,
    RustFsRuntime, SeekFrom,
};

use super::ReferenceRuntime;

struct FileDesc {
    slot_id: u32,
    pos: u64,
    handle: ObjectHandle,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FileMetadata {
    magic: u64,
    size: u64,
    direct: [u128; 10],
}

const MAGIC_NUMBER: u64 = 0xBEEFDEAD;

lazy_static! {
    static ref FD_SLOTS: Mutex<StableVec<Arc<Mutex<FileDesc>>>> = Mutex::new(StableVec::new());
}

fn get_fd_slots() -> &'static Mutex<StableVec<Arc<Mutex<FileDesc>>>> {
    &FD_SLOTS
}

impl RustFsRuntime for ReferenceRuntime {
    fn open(&self, path: &core::ffi::CStr) -> Result<RawFd, FsError> {
        let obj_id = ObjID::new(
            path.to_str()
                .map_err(|err| (FsError::InvalidPath))?
                .parse::<u128>()
                .map_err(|err| (FsError::InvalidPath))?,
        );
        let flags = twizzler_runtime_api::MapFlags::READ | twizzler_runtime_api::MapFlags::WRITE;

        let handle = self.map_object(obj_id, flags).unwrap();

        let mut metadata_handle = unsafe {
            handle
                .start
                .offset(NULLPAGE_SIZE as isize)
                .cast::<FileMetadata>()
        };
        if (unsafe { *metadata_handle }).magic != MAGIC_NUMBER {
            unsafe {
                *metadata_handle = FileMetadata {
                    magic: MAGIC_NUMBER,
                    size: 0,
                    direct: [0; 10],
                }
            };
        }

        let fd = get_fd_slots()
            .lock()
            .unwrap()
            .push(Arc::new(Mutex::new(FileDesc {
                slot_id: 0,
                pos: 0,
                handle,
            })));

        Ok(RawFd(fd.try_into().unwrap()))
    }

    fn read(&self, fd: &RawFd, buf: &mut [u8]) -> Result<usize, FsError> {
        let binding = get_fd_slots().lock().unwrap();
        let mut file_desc = binding
            .get(fd.0.try_into().unwrap())
            .ok_or(FsError::LookupError)?;

        let mut binding = file_desc.lock().unwrap();

        unsafe {
            buf.as_mut_ptr().copy_from(
                binding.handle.start.offset(
                    NULLPAGE_SIZE as isize
                        + binding.pos as isize
                        + size_of::<FileMetadata>() as isize,
                ),
                buf.len(),
            )
        }

        binding.pos += buf.len() as u64;

        Ok(buf.len())
    }

    fn write(&self, fd: &RawFd, buf: &[u8]) -> Result<usize, FsError> {
        let binding = get_fd_slots().lock().unwrap();
        let file_desc = binding
            .get(fd.0.try_into().unwrap())
            .ok_or(FsError::LookupError)?;

        let mut binding = file_desc.lock().unwrap();

        unsafe {
            binding
                .handle
                .start
                .offset(
                    NULLPAGE_SIZE as isize
                        + binding.pos as isize
                        + size_of::<FileMetadata>() as isize,
                )
                .copy_from(buf.as_ptr(), buf.len())
        }

        binding.pos += buf.len() as u64;

        Ok(buf.len())
    }

    fn close(&self, fd: &mut RawFd) -> Result<(), FsError> {
        let file_desc = get_fd_slots()
            .lock()
            .unwrap()
            .remove(fd.0.try_into().unwrap())
            .ok_or(FsError::LookupError)?;

        let mut binding = file_desc.lock().unwrap();

        self.release_handle(&mut binding.handle);

        Ok(())
    }

    fn seek(&self, fd: &RawFd, pos: SeekFrom) -> Result<usize, FsError> {
        let binding = get_fd_slots().lock().unwrap();

        let file_desc = binding
            .get(fd.0.try_into().unwrap())
            .ok_or(FsError::LookupError)?;

        let mut binding = file_desc.lock().unwrap();
        let mut metadata_handle = unsafe { &mut *binding.handle.start.cast::<FileMetadata>() };

        let new_pos: i64 = match pos {
            SeekFrom::Start(x) => x as i64,
            SeekFrom::End(x) => (metadata_handle.size as i64) - x,
            SeekFrom::Current(x) => (binding.pos as i64) + x,
        };

        if new_pos > metadata_handle.size.try_into().unwrap() || new_pos < 0 {
            Err(FsError::SeekError)
        } else {
            binding.pos = new_pos as u64;
            Ok(binding.pos.try_into().unwrap())
        }
    }
}
