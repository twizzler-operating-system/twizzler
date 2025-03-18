use std::{
    cmp::{max, min},
    io::{ErrorKind, Read, SeekFrom, Write},
    num::NonZeroUsize,
};

use lru::LruCache;
use twizzler_abi::{
    object::{ObjID, NULLPAGE_SIZE},
    syscall::{
        sys_object_create, BackingType, LifetimeType, ObjectControlCmd, ObjectCreate,
        ObjectCreateFlags,
    },
};
use twizzler_rt_abi::{
    fd::FdInfo,
    object::{MapFlags, ObjectHandle},
};

use super::{CreateOptions, OperationOptions};
use crate::OUR_RUNTIME;

#[derive(Clone)]
pub struct FileDesc {
    pos: u64,
    handle: ObjectHandle,
    map: LruCache<usize, ObjectHandle>, // Lazily loads object handles when using extensible files
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
const MAX_LOADABLE_OBJECTS: usize = 16;

impl FileDesc {
    pub fn open(
        open_opt: &OperationOptions,
        obj_id: ObjID,
        flags: MapFlags,
        create_opts: &CreateOptions,
    ) -> std::io::Result<Self> {
        let handle = OUR_RUNTIME.map_object(obj_id, flags).unwrap();
        let metadata_handle = unsafe {
            handle
                .start()
                .offset(NULLPAGE_SIZE as isize)
                .cast::<FileMetadata>()
        };
        if (unsafe { *metadata_handle }).magic != MAGIC_NUMBER {
            match create_opts {
                CreateOptions::CreateKindNew => unsafe {
                    *metadata_handle = FileMetadata {
                        magic: MAGIC_NUMBER,
                        size: 0,
                        direct: [ObjID::new(0); DIRECT_OBJECT_COUNT],
                    };
                },
                _ => {
                    return Err(ErrorKind::Unsupported.into());
                }
            }
        }
        if open_opt.contains(OperationOptions::OPEN_FLAG_TRUNCATE) {
            unsafe {
                { *metadata_handle }.size = 0;
            }
        }

        Ok(FileDesc {
            pos: 0,
            handle,
            map: LruCache::<usize, ObjectHandle>::new(
                NonZeroUsize::new(MAX_LOADABLE_OBJECTS).unwrap(),
            ),
        })
    }

    pub fn seek(&mut self, pos: SeekFrom) -> std::io::Result<usize> {
        let metadata_handle = unsafe {
            &mut *self
                .handle
                .start()
                .offset(NULLPAGE_SIZE as isize)
                .cast::<FileMetadata>()
        };

        let new_pos: i64 = match pos {
            SeekFrom::Start(x) => x as i64,
            SeekFrom::End(x) => (metadata_handle.size as i64) - x,
            SeekFrom::Current(x) => (self.pos as i64) + x,
        };

        if new_pos < 0 {
            Err(ErrorKind::InvalidInput.into())
        } else {
            self.pos = new_pos as u64;
            Ok(self.pos.try_into().unwrap())
        }
    }

    pub fn stat(&self) -> std::io::Result<FdInfo> {
        let metadata_handle = unsafe {
            &mut *self
                .handle
                .start()
                .offset(NULLPAGE_SIZE as isize)
                .cast::<FileMetadata>()
        };

        Ok(FdInfo {
            kind: twizzler_rt_abi::fd::FdKind::Regular,
            size: metadata_handle.size,
            flags: twizzler_rt_abi::fd::FdFlags::empty(),
            id: self.handle.id().raw(),
            unix_mode: 0,
            accessed: std::time::Duration::ZERO,
            modified: std::time::Duration::ZERO,
            created: std::time::Duration::ZERO,
        })
    }

