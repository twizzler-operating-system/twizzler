use alloc::borrow::ToOwned;
use alloc::sync::Arc;
use core::convert::TryFrom;
use core::ops::Deref;
use core::{slice, str};

use object::elf::{ELFCOMPRESS_ZLIB, ProgramHeader64, SHF_COMPRESSED};
use object::read::StringTable;
use object::read::elf::{CompressionHeader, FileHeader, SectionHeader, SectionTable, Sym};
use object::{BigEndian, Bytes, NativeEndian};
use twizzler_rt_abi::debug::LoadedImage;
use twizzler_rt_abi::object::ObjectHandle;

use super::mystd::ffi::{OsStr, OsString};
use super::mystd::os::twizzler::ffi::OsStrExt;
use super::{Context, Either, Endian, EndianSlice, Mapping, Stash, Vec, gimli};

#[cfg(target_pointer_width = "32")]
type Elf = object::elf::FileHeader32<NativeEndian>;
#[cfg(target_pointer_width = "64")]
type Elf = object::elf::FileHeader64<NativeEndian>;

pub(super) fn native_libraries() -> Vec<super::Library> {
    let mut ret = Vec::new();
    let mut id = twizzler_rt_abi::debug::TWZ_RT_EXEID;
    while let Some(lib) = twizzler_rt_abi::debug::twz_rt_get_loaded_image(id) {
        let headers = if lib.dl_info().phdr.is_null() || lib.dl_info().phnum == 0 {
            &[]
        } else {
            // SAFETY: We just checked for nullness or 0-len slices
            unsafe {
                slice::from_raw_parts(
                    lib.dl_info().phdr.cast::<ProgramHeader64<NativeEndian>>(),
                    lib.dl_info().phnum as usize,
                )
            }
        };
        // this fallback works even if we are main, because some platforms give the name anyways
        let name = if lib.dl_info().name.is_null() {
            OsString::new()
        } else {
            // SAFETY: we just checked for nullness
            OsStr::from_bytes(unsafe { core::ffi::CStr::from_ptr(lib.dl_info().name) }.to_bytes())
                .to_owned()
        };
        ret.push(super::Library {
            name,
            segments: headers
                .iter()
                .map(|header| super::LibrarySegment {
                    len: (*header).p_memsz.get(NativeEndian) as usize,
                    stated_virtual_memory_address: (*header).p_vaddr.get(NativeEndian) as usize,
                })
                .collect(),
            bias: lib.dl_info().addr as usize,
            image: lib,
        });
        id += 1;
    }
    return ret;
}

pub struct Mmap {
    ptr: *const u8,
    // Keeps the object mapped for as long as `ptr` is handed out.
    _handle: ObjectHandle,
    len: usize,
}

impl Deref for Mmap {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Mapping {
    pub fn new_twizzler(lib: &LoadedImage) -> Option<Mapping> {
        let map = Mmap {
            ptr: lib.image().as_ptr(),
            _handle: lib.handle().clone(),
            len: lib.image().len(),
        };
        Mapping::mk_or_other(map, |map, stash| {
            let object = Object::parse(&map)?;
            Context::new(stash, object, None, None).map(Either::B)
        })
    }
}

#[derive(Debug)]
struct ParsedSym {
    address: u64,
    size: u64,
    name: u32,
}

pub struct Object<'a> {
    /// Zero-sized type representing the native endianness.
    ///
    /// We could use a literal instead, but this helps ensure correctness.
    endian: NativeEndian,
    /// The entire file data.
    data: &'a [u8],
    sections: SectionTable<'a, Elf>,
    strings: StringTable<'a>,
    /// List of pre-parsed and sorted symbols by base address.
    syms: Vec<ParsedSym>,
}

