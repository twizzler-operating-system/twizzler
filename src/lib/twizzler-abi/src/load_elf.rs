use core::intrinsics::transmute;

use crate::{
    aux::AuxEntry,
    object::{ObjID, Protections},
    syscall::{
        BackingType, HandleType, LifetimeType, MapFlags, NewHandleFlags, ObjectCreate,
        ObjectCreateFlags, ObjectSource, ThreadSpawnArgs, ThreadSpawnFlags,
    },
};

#[derive(Debug)]
#[repr(C)]
struct ElfHeader {
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
        self.magic == [0x7f, 'E' as u8, 'L' as u8, 'F' as u8]
            && self.version == 1
            && self.ident_version == 1
            && self.class == 2 /* 64-bit */
    }
}

#[derive(Debug, PartialEq, PartialOrd, Ord, Eq, Clone, Copy)]
enum PhdrType {
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
            _ => Err(())?,
        })
    }
}

#[repr(C)]
#[derive(Debug)]
struct ElfPhdr {
    ptype: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    paddr: u64,
    filesz: u64,
    memsz: u64,
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

#[derive(Debug)]
pub struct ElfObject<'a> {
    hdr: &'a ElfHeader,
    base_raw: *const u8,
}

struct PhdrIter<'a> {
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

    fn ph_entry_size(&self) -> usize {
        self.hdr.phentsize as usize
    }

    fn get_phdr(&self, pos: usize) -> Option<&'a ElfPhdr> {
        if pos >= self.hdr.phnum as usize {
            return None;
        }
        let offset = pos * self.hdr.phentsize as usize + self.hdr.phoff as usize;
        Some(unsafe { transmute(self.base_raw.add(offset)) })
    }

    fn from_raw_memroy(mem: *const u8) -> Option<Self> {
        let elf = Self {
            hdr: unsafe { transmute(mem) },
            base_raw: mem,
        };
        if elf.verify() {
            Some(elf)
        } else {
            None
        }
    }

    fn phdrs(&self) -> PhdrIter {
        PhdrIter { elf: self, pos: 0 }
    }
}

extern crate alloc;
pub fn spawn_new_executable(exe: ObjID) -> Option<ElfObject<'static>> {
    let slot = 1000; //TODO
    let stackslot = 1001; //TODO

    crate::syscall::sys_object_map(None, exe, slot, Protections::READ, MapFlags::empty()).unwrap();
    let (start, _) = crate::slot::to_vaddr_range(slot);
    let elf = ElfObject::from_raw_memroy(start as *const u8);
    if elf.is_none() {
        crate::print_err("LOL\n");
        return None;
    }

    let elf = elf.unwrap();

    let cs = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Volatile,
        None,
        ObjectCreateFlags::empty(),
    );
    let vm_handle = crate::syscall::sys_object_create(cs, &[], &[]).unwrap();
    crate::syscall::sys_new_handle(vm_handle, HandleType::VmContext, NewHandleFlags::empty())
        .unwrap();

    let mut text_copy = alloc::vec::Vec::new();
    let mut data_copy = alloc::vec::Vec::new();
    let mut data_zero = alloc::vec::Vec::new();
    let page_size = 0x1000; //TODO
    let null_page_size = 0x1000; //TODO
    let obj_size = 1024 * 1024 * 1024; //TODO
    for phdr in elf.phdrs().filter(|p| p.phdr_type() == PhdrType::Load) {
        crate::print_err("got phdr\n");
        let cs = ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
        );
        let src_start = (phdr.offset & ((!page_size) + 1)) + null_page_size;
        let dest_start = phdr.vaddr & ((!page_size) + 1);
        let len = (phdr.filesz as u64 + (phdr.vaddr & (page_size - 1))) as usize;
        let aligned_len = len.checked_next_multiple_of(page_size as usize).unwrap();
        let copy = ObjectSource::new(exe, src_start, dest_start, aligned_len);
        let prot = phdr.prot();
        if prot.contains(Protections::WRITE) {
            let brk = (phdr.vaddr & (page_size - 1)) + phdr.filesz;
            let pgbrk = (brk + (page_size - 1)) & ((!page_size) + 1);
            let pgend = (brk + phdr.memsz - phdr.filesz + (page_size - 1)) & ((!page_size) + 1);
            let dest_start = pgbrk & (obj_size - 1);
            let dest_zero_start = brk & (obj_size - 1);
            data_copy.push(copy);
            if pgend > pgbrk {
                data_copy.push(ObjectSource::new(
                    ObjID::new(0),
                    0,
                    dest_start,
                    (pgend - pgbrk) as usize,
                ))
            }
            data_zero.push((dest_zero_start, pgbrk - brk));
        } else {
            text_copy.push(copy);
        }
    }

    let text = crate::syscall::sys_object_create(cs, &text_copy, &[]).unwrap();
    let data = crate::syscall::sys_object_create(cs, &data_copy, &[]).unwrap();
    let stack = crate::syscall::sys_object_create(cs, &[], &[]).unwrap();

    crate::syscall::sys_object_map(
        Some(vm_handle),
        text,
        0,
        Protections::READ | Protections::EXEC,
        MapFlags::empty(),
    )
    .unwrap();
    crate::syscall::sys_object_map(
        Some(vm_handle),
        data,
        1,
        Protections::WRITE | Protections::READ,
        MapFlags::empty(),
    )
    .unwrap();
    crate::syscall::sys_object_map(
        Some(vm_handle),
        stack,
        2,
        Protections::WRITE | Protections::READ,
        MapFlags::empty(),
    )
    .unwrap();

    let stack_addr = 1024u64 * 1024 * 1024 * 2 + 0x1000;

    let stackslice = unsafe { core::slice::from_raw_parts(stack_addr as *const u8, 0x200000) };

    fn append_aux(aux: *mut AuxEntry, entry: AuxEntry) -> *mut AuxEntry {
        unsafe {
            *aux = entry;
            aux.add(1)
        }
    }

    crate::syscall::sys_object_map(
        None,
        stack,
        stackslot,
        Protections::WRITE | Protections::READ,
        MapFlags::empty(),
    )
    .unwrap();

    let aux_start: u64 = (1 << 30) * (stackslot as u64) + 0x300000;
    let spawnaux_start = (1 << 30) * 2 + 0x300000;
    let aux_start = aux_start as *mut AuxEntry;
    let mut aux = aux_start;

    if let Some(phinfo) = elf
        .phdrs()
        .filter(|p| p.phdr_type() == PhdrType::Phdr)
        .next()
    {
        aux = append_aux(
            aux,
            AuxEntry::ProgramHeaders(phinfo.vaddr, phinfo.memsz as usize / elf.ph_entry_size()),
        )
    }

    aux = append_aux(aux, AuxEntry::ExecId(exe));

    let ts = ThreadSpawnArgs::new(
        elf.entry() as usize,
        stackslice,
        0,
        spawnaux_start,
        ThreadSpawnFlags::empty(),
        Some(vm_handle),
    );
    unsafe {
        crate::syscall::sys_spawn(ts).unwrap();
    }

    Some(elf)
}
