use core::{intrinsics::size_of, ptr::NonNull, cmp::min, cmp::max};

use lazy_static::lazy_static;
use rustc_alloc::{borrow::ToOwned, sync::Arc};
use stable_vec::{self, StableVec};
use twizzler_runtime_api::{
    FsError, InternalHandleRefs, JoinError, MapError, ObjectHandle, ObjectRuntime, RawFd,
    RustFsRuntime, SeekFrom,
};

use super::MinimalRuntime;
use crate::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE}, print_err, runtime::{
        idcounter::IdCounter,
        object::slot::{self, global_allocate},
        simple_mutex::Mutex,
    }, rustc_alloc::{boxed::Box, collections::BTreeMap}, syscall::{sys_object_create, sys_object_map, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags, UnmapFlags}, thread::{ExecutionState, ThreadRepr}
};

struct FileDesc {
    slot_id: u32,
    pos: u64,
    handle: ObjectHandle,
    map: [Option<ObjectHandle>; 10], // Lazily loads object handles when using extensible files
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FileMetadata {
    magic: u64,
    size: u64,
    direct: [ObjID; 10],
}

const MAGIC_NUMBER: u64 = 0xBEEFDEAD;
const OBJECT_SIZE: u64 = 1 << 30;
const OBJECT_COUNT: usize = 11;
const MAX_FILE_SIZE: u64 = OBJECT_SIZE * 11;

lazy_static! {
    static ref FD_SLOTS: Mutex<StableVec<Arc<Mutex<FileDesc>>>> = Mutex::new(StableVec::new());
}

fn get_fd_slots() -> &'static Mutex<StableVec<Arc<Mutex<FileDesc>>>> {
    &FD_SLOTS
}

