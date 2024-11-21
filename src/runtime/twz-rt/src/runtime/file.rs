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
use twizzler_rt_abi::{
    fd::{OpenError, RawFd},
    io::{IoError, SeekFrom},
    object::{MapError, MapFlags, ObjectHandle},
    thread::JoinError,
};

use super::ReferenceRuntime;

struct FileDesc {
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

impl ReferenceRuntime {
    pub fn open(&self, path: &core::ffi::CStr) -> Result<RawFd, OpenError> {
        let obj_id = ObjID::new(
            path.to_str()
                .map_err(|_err| (OpenError::InvalidArgument))?
                .parse::<u128>()
                .map_err(|_err| (OpenError::InvalidArgument))?,
        );
        let flags = MapFlags::READ | MapFlags::WRITE;

        let handle = self.map_object(obj_id, flags).unwrap();

        let metadata_handle = unsafe {
            handle
                .start()
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
            .push(Arc::new(Mutex::new(FileDesc { pos: 0, handle })));

        Ok(fd.try_into().unwrap())
    }

    pub fn read(&self, fd: RawFd, buf: &mut [u8]) -> Result<usize, IoError> {
        let binding = get_fd_slots().lock().unwrap();
        let file_desc = binding
            .get(fd.try_into().unwrap())
            .ok_or(IoError::InvalidDesc)?;

        let mut binding = file_desc.lock().unwrap();

        unsafe {
            buf.as_mut_ptr().copy_from(
                binding.handle.start().offset(
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

    pub fn write(&self, fd: RawFd, buf: &[u8]) -> Result<usize, IoError> {
        let binding = get_fd_slots().lock().unwrap();
        let file_desc = binding
            .get(fd.try_into().unwrap())
            .ok_or(IoError::InvalidDesc)?;

        let mut binding = file_desc.lock().unwrap();

        unsafe {
            binding
                .handle
                .start()
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

    pub fn close(&self, fd: RawFd) -> Result<(), IoError> {
        let _file_desc = get_fd_slots()
            .lock()
            .unwrap()
            .remove(fd.try_into().unwrap())
            .ok_or(IoError::InvalidDesc)?;
        Ok(())
    }

    fn seek(&self, fd: RawFd, pos: SeekFrom) -> Result<usize, IoError> {
        let binding = get_fd_slots().lock().unwrap();

        let file_desc = binding
            .get(fd.try_into().unwrap())
            .ok_or(IoError::InvalidDesc)?;

        let mut binding = file_desc.lock().unwrap();
        let metadata_handle = unsafe { &mut *binding.handle.start().cast::<FileMetadata>() };

        let new_pos: i64 = match pos {
            SeekFrom::Start(x) => x as i64,
            SeekFrom::End(x) => (metadata_handle.size as i64) - x,
            SeekFrom::Current(x) => (binding.pos as i64) + x,
        };

        if new_pos > metadata_handle.size.try_into().unwrap() || new_pos < 0 {
            Err(IoError::SeekError)
        } else {
            binding.pos = new_pos as u64;
            Ok(binding.pos.try_into().unwrap())
        }
    }
}