impl<'a> Object<'a> {
    fn parse(data: &'a [u8]) -> Option<Object<'a>> {
        let elf = Elf::parse(data).ok()?;
        let endian = elf.endian().ok()?;
        let sections = elf.sections(endian, data).ok()?;
        let mut syms = sections.symbols(endian, data, object::elf::SHT_SYMTAB).ok()?;
        if syms.is_empty() {
            syms = sections.symbols(endian, data, object::elf::SHT_DYNSYM).ok()?;
        }
        let strings = syms.strings();

        let mut syms = syms
            .iter()
            // Only look at function/object symbols. This mirrors what
            // libbacktrace does and in general we're only symbolicating
            // function addresses in theory. Object symbols correspond
            // to data, and maybe someone's crazy enough to have a
            // function go into static data?
            .filter(|sym| {
                let st_type = sym.st_type();
                st_type == object::elf::STT_FUNC || st_type == object::elf::STT_OBJECT
            })
            // skip anything that's in an undefined section header,
            // since it means it's an imported function and we're only
            // symbolicating with locally defined functions.
            .filter(|sym| sym.st_shndx(endian) != object::elf::SHN_UNDEF)
            .map(|sym| {
                let address = sym.st_value(endian).into();
                let size = sym.st_size(endian).into();
                let name = sym.st_name(endian);
                ParsedSym { address, size, name }
            })
            .collect::<Vec<_>>();
        syms.sort_unstable_by_key(|s| s.address);
        Some(Object { endian, data, sections, strings, syms })
    }

    pub fn section(&self, stash: &'a Stash, name: &str) -> Option<&'a [u8]> {
        if let Some(section) = self.section_header(name) {
            let mut data = Bytes(section.data(self.endian, self.data).ok()?);

            // Check for DWARF-standard (gABI) compression, i.e., as generated
            // by ld's `--compress-debug-sections=zlib-gabi` flag.
            let flags: u64 = section.sh_flags(self.endian).into();
            if (flags & u64::from(SHF_COMPRESSED)) == 0 {
                // Not compressed.
                return Some(data.0);
            }

            let header = data.read::<<Elf as FileHeader>::CompressionHeader>().ok()?;
            if header.ch_type(self.endian) != ELFCOMPRESS_ZLIB {
                // Zlib compression is the only known type.
                return None;
            }
            let size = usize::try_from(header.ch_size(self.endian)).ok()?;
            let buf = stash.allocate(size);
            decompress_zlib(data.0, buf)?;
            return Some(buf);
        }

        // Check for the nonstandard GNU compression format, i.e., as generated
        // by ld's `--compress-debug-sections=zlib-gnu` flag. This means that if
        // we're actually asking for `.debug_info` then we need to look up a
        // section named `.zdebug_info`.
        if !name.starts_with(".debug_") {
            return None;
        }
        let debug_name = name[7..].as_bytes();
        let compressed_section = self
            .sections
            .iter()
            .filter_map(|header| {
                let name = self.sections.section_name(self.endian, header).ok()?;
                if name.starts_with(b".zdebug_") && &name[8..] == debug_name {
                    Some(header)
                } else {
                    None
                }
            })
            .next()?;
        let mut data = Bytes(compressed_section.data(self.endian, self.data).ok()?);
        if data.read_bytes(8).ok()?.0 != b"ZLIB\0\0\0\0" {
            return None;
        }
        let size = usize::try_from(data.read::<object::U32Bytes<_>>().ok()?.get(BigEndian)).ok()?;
        let buf = stash.allocate(size);
        decompress_zlib(data.0, buf)?;
        Some(buf)
    }

    fn section_header(&self, name: &str) -> Option<&<Elf as FileHeader>::SectionHeader> {
        self.sections.section_by_name(self.endian, name.as_bytes()).map(|(_index, section)| section)
    }

    pub fn search_symtab<'b>(&'b self, addr: u64) -> Option<&'b [u8]> {
        // Same sort of binary search as Windows above
        let i = match self.syms.binary_search_by_key(&addr, |sym| sym.address) {
            Ok(i) => i,
            Err(i) => i.checked_sub(1)?,
        };
        let sym = self.syms.get(i)?;
        if sym.address <= addr && addr <= sym.address + sym.size {
            self.strings.get(sym.name).ok()
        } else {
            None
        }
    }

    pub(super) fn search_object_map(&self, _addr: u64) -> Option<(&Context<'_>, u64)> {
        None
    }
}

fn decompress_zlib(input: &[u8], output: &mut [u8]) -> Option<()> {
    use miniz_oxide::inflate::TINFLStatus;
    use miniz_oxide::inflate::core::inflate_flags::{
        TINFL_FLAG_PARSE_ZLIB_HEADER, TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF,
    };
    use miniz_oxide::inflate::core::{DecompressorOxide, decompress};

    let (status, in_read, out_read) = decompress(
        &mut DecompressorOxide::new(),
        input,
        output,
        0,
        TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF | TINFL_FLAG_PARSE_ZLIB_HEADER,
    );
    if status == TINFLStatus::Done && in_read == input.len() && out_read == output.len() {
        Some(())
    } else {
        None
    }
}

pub(super) fn handle_split_dwarf<'data>(
    _package: Option<&gimli::DwarfPackage<EndianSlice<'data, Endian>>>,
    _stash: &'data Stash,
    _load: addr2line::SplitDwarfLoad<EndianSlice<'data, Endian>>,
) -> Option<Arc<gimli::Dwarf<EndianSlice<'data, Endian>>>> {
    // TODO (dbittman): Add support
    None
}