impl RustFsRuntime for MinimalRuntime {
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
                    direct: [ObjID::new(0); 10],
                }
            };
        }

        let fd = get_fd_slots().lock().push(Arc::new(Mutex::new(FileDesc {
            slot_id: 0,
            pos: 0,
            handle: handle,
            map: Default::default(),
        })));
        
        let fd = if binding.is_compact() {
            binding.push(elem)
        }
        else {
            let fd = binding.first_empty_slot_from(0).unwrap();
            binding.insert(fd, elem);
            fd
        };

        Ok(RawFd(fd.try_into().unwrap()))
    }

    fn read(&self, fd: &RawFd, buf: &mut [u8]) -> Result<usize, FsError> {
        let binding = get_fd_slots().lock();
        let mut file_desc = binding
            .get(fd.0.try_into().unwrap())
            .ok_or(FsError::LookupError)?;

        let mut binding = file_desc.lock();

        let mut metadata_handle = unsafe {
            binding
                .handle
                .start
                .offset(NULLPAGE_SIZE as isize)
                .cast::<FileMetadata>()
        };

        let mut bytes_read = 0;
        while bytes_read < buf.len() {
            if (binding.pos > (unsafe{*metadata_handle}).size) {
                break;
            }

            let available_bytes = (unsafe{*metadata_handle}).size - binding.pos;
            
            let object_window: usize = ((binding.pos + size_of::<FileMetadata>() as u64) / OBJECT_SIZE) as usize;
            let mut offset = (binding.pos + size_of::<FileMetadata>() as u64) % OBJECT_SIZE;

            if object_window > OBJECT_COUNT || available_bytes == 0 {
                break;
            }
            // If the offset is in the first object, then 
            
            // OBJECT_SIZE - offset, is the bytes you can write in one object. Offset is bound by modulo of OBJECT_SIZE.
            // available_bytes is the total bytes you can write to the file, this is bound by the writer since the writer can modify the size of the file
            // buf.len() - bytes_read is the bytes you have left to read, this is bound by buf.len() > bytes_read 
            let bytes_to_read = min(min(
                OBJECT_SIZE - offset, 
                available_bytes), 
                (buf.len() - bytes_read) as u64
            );

            let object_ptr = if object_window == 0 {
                binding.handle.start
            }
            else {
                if let Some(new_handle) = &binding.map[object_window - 1] {
                    new_handle.start 
                }
                else {
                    let obj_id = ((unsafe{*metadata_handle}).direct)[(object_window - 1) as usize];
                    let flags = twizzler_runtime_api::MapFlags::READ | twizzler_runtime_api::MapFlags::WRITE;
                    let handle = self.map_object(obj_id, flags).unwrap();
                    binding.map[object_window - 1] = Some(handle.clone());
                    handle.start
                }
            };

            unsafe {
                buf.as_mut_ptr().offset(bytes_read as isize).copy_from(
                    object_ptr.offset(
                        NULLPAGE_SIZE as isize +
                        offset as isize
                    ),
                    bytes_to_read as usize,
                )
            }
            
            binding.pos += bytes_to_read;

            bytes_read += bytes_to_read as usize;
        }

        Ok(bytes_read)
    }

    fn write(&self, fd: &RawFd, buf: &[u8]) -> Result<usize, FsError> {
        let binding = get_fd_slots().lock();
        let file_desc = binding
            .get((&fd).0.try_into().unwrap())
            .ok_or(FsError::LookupError)?;

        let mut binding = file_desc.lock();

        let mut metadata_handle = unsafe {
            binding
                .handle
                .start
                .offset(NULLPAGE_SIZE as isize)
                .cast::<FileMetadata>()
        };

        let mut bytes_written = 0;
        while bytes_written < buf.len() {
            // The available bytes for writing is the OBJECT_SIZE * OBJECT_COUNT
            // The metadata fills some bytes, the rest is defined by binding.pos which overlays the rest of the object space
            let available_bytes = MAX_FILE_SIZE - binding.pos - size_of::<FileMetadata>() as u64;

            let object_window: usize = ((binding.pos + size_of::<FileMetadata>() as u64) / OBJECT_SIZE) as usize;
            let mut offset = (binding.pos + size_of::<FileMetadata>() as u64) % OBJECT_SIZE;
            
            if object_window > OBJECT_COUNT || available_bytes == 0 {
                break;
            }
            
            // OBJECT_SIZE - offset, 0 is the bytes you can write in one object. Offset is bound by modulo of OBJECT_SIZE.
            // available_bytes is the total bytes you can write to the file, available_bytes is always bound by the max file size
            // buf.len() - bytes_written is the bytes you have left to write
            let bytes_to_write = min(min(
                OBJECT_SIZE - offset, 
                available_bytes), 
                (buf.len() - bytes_written) as u64);

                let object_ptr = if object_window == 0 {
                binding.handle.start
            }
            else {
                // If the object is already mapped, return it's pointer
                if let Some(new_handle) = &binding.map[object_window - 1] {
                    new_handle.start 
                }
                // Otherwise check the direct map, if the ID is valid then map it, otherwise create the object, store it, then map it.
                else {
                    let obj_id = ((unsafe{*metadata_handle}).direct)[(object_window - 1) as usize];
                    let flags = twizzler_runtime_api::MapFlags::READ | twizzler_runtime_api::MapFlags::WRITE;

                    let mapped_id = if obj_id == 0.into() {
                        let create = ObjectCreate::new(
                            BackingType::Normal,
                            LifetimeType::Volatile,
                            None,
                            ObjectCreateFlags::empty(),
                        );
                        let new_id = sys_object_create(create, &[], &[]).unwrap();
                        ((unsafe{*metadata_handle}).direct)[(object_window - 1) as usize] = new_id;

                        new_id
                    }
                    else {
                        obj_id
                    };

                    let handle = self.map_object(mapped_id, flags).unwrap();
                    binding.map[object_window - 1] = Some(handle.clone());
                    handle.start
                }
            };

            unsafe {
                object_ptr.offset(NULLPAGE_SIZE as isize + offset as isize).copy_from(
                    buf.as_ptr().offset(
                        bytes_written as isize
                    ),
                    bytes_to_write as usize,
                )
            }
            
            binding.pos += bytes_to_write as u64;
            unsafe {((*metadata_handle).size) = max(binding.pos, (*metadata_handle).size)};
            bytes_written += bytes_to_write as usize;
        }

        Ok(bytes_written)
    }

    fn close(&self, fd: &mut RawFd) -> Result<(), FsError> {
        let file_desc = get_fd_slots()
            .lock()
            .remove(fd.0.try_into().unwrap())
            .ok_or(FsError::LookupError)?;

        let mut binding = file_desc.lock();

        self.release_handle(&mut binding.handle);

        Ok(())
    }

    fn seek(&self, fd: &RawFd, pos: SeekFrom) -> Result<usize, FsError> {
        let binding = get_fd_slots().lock();

        let file_desc = binding
            .get(fd.0.try_into().unwrap())
            .ok_or(FsError::LookupError)?;

        let mut binding = file_desc.lock();
        let mut metadata_handle = unsafe { &mut *binding.handle.start.offset(NULLPAGE_SIZE as isize).cast::<FileMetadata>() };

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
