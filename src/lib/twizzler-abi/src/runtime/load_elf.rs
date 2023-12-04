//! Functions to start new executable programs.

use core::{intrinsics::copy_nonoverlapping, mem::size_of};

use crate::object::InternalObject;

use crate::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    slot::{RESERVED_DATA, RESERVED_STACK, RESERVED_TEXT},
    syscall::{
        sys_unbind_handle, BackingType, HandleType, LifetimeType, MapFlags, NewHandleFlags,
        ObjectCreate, ObjectCreateFlags, ObjectSource, ThreadSpawnArgs, ThreadSpawnFlags,
        UnbindHandleFlags,
    },
};

use twizzler_runtime_api::AuxEntry;

#[derive(Debug)]
#[repr(C)]
pub(crate) struct ElfHeader {
    magic: [u8; 4],
    class: u8,
    data: u8,
    ident_version: u8,
    os_abi: u8,
    abi_version: u8,
    pad: [u8; 7],
    elf_type: u16,
    machine: u16,
    version: u32,
    entry: u64,
    phoff: u64,
    shoff: u64,
    flags: u32,
    ehsize: u16,
    phentsize: u16,
    phnum: u16,
    shentsize: u16,
    shnum: u16,
    shstrndx: u16,
}

impl ElfHeader {
    pub fn verify(&self) -> bool {
        self.magic == [0x7f, b'E', b'L', b'F']
            && self.version == 1
            && self.ident_version == 1
            && self.class == 2 /* 64-bit */
    }
}

#[derive(Debug, PartialEq, PartialOrd, Ord, Eq, Clone, Copy)]
pub(crate) enum PhdrType {
    Null = 0,
    Load = 1,
    Dynamic = 2,
    Interp = 3,
    Phdr = 6,
    Tls = 7,
}

impl TryFrom<u32> for PhdrType {
    type Error = ();
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::Null,
            1 => Self::Load,
            2 => Self::Dynamic,
            3 => Self::Interp,
            6 => Self::Phdr,
            7 => Self::Tls,
            _ => return Err(()),
        })
    }
}

#[repr(C)]
#[derive(Debug)]
pub(crate) struct ElfPhdr {
    ptype: u32,
    flags: u32,
    offset: u64,
    pub(crate) vaddr: u64,
    paddr: u64,
    filesz: u64,
    pub(crate) memsz: u64,
    align: u64,
}

impl ElfPhdr {
    pub fn phdr_type(&self) -> PhdrType {
        self.ptype.try_into().unwrap_or(PhdrType::Null)
    }

    pub fn prot(&self) -> Protections {
        let mut p = Protections::empty();
        if self.flags & 1 != 0 {
            p.insert(Protections::EXEC);
        }
        if self.flags & 2 != 0 {
            p.insert(Protections::WRITE);
        }
        if self.flags & 4 != 0 {
            p.insert(Protections::READ);
        }
        p
    }
}

/// An object that contains an ELF file.
#[derive(Debug)]
pub struct ElfObject<'a> {
    hdr: &'a ElfHeader,
    base_raw: *const u8,
    #[allow(dead_code)]
    obj: &'a InternalObject<ElfHeader>,
}

pub(crate) struct PhdrIter<'a> {
    elf: &'a ElfObject<'a>,
    pos: usize,
}

impl<'a> Iterator for PhdrIter<'a> {
    type Item = &'a ElfPhdr;

    fn next(&mut self) -> Option<Self::Item> {
        let n = self.pos;
        self.pos += 1;
        self.elf.get_phdr(n)
    }
}

impl<'a> ElfObject<'a> {
    fn verify(&self) -> bool {
        self.hdr.verify()
    }

    fn entry(&self) -> u64 {
        self.hdr.entry
    }

    fn get_phdr(&self, pos: usize) -> Option<&'a ElfPhdr> {
        if pos >= self.hdr.phnum as usize {
            return None;
        }
        let offset = pos * self.hdr.phentsize as usize + self.hdr.phoff as usize;
        Some(unsafe { &*(self.base_raw.add(offset) as *const ElfPhdr) })
    }

    fn from_raw_memory(obj: &'a InternalObject<ElfHeader>, mem: *const u8) -> Option<Self> {
        let elf = Self {
            hdr: unsafe { &*(mem as *const ElfHeader) },
            base_raw: mem,
            obj,
        };
        if elf.verify() {
            Some(elf)
        } else {
            None
        }
    }

    pub(crate) fn from_obj(obj: &'a InternalObject<ElfHeader>) -> Option<Self> {
        let start = obj.base();
        Self::from_raw_memory(obj, start as *const ElfHeader as *const u8)
    }

    pub(crate) fn phdrs(&self) -> PhdrIter {
        PhdrIter { elf: self, pos: 0 }
    }
}