    pub fn fd_cmd(&mut self, cmd: u32, _arg: *const u8, _ret: *mut u8) -> u32 {
        let metadata_handle: &FileMetadata = unsafe {
            self.handle
                .start()
                .offset(NULLPAGE_SIZE as isize)
                .cast::<FileMetadata>()
                .as_ref()
                .unwrap()
        };
        match cmd {
            twizzler_rt_abi::bindings::FD_CMD_SYNC => {
                let mut ok = true;
                for id in &metadata_handle.direct {
                    if id.raw() != 0 {
                        if twizzler_abi::syscall::sys_object_ctrl(*id, ObjectControlCmd::Sync)
                            .is_err()
                        {
                            ok = false;
                        }
                    }
                }
                if twizzler_abi::syscall::sys_object_ctrl(self.handle.id(), ObjectControlCmd::Sync)
                    .is_err()
                {
                    return 1;
                }
                if ok {
                    0
                } else {
                    1
                }
            }
            /*
                        twizzler_rt_abi::bindings::FD_CMD_DELETE => {
                            let mut ok = true;
                            for id in &metadata_handle.direct {
                                if id.raw() != 0 && false {
                                    if twizzler_abi::syscall::sys_object_ctrl(
                                        *id,
                                        ObjectControlCmd::Delete(DeleteFlags::empty()),
                                    )
                                    .is_err()
                                    {
                                        ok = false;
                                    }
                                }
                            }
                            if twizzler_abi::syscall::sys_object_ctrl(
                                self.handle.id(),
                                ObjectControlCmd::Delete(DeleteFlags::empty()),
                            )
                            .is_err()
                            {
                                return 1;
                            }
                            if ok {
                                0
                            } else {
                                1
                            }
                        }
            */
            _ => 1,
        }
    }
}

impl Read for FileDesc {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let metadata_handle = unsafe {
            self.handle
                .start()
                .offset(NULLPAGE_SIZE as isize)
                .cast::<FileMetadata>()
        };

        let mut bytes_read = 0;
        while bytes_read < buf.len() {
            if self.pos > (unsafe { *metadata_handle }).size {
                break;
            }

            let available_bytes = (unsafe { *metadata_handle }).size - self.pos;

            let object_window: usize = (self.pos / WRITABLE_BYTES) as usize;
            let offset = (self.pos) % WRITABLE_BYTES;

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
                self.handle.start()
            } else {
                if let Some(new_handle) = self.map.get(&object_window) {
                    new_handle.start()
                } else {
                    let obj_id =
                        ((unsafe { *metadata_handle }).direct)[(object_window - 1) as usize];
                    let flags = MapFlags::READ | MapFlags::WRITE;
                    let handle = OUR_RUNTIME.map_object(obj_id, flags).unwrap();
                    self.map.put(object_window, handle.clone());
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

            self.pos += bytes_to_read;

            bytes_read += bytes_to_read as usize;
        }

        Ok(bytes_read)
    }
}

impl Write for FileDesc {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let metadata_handle = unsafe {
            self.handle
                .start()
                .offset(NULLPAGE_SIZE as isize)
                .cast::<FileMetadata>()
        };

        let mut bytes_written = 0;
        while bytes_written < buf.len() {
            // The available bytes for writing is the OBJECT_SIZE * OBJECT_COUNT
            // The metadata fills some bytes, the rest is defined by binding.pos which overlays the
            // rest of the object space
            let available_bytes = MAX_FILE_SIZE - self.pos;

            let object_window: usize = (self.pos / WRITABLE_BYTES) as usize;
            let offset = self.pos % WRITABLE_BYTES;

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
                self.handle.start()
            } else {
                // If the object is already mapped, return it's pointer
                if let Some(new_handle) = self.map.get(&object_window) {
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
                            LifetimeType::Persistent,
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

                    let handle = OUR_RUNTIME.map_object(mapped_id, flags).unwrap();
                    self.map.push(object_window, handle.clone());

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
            self.pos += bytes_to_write as u64;
            unsafe { ((*metadata_handle).size) = max(self.pos, (*metadata_handle).size) };
            bytes_written += bytes_to_write as usize;
        }

        Ok(bytes_written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
