use core::{
    cmp::{max, min},
    mem::size_of,
    num::NonZeroUsize,
    time::Duration,
};

use lazy_static::lazy_static;
use lru::LruCache;
use rustc_alloc::sync::Arc;
use stable_vec::{self, StableVec};
use twizzler_abi::{
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
    simple_mutex::Mutex,
    syscall::{sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
};
use twizzler_rt_abi::{
    bindings::{io_ctx, io_vec},
    error::{ArgumentError, GenericError, IoError},
    fd::RawFd,
    io::SeekFrom,
    object::{MapFlags, ObjectHandle},
    Result,
};

use super::MinimalRuntime;

#[derive(Clone)]
struct FileDesc {
    pos: u64,
    handle: ObjectHandle,
    map: LruCache<usize, ObjectHandle>, // Lazily loads object handles when using extensible files
}

#[derive(Clone)]
enum FdKind {
    File(Arc<Mutex<FileDesc>>),
    Stdio,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FileMetadata {
    magic: u64,
    size: u64,
    direct: [ObjID; DIRECT_OBJECT_COUNT],
}

const MAGIC_NUMBER: u64 = 0xBEEFDEAD;
// 64 megabytes
const WRITABLE_BYTES: u64 = (1 << 26) - size_of::<FileMetadata>() as u64 - NULLPAGE_SIZE as u64;
const OBJECT_COUNT: usize = 256;
const DIRECT_OBJECT_COUNT: usize = 255; // The number of objects reachable from the direct pointer list
const MAX_FILE_SIZE: u64 = WRITABLE_BYTES * 256;
const _MAX_LOADABLE_OBJECTS: usize = 16;
lazy_static! {
    static ref FD_SLOTS: Mutex<StableVec<FdKind>> = Mutex::new(StableVec::from([
        FdKind::Stdio,
        FdKind::Stdio,
        FdKind::Stdio
    ]));
}

fn get_fd_slots() -> &'static Mutex<StableVec<FdKind>> {
    &FD_SLOTS
}

impl MinimalRuntime {
    pub fn open(&self, path: &str) -> Result<RawFd> {
        let obj_id = ObjID::new(
            path.parse::<u128>()
                .map_err(|_err| (ArgumentError::InvalidArgument))?,
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
                    direct: [ObjID::new(0); DIRECT_OBJECT_COUNT],
                }
            };
        }

        let mut binding = get_fd_slots().lock();

        let elem = FdKind::File(Arc::new(Mutex::new(FileDesc {
            pos: 0,
            handle,
            map: LruCache::<usize, ObjectHandle>::new(NonZeroUsize::new(1).unwrap()),
        })));

        let fd = if binding.is_compact() {
            binding.push(elem)
        } else {
            let fd = binding.first_empty_slot_from(0).unwrap();
            binding.insert(fd, elem);
            fd
        };

        Ok(fd.try_into().unwrap())
    }

    pub fn read(&self, fd: RawFd, buf: &mut [u8]) -> Result<usize> {
        let binding = get_fd_slots().lock();
        let file_desc = binding
            .get(fd.try_into().unwrap())
            .ok_or(ArgumentError::BadHandle)?;

        let FdKind::File(file_desc) = &file_desc else {
            // Just do basic stdio via kernel console
            let len = twizzler_abi::syscall::sys_kernel_console_read(
                buf,
                twizzler_abi::syscall::KernelConsoleReadFlags::empty(),
            )
            .map_err(|_| IoError::DeviceError)?;
            return Ok(len);
        };

        let mut binding = file_desc.lock();

        let metadata_handle = unsafe {
            binding
                .handle
                .start()
                .offset(NULLPAGE_SIZE as isize)
                .cast::<FileMetadata>()
        };

        let mut bytes_read = 0;
        while bytes_read < buf.len() {
            if binding.pos > (unsafe { *metadata_handle }).size {
                break;
            }

            let available_bytes = (unsafe { *metadata_handle }).size - binding.pos;

            let object_window: usize = ((binding.pos) / WRITABLE_BYTES) as usize;
            let offset = (binding.pos) % WRITABLE_BYTES;

            if object_window > OBJECT_COUNT || available_bytes == 0 {
                break;
            }
            // If the offset is in the first object, then

            // OBJECT_SIZE - offset, is the bytes you can write in one object. Offset is bound by
            // modulo of OBJECT_SIZE. available_bytes is the total bytes you can write
            // to the file, this is bound by the writer since the writer can modify the size of the
            // file buf.len() - bytes_read is the bytes you have left to read, this is
            // bound by buf.len() > bytes_read
            let bytes_to_read = min(
                min(WRITABLE_BYTES - offset, available_bytes),
                (buf.len() - bytes_read) as u64,
            );

            let object_ptr = if object_window == 0 {
                binding.handle.start()
            } else {
                if let Some(new_handle) = binding.map.get(&object_window) {
                    new_handle.start()
                } else {
                    let obj_id =
                        ((unsafe { *metadata_handle }).direct)[(object_window - 1) as usize];
                    let flags = MapFlags::READ | MapFlags::WRITE;
                    let handle = self.map_object(obj_id, flags).unwrap();
                    binding.map.put(object_window, handle.clone());
                    handle.start()
                }
            };

            unsafe {
                buf.as_mut_ptr().offset(bytes_read as isize).copy_from(
                    object_ptr.offset(
                        NULLPAGE_SIZE as isize
                            + size_of::<FileMetadata>() as isize
                            + offset as isize,
                    ),
                    bytes_to_read as usize,
                )
            }

            binding.pos += bytes_to_read;

            bytes_read += bytes_to_read as usize;
        }

        Ok(bytes_read)
    }

    pub fn fd_pread(&self, fd: RawFd, buf: &mut [u8], _ctx: Option<&mut io_ctx>) -> Result<usize> {
        self.read(fd, buf)
    }

    pub fn fd_pwrite(&self, fd: RawFd, buf: &[u8], _ctx: Option<&mut io_ctx>) -> Result<usize> {
        self.write(fd, buf)
    }

    pub fn fd_pwritev(
        &self,
        _fd: RawFd,
        _buf: &[io_vec],
        _ctx: Option<&mut io_ctx>,
    ) -> Result<usize> {
        return Err(GenericError::NotSupported.into());
    }

    pub fn fd_preadv(
        &self,
        _fd: RawFd,
        _buf: &[io_vec],
        _ctx: Option<&mut io_ctx>,
    ) -> Result<usize> {
        return Err(GenericError::NotSupported.into());
    }

    pub fn fd_get_info(&self, fd: RawFd) -> Option<twizzler_rt_abi::bindings::fd_info> {
        let binding = get_fd_slots().lock();
        if binding.get(fd.try_into().unwrap()).is_none() {
            return None;
        }
        Some(twizzler_rt_abi::bindings::fd_info {
            flags: 0,
            kind: twizzler_rt_abi::fd::FdKind::Regular.into(),
            len: (MAX_SIZE - NULLPAGE_SIZE * 2) as u64,
            id: 0,
            created: Duration::from_secs(0).into(),
            modified: Duration::from_secs(0).into(),
            accessed: Duration::from_secs(0).into(),
            unix_mode: 0,
        })
    }

    pub fn write(&self, fd: RawFd, buf: &[u8]) -> Result<usize> {
        let binding = get_fd_slots().lock();
        let file_desc = binding
            .get(fd.try_into().unwrap())
            .ok_or(ArgumentError::BadHandle)?;

        let FdKind::File(file_desc) = &file_desc else {
            // Just do basic stdio via kernel console
            twizzler_abi::syscall::sys_kernel_console_write(
                buf,
                twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
            );
            return Ok(buf.len());
        };

        let mut binding = file_desc.lock();

        let metadata_handle = unsafe {
            binding
                .handle
                .start()
                .offset(NULLPAGE_SIZE as isize)
                .cast::<FileMetadata>()
        };

        let mut bytes_written = 0;
        while bytes_written < buf.len() {
            // The available bytes for writing is the OBJECT_SIZE * OBJECT_COUNT
            // The metadata fills some bytes, the rest is defined by binding.pos which overlays the
            // rest of the object space
            let available_bytes = MAX_FILE_SIZE - binding.pos;

            let object_window: usize = (binding.pos / WRITABLE_BYTES) as usize;
            let offset = binding.pos % WRITABLE_BYTES;

            if object_window > OBJECT_COUNT || available_bytes == 0 {
                break;
            }

            // OBJECT_SIZE - offset, 0 is the bytes you can write in one object. Offset is bound by
            // modulo of OBJECT_SIZE. available_bytes is the total bytes you can write
            // to the file, available_bytes is always bound by the max file size
            // buf.len() - bytes_written is the bytes you have left to write
            let bytes_to_write = min(
                min(WRITABLE_BYTES - offset, available_bytes),
                (buf.len() - bytes_written) as u64,
            );

            let object_ptr = if object_window == 0 {
                binding.handle.start()
            } else {
                // If the object is already mapped, return it's pointer
                if let Some(new_handle) = binding.map.get(&object_window) {
                    new_handle.start()
                }
                // Otherwise check the direct map, if the ID is valid then map it, otherwise create
                // the object, store it, then map it.
                else {
                    let obj_id =
                        ((unsafe { *metadata_handle }).direct)[(object_window - 1) as usize];

                    let flags = MapFlags::READ | MapFlags::WRITE;

                    let mapped_id = if obj_id == 0.into() {
                        let create = ObjectCreate::new(
                            BackingType::Normal,
                            LifetimeType::Volatile,
                            None,
                            ObjectCreateFlags::empty(),
                        );
                        let new_id = sys_object_create(create, &[], &[]).unwrap();
                        unsafe {
                            (*metadata_handle).direct[(object_window - 1) as usize] = new_id;
                        }
                        new_id
                    } else {
                        obj_id
                    };

                    let handle = self.map_object(mapped_id, flags).unwrap();
                    binding.map.push(object_window, handle.clone());

                    handle.start()
                }
            };

            unsafe {
                object_ptr
                    .offset(
                        NULLPAGE_SIZE as isize
                            + size_of::<FileMetadata>() as isize
                            + offset as isize,
                    )
                    .copy_from(
                        buf.as_ptr().offset(bytes_written as isize),
                        (bytes_to_write) as usize,
                    )
            }
            binding.pos += bytes_to_write as u64;
            unsafe { ((*metadata_handle).size) = max(binding.pos, (*metadata_handle).size) };
            bytes_written += bytes_to_write as usize;
        }

        Ok(bytes_written)
    }

    pub fn close(&self, fd: RawFd) -> Option<()> {
        let _file_desc = get_fd_slots().lock().remove(fd.try_into().unwrap())?;

        Some(())
    }

    pub fn seek(&self, fd: RawFd, pos: SeekFrom) -> Result<usize> {
        let binding = get_fd_slots().lock();
        let file_desc = binding
            .get(fd.try_into().unwrap())
            .ok_or(ArgumentError::BadHandle)?;

        let FdKind::File(file_desc) = &file_desc else {
            return Err(IoError::SeekFailed.into());
        };

        let mut binding = file_desc.lock();

        let metadata_handle = unsafe {
            &mut *binding
                .handle
                .start()
                .offset(NULLPAGE_SIZE as isize)
                .cast::<FileMetadata>()
        };

        let new_pos: i64 = match pos {
            SeekFrom::Start(x) => x as i64,
            SeekFrom::End(x) => (metadata_handle.size as i64) - x,
            SeekFrom::Current(x) => (binding.pos as i64) + x,
        };

        if new_pos < 0 {
            Err(IoError::SeekFailed.into())
        } else {
            binding.pos = new_pos as u64;
            Ok(binding.pos.try_into().unwrap())
        }
    }
}