const INITIAL_STACK_SIZE: usize = 1024 * 1024 * 4;

extern crate alloc;

/// Possible errors for [spawn_new_executable].
pub enum SpawnExecutableError {
    ObjectCreateFailed,
    InvalidExecutable,
    MapFailed,
    ThreadSpawnFailed,
}

/// Start a new executable in a new address space.
pub fn spawn_new_executable(
    exe: ObjID,
    args: &[&[u8]],
    env: &[&[u8]],
) -> Result<ObjID, SpawnExecutableError> {
    let exe = InternalObject::<ElfHeader>::map(exe, Protections::READ)
        .ok_or(SpawnExecutableError::MapFailed)?;
    let elf = ElfObject::from_obj(&exe).ok_or(SpawnExecutableError::InvalidExecutable)?;

    let cs = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Volatile,
        None,
        ObjectCreateFlags::empty(),
    );
    let vm_handle = crate::syscall::sys_object_create(cs, &[], &[]).unwrap();
    crate::syscall::sys_new_handle(vm_handle, HandleType::VmContext, NewHandleFlags::empty())
        .map_err(|_| SpawnExecutableError::ObjectCreateFailed)?;

    let phdr_vaddr = elf
        .phdrs()
        .find(|p| p.phdr_type() == PhdrType::Phdr)
        .map(|p| p.vaddr);

    use alloc::vec::Vec;
    // map the PT_LOAD directives to copy-from commands Twizzler can use for creating objects.
    let mut copy_cmds: Vec<_> = elf
        .phdrs()
        .filter(|p| p.phdr_type() == PhdrType::Load)
        .map(|phdr| {
            let targets_data = phdr.prot().contains(Protections::WRITE);
            let vaddr = phdr.vaddr as usize;
            let memsz = phdr.memsz as usize;
            let offset = phdr.offset as usize;
            let align = phdr.align as usize;
            let filesz = phdr.filesz as usize;

            fn within_object(slot: usize, addr: usize) -> bool {
                addr >= slot * MAX_SIZE + NULLPAGE_SIZE && addr < (slot + 1) * MAX_SIZE - NULLPAGE_SIZE * 2
            }
            if !within_object(if targets_data { 1 } else { 0 }, vaddr)
                || memsz > MAX_SIZE - NULLPAGE_SIZE * 2
                || offset > MAX_SIZE - NULLPAGE_SIZE * 2
                || filesz > memsz
            {
                panic!("address not within object")
            }

            // the offset from the base of the object with the ELF executable data
            let src_start = NULLPAGE_SIZE + offset;
            // the destination offset is the virtual address we want this data
            // to be mapped into. since the different sections are seperated
            // by object boundaries, we keep the object-relative offset
            // we trust the destination offset to be after the NULL_PAGE
            let dest_start = vaddr as usize % MAX_SIZE;
            // the size of the data that must be copied from the ELF
            let len = filesz;
            
            // NOTE: Data that needs to be initialized to zero is not handled
            // (filesz < memsz). The reason things work now is because
            // the frame allocator in the kernel hands out zeroed pages by default.
            // If this behaviour changes, we will need to explicitly handle it here.
            (
                targets_data,
                ObjectSource::new_copy(
                    exe.id(),
                    src_start as u64,
                    dest_start as u64,
                    len,
                ),
            )
        })
        .collect();

    // Separate out the commands for text and data segmets.
    let text_copy: Vec<_> = copy_cmds
        .iter()
        .filter(|(td, _)| !*td)
        .map(|(_, c)| c)
        .cloned()
        .collect();
    let data_copy: Vec<_> = copy_cmds
        .into_iter()
        .filter(|(td, _)| *td)
        .map(|(_, c)| c)
        .collect();

    let text = crate::syscall::sys_object_create(cs, &text_copy, &[]).unwrap();
    let data = crate::syscall::sys_object_create(cs, &data_copy, &[]).unwrap();

    let mut stack = InternalObject::<()>::create_data_and_map()
        .ok_or(SpawnExecutableError::ObjectCreateFailed)?;

    crate::syscall::sys_object_map(
        Some(vm_handle),
        text,
        RESERVED_TEXT,
        Protections::READ | Protections::EXEC,
        MapFlags::empty(),
    )
    .map_err(|_| SpawnExecutableError::MapFailed)?;
    crate::syscall::sys_object_map(
        Some(vm_handle),
        data,
        RESERVED_DATA,
        Protections::WRITE | Protections::READ,
        MapFlags::empty(),
    )
    .map_err(|_| SpawnExecutableError::MapFailed)?;
    crate::syscall::sys_object_map(
        Some(vm_handle),
        stack.id(),
        RESERVED_STACK,
        Protections::WRITE | Protections::READ,
        MapFlags::empty(),
    )
    .map_err(|_| SpawnExecutableError::MapFailed)?;

    let stack_nullpage = RESERVED_STACK * MAX_SIZE;
    let spawnaux_start = stack_nullpage + AUX_OFFSET;
    const STACK_OFFSET: usize = NULLPAGE_SIZE;
    const AUX_OFFSET: usize = STACK_OFFSET + INITIAL_STACK_SIZE;
    const MAX_AUX: usize = 32;
    const ARGS_OFFSET: usize = AUX_OFFSET + MAX_AUX * size_of::<AuxEntry>();

    fn copy_strings<T>(
        stack: &mut InternalObject<T>,
        strs: &[&[u8]],
        offset: usize,
    ) -> (usize, usize) {
        let stack_nullpage = RESERVED_STACK * MAX_SIZE;
        let offset = offset.checked_next_multiple_of(64).unwrap();
        let args_start = unsafe {
            let args_start: *mut usize = stack.offset_mut(ARGS_OFFSET + offset).unwrap();

            core::slice::from_raw_parts_mut(args_start, strs.len() + 1)
        };
        let spawnargs_start = stack_nullpage + ARGS_OFFSET + offset;

        let args_data_start = {
            let args_data_start: *mut u8 = stack
                .offset_mut(ARGS_OFFSET + offset + size_of::<*const u8>() * (strs.len() + 1))
                .unwrap();
            args_data_start
        };
        let spawnargs_data_start = spawnargs_start + size_of::<*const u8>() * (strs.len() + 1);

        let mut data_offset = 0;
        for (i, arg) in strs.iter().enumerate() {
            let len = arg.len() + 1;
            unsafe {
                copy_nonoverlapping((*arg).as_ptr(), args_data_start.add(data_offset), len - 1);
                args_data_start.add(data_offset + len - 1).write(0);
            }
            args_start[i] = spawnargs_data_start + data_offset;
            data_offset += len;
        }
        args_start[strs.len()] = 0;
        let total = (spawnargs_data_start as usize + data_offset + 16) - spawnargs_start;
        (spawnargs_start, total)
    }

    let (spawnargs_start, args_len) = copy_strings(&mut stack, args, 0);
    let (spawnenv_start, _) = copy_strings(&mut stack, env, args_len);

    let aux_array = unsafe {
        stack
            .offset_mut::<[AuxEntry; 32]>(AUX_OFFSET)
            .unwrap()
            .as_mut()
    }
    .unwrap();
    let mut idx = 0;

    aux_array[idx] = AuxEntry::ExecId(exe.id().as_u128());
    idx += 1;
    aux_array[idx] = AuxEntry::Arguments(args.len(), spawnargs_start as u64);
    idx += 1;
    aux_array[idx] = AuxEntry::Environment(spawnenv_start as u64);
    idx += 1;
    if let Some(phdr_vaddr) = phdr_vaddr {
        aux_array[idx] = AuxEntry::ProgramHeaders(phdr_vaddr, elf.hdr.phnum.into());
        idx += 1;
    }
    aux_array[idx] = AuxEntry::Null;

    let ts = ThreadSpawnArgs::new(
        elf.entry() as usize,
        stack_nullpage + STACK_OFFSET,
        INITIAL_STACK_SIZE,
        0,
        spawnaux_start,
        ThreadSpawnFlags::empty(),
        Some(vm_handle),
    );
    let thr = unsafe {
        crate::syscall::sys_spawn(ts).map_err(|_| SpawnExecutableError::ThreadSpawnFailed)?
    };

    sys_unbind_handle(vm_handle, UnbindHandleFlags::empty());

    //TODO: delete objects

    Ok(thr)
}
