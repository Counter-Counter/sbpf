//! Dependency-less 64 bit ELF parser

pub mod consts;
pub mod types;

use alloc::string::{String, ToString};
use core::{fmt, mem, ops::Range, slice};

use crate::{ArithmeticOverflow, ErrCheckedArithmetic};
use {consts::*, types::*};

/// Maximum length of section name allowed.
pub const SECTION_NAME_LENGTH_MAXIMUM: usize = 16;
const SYMBOL_NAME_LENGTH_MAXIMUM: usize = 64;

/// Error definitions
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ElfParserError {
    /// ELF file header is inconsistent or unsupported
    #[error("invalid file header")]
    InvalidFileHeader,
    /// Program header is inconsistent or unsupported
    #[error("invalid program header")]
    InvalidProgramHeader,
    /// Section header is inconsistent or unsupported
    #[error("invalid section header")]
    InvalidSectionHeader,
    /// Section or symbol name is not UTF8 or too long
    #[error("invalid string")]
    InvalidString,
    /// Section or symbol name is too long
    #[error("Section or symbol name `{0}` is longer than `{1}` bytes")]
    StringTooLong(String, usize),
    /// An index or memory range does exceed its boundaries
    #[error("value out of bounds")]
    OutOfBounds,
    /// The size isn't valid
    #[error("invalid size")]
    InvalidSize,
    /// Headers, tables or sections do overlap in the file
    #[error("values overlap")]
    Overlap,
    /// Sections are not sorted in ascending order
    #[error("sections not in ascending order")]
    SectionNotInOrder,
    /// No section name string table present in the file
    #[error("no section name string table found")]
    NoSectionNameStringTable,
    /// Invalid .dynamic section table
    #[error("invalid dynamic section table")]
    InvalidDynamicSectionTable,
    /// Invalid relocation table
    #[error("invalid relocation table")]
    InvalidRelocationTable,
    /// Invalid alignment
    #[error("invalid alignment")]
    InvalidAlignment,
    /// No string table
    #[error("no string table")]
    NoStringTable,
    /// No dynamic string table
    #[error("no dynamic string table")]
    NoDynamicStringTable,
}

impl Elf64Phdr {
    /// Returns the byte range the section spans in the file.
    pub fn file_range(&self) -> Option<Range<usize>> {
        (self.p_type == PT_LOAD).then(|| {
            let offset = self.p_offset as usize;
            offset..offset.saturating_add(self.p_filesz as usize)
        })
    }

    /// Returns the segment virtual address range.
    pub fn vm_range(&self) -> Range<Elf64Addr> {
        let addr = self.p_vaddr;
        addr..addr.saturating_add(self.p_memsz)
    }
}

impl Elf64Shdr {
    /// Returns whether the section is writable.
    pub fn is_writable(&self) -> bool {
        self.sh_flags & (SHF_ALLOC | SHF_WRITE) == SHF_ALLOC | SHF_WRITE
    }

    /// Returns the byte range the section spans in the file.
    pub fn file_range(&self) -> Option<Range<usize>> {
        (self.sh_type != SHT_NOBITS).then(|| {
            let offset = self.sh_offset as usize;
            offset..offset.saturating_add(self.sh_size as usize)
        })
    }

    /// Returns the virtual address range.
    pub fn vm_range(&self) -> Range<Elf64Addr> {
        self.sh_addr..self.sh_addr.saturating_add(self.sh_size)
    }
}

impl Elf64Sym {
    /// Returns whether the symbol is a function.
    pub fn is_function(&self) -> bool {
        (self.st_info & 0xF) == STT_FUNC
    }
}

impl Elf64Rel {
    /// Returns the relocation type.
    pub fn r_type(&self) -> Elf64Word {
        (self.r_info & 0xFFFFFFFF) as Elf64Word
    }

    /// Returns the symbol index.
    pub fn r_sym(&self) -> Elf64Word {
        self.r_info.checked_shr(32).unwrap_or(0) as Elf64Word
    }
}

fn check_that_there_is_no_overlap(
    range_a: &Range<usize>,
    range_b: &Range<usize>,
) -> Result<(), ElfParserError> {
    if range_a.end <= range_b.start || range_b.end <= range_a.start {
        Ok(())
    } else {
        Err(ElfParserError::Overlap)
    }
}

/// The parsed structure of an ELF file
pub struct Elf64<'a> {
    elf_bytes: &'a [u8],
    file_header: &'a Elf64Ehdr,
    program_header_table: &'a [Elf64Phdr],
    section_header_table: &'a [Elf64Shdr],
    section_names_section_header: Option<&'a Elf64Shdr>,
    symbol_section_header: Option<&'a Elf64Shdr>,
    symbol_names_section_header: Option<&'a Elf64Shdr>,
    dynamic_table: [Elf64Xword; DT_NUM],
    dynamic_relocations_table: Option<&'a [Elf64Rel]>,
    dynamic_symbol_table: Option<&'a [Elf64Sym]>,
    dynamic_symbol_names_section_header: Option<&'a Elf64Shdr>,
}

impl<'a> Elf64<'a> {
    /// Parse from the given byte slice
    pub fn parse(elf_bytes: &'a [u8]) -> Result<Self, ElfParserError> {
        let (file_header_range, file_header) = Self::parse_file_header(elf_bytes)?;

        if file_header.e_ident.ei_mag != ELFMAG
            || file_header.e_ident.ei_class != ELFCLASS64
            || file_header.e_ident.ei_data != ELFDATA2LSB
            || file_header.e_ident.ei_version != EV_CURRENT as u8
            || file_header.e_version != EV_CURRENT
            || file_header.e_ehsize != mem::size_of::<Elf64Ehdr>() as u16
            || file_header.e_phentsize != mem::size_of::<Elf64Phdr>() as u16
            || file_header.e_shentsize != mem::size_of::<Elf64Shdr>() as u16
            || file_header.e_shstrndx >= file_header.e_shnum
        {
            return Err(ElfParserError::InvalidFileHeader);
        }

        let (program_header_table_range, program_header_table) =
            Self::parse_program_header_table(elf_bytes, file_header_range.clone(), file_header)?;

        let (section_header_table_range, section_header_table) = Self::parse_section_header_table(
            elf_bytes,
            file_header_range.clone(),
            file_header,
            program_header_table_range.clone(),
        )?;

        section_header_table
            .first()
            .filter(|section_header| section_header.sh_type == SHT_NULL)
            .ok_or(ElfParserError::InvalidSectionHeader)?;

        let mut vaddr = 0;
        for program_header in program_header_table {
            if program_header.p_type != PT_LOAD {
                continue;
            }
            // program headers must be ascending
            if program_header.p_vaddr < vaddr {
                return Err(ElfParserError::InvalidProgramHeader);
            }
            if program_header
                .p_offset
                .err_checked_add(program_header.p_filesz)? as usize
                > elf_bytes.len()
            {
                return Err(ElfParserError::OutOfBounds);
            }
            vaddr = program_header.p_vaddr;
        }

        let mut offset = 0usize;
        for section_header in section_header_table.iter() {
            if section_header.sh_type == SHT_NOBITS {
                continue;
            }
            let section_range = section_header.sh_offset as usize
                ..(section_header.sh_offset as usize)
                    .err_checked_add(section_header.sh_size as usize)?;
            check_that_there_is_no_overlap(&section_range, &file_header_range)?;
            check_that_there_is_no_overlap(&section_range, &program_header_table_range)?;
            check_that_there_is_no_overlap(&section_range, &section_header_table_range)?;
            if section_range.start < offset {
                return Err(ElfParserError::SectionNotInOrder);
            }
            offset = section_range.end;
            if offset > elf_bytes.len() {
                return Err(ElfParserError::OutOfBounds);
            }
        }

        let section_names_section_header = (file_header.e_shstrndx != SHN_UNDEF)
            .then(|| {
                section_header_table
                    .get(file_header.e_shstrndx as usize)
                    .ok_or(ElfParserError::OutOfBounds)
            })
            .transpose()?;

        let mut parser = Self {
            elf_bytes,
            file_header,
            program_header_table,
            section_header_table,
            section_names_section_header,
            symbol_section_header: None,
            symbol_names_section_header: None,
            dynamic_table: [0; DT_NUM],
            dynamic_relocations_table: None,
            dynamic_symbol_table: None,
            dynamic_symbol_names_section_header: None,
        };

        parser.parse_sections()?;
        parser.parse_dynamic()?;

        Ok(parser)
    }

    /// Returns the file header.
    pub fn file_header(&self) -> &Elf64Ehdr {
        self.file_header
    }

    /// Returns the program header table.
    pub fn program_header_table(&self) -> &[Elf64Phdr] {
        self.program_header_table
    }

    /// Returns the section header table.
    pub fn section_header_table(&self) -> &[Elf64Shdr] {
        self.section_header_table
    }

    /// Returns the dynamic symbol table.
    pub fn dynamic_symbol_table(&self) -> Option<&[Elf64Sym]> {
        self.dynamic_symbol_table
    }

    /// Returns the dynamic relocations table.
    pub fn dynamic_relocations_table(&self) -> Option<&[Elf64Rel]> {
        self.dynamic_relocations_table
    }

    /// Parses the file header.
    pub fn parse_file_header(
        elf_bytes: &'a [u8],
    ) -> Result<(core::ops::Range<usize>, &'a Elf64Ehdr), ElfParserError> {
        let file_header_range = 0..mem::size_of::<Elf64Ehdr>();
        let file_header_bytes = elf_bytes
            .get(file_header_range.clone())
            .ok_or(ElfParserError::OutOfBounds)?;
        let ptr = file_header_bytes.as_ptr();
        if (ptr as usize)
            .checked_rem(mem::align_of::<Elf64Ehdr>())
            .map(|remaining| remaining != 0)
            .unwrap_or(true)
        {
            return Err(ElfParserError::InvalidAlignment);
        }
        let file_header = unsafe { &*ptr.cast::<Elf64Ehdr>() };
        Ok((file_header_range, file_header))
    }

    /// Parses the program header table.
    pub fn parse_program_header_table(
        elf_bytes: &'a [u8],
        file_header_range: core::ops::Range<usize>,
        file_header: &Elf64Ehdr,
    ) -> Result<(core::ops::Range<usize>, &'a [Elf64Phdr]), ElfParserError> {
        let program_header_table_range = file_header.e_phoff as usize
            ..mem::size_of::<Elf64Phdr>()
                .err_checked_mul(file_header.e_phnum as usize)?
                .err_checked_add(file_header.e_phoff as usize)?;
        check_that_there_is_no_overlap(&file_header_range, &program_header_table_range)?;
        let program_header_table =
            Self::slice_from_bytes::<Elf64Phdr>(elf_bytes, program_header_table_range.clone())?;
        Ok((program_header_table_range, program_header_table))
    }

    /// Parses the section header table.
    pub fn parse_section_header_table(
        elf_bytes: &'a [u8],
        file_header_range: core::ops::Range<usize>,
        file_header: &Elf64Ehdr,
        program_header_table_range: core::ops::Range<usize>,
    ) -> Result<(core::ops::Range<usize>, &'a [Elf64Shdr]), ElfParserError> {
        let section_header_table_range = file_header.e_shoff as usize
            ..mem::size_of::<Elf64Shdr>()
                .err_checked_mul(file_header.e_shnum as usize)?
                .err_checked_add(file_header.e_shoff as usize)?;
        check_that_there_is_no_overlap(&file_header_range, &section_header_table_range)?;
        check_that_there_is_no_overlap(&program_header_table_range, &section_header_table_range)?;
        let section_header_table =
            Self::slice_from_bytes::<Elf64Shdr>(elf_bytes, section_header_table_range.clone())?;
        Ok((section_header_table_range, section_header_table))
    }

    fn parse_sections(&mut self) -> Result<(), ElfParserError> {
        macro_rules! section_header_by_name {
            ($self:expr, $section_header:expr, $section_name:expr,
             $($name:literal => $field:ident,)*) => {
                match $section_name {
                    $($name => {
                        if $self.$field.is_some() {
                            return Err(ElfParserError::InvalidSectionHeader);
                        }
                        $self.$field = Some($section_header);
                    })*
                    _ => {}
                }
            }
        }
        let section_names_section_header = self
            .section_names_section_header
            .ok_or(ElfParserError::NoSectionNameStringTable)?;
        for section_header in self.section_header_table.iter() {
            let section_name = Self::get_string_in_section(
                self.elf_bytes,
                section_names_section_header,
                section_header.sh_name,
                SECTION_NAME_LENGTH_MAXIMUM,
            )?;
            section_header_by_name!(
                self, section_header, section_name,
                b".symtab" => symbol_section_header,
                b".strtab" => symbol_names_section_header,
                b".dynstr" => dynamic_symbol_names_section_header,
            )
        }

        Ok(())
    }

    fn parse_dynamic(&mut self) -> Result<(), ElfParserError> {
        let mut dynamic_table: Option<&[Elf64Dyn]> = None;

        // try to parse PT_DYNAMIC
        if let Some(dynamic_program_header) = self
            .program_header_table
            .iter()
            .find(|program_header| program_header.p_type == PT_DYNAMIC)
        {
            dynamic_table =
                Self::slice_from_program_header(self.elf_bytes, dynamic_program_header).ok();
        }

        // if PT_DYNAMIC does not exist or is invalid (some of our tests have this),
        // fallback to parsing SHT_DYNAMIC
        if dynamic_table.is_none() {
            if let Some(dynamic_section_header) = self
                .section_header_table
                .iter()
                .find(|section_header| section_header.sh_type == SHT_DYNAMIC)
            {
                dynamic_table = Some(
                    Self::slice_from_section_header(self.elf_bytes, dynamic_section_header)
                        .map_err(|_| ElfParserError::InvalidDynamicSectionTable)?,
                );
            }
        }

        // if there are neither PT_DYNAMIC nor SHT_DYNAMIC, this is a static
        // file
        let dynamic_table = match dynamic_table {
            Some(table) => table,
            None => return Ok(()),
        };

        // expand Elf64Dyn entries into self.dynamic_table
        for dyn_info in dynamic_table {
            if dyn_info.d_tag == DT_NULL {
                break;
            }

            if dyn_info.d_tag as usize >= DT_NUM {
                // we don't parse any reserved tags
                continue;
            }
            self.dynamic_table[dyn_info.d_tag as usize] = dyn_info.d_val;
        }

        self.dynamic_relocations_table = self.parse_dynamic_relocations()?;
        self.dynamic_symbol_table = self.parse_dynamic_symbol_table()?;

        Ok(())
    }

    fn parse_dynamic_relocations(&mut self) -> Result<Option<&'a [Elf64Rel]>, ElfParserError> {
        let vaddr = self.dynamic_table[DT_REL as usize];
        if vaddr == 0 {
            return Ok(None);
        }

        if self.dynamic_table[DT_RELENT as usize] as usize != mem::size_of::<Elf64Rel>() {
            return Err(ElfParserError::InvalidDynamicSectionTable);
        }

        let size = self.dynamic_table[DT_RELSZ as usize] as usize;
        if size == 0 {
            return Err(ElfParserError::InvalidDynamicSectionTable);
        }

        let offset = if let Some(program_header) = self.program_header_for_vaddr(vaddr)? {
            vaddr
                .err_checked_sub(program_header.p_vaddr)?
                .err_checked_add(program_header.p_offset)?
        } else {
            // At least until rust-bpf-sysroot v0.13, we used to generate
            // invalid dynamic sections where the address of DT_REL was not
            // contained in any program segment. When loading one of those
            // files, fallback to relying on section headers.
            self.section_header_table
                .iter()
                .find(|section_header| section_header.sh_addr == vaddr)
                .ok_or(ElfParserError::InvalidDynamicSectionTable)?
                .sh_offset
        } as usize;

        Self::slice_from_bytes(self.elf_bytes, offset..offset.err_checked_add(size)?)
            .map(Some)
            .map_err(|_| ElfParserError::InvalidDynamicSectionTable)
    }

    fn parse_dynamic_symbol_table(&mut self) -> Result<Option<&'a [Elf64Sym]>, ElfParserError> {
        let vaddr = self.dynamic_table[DT_SYMTAB as usize];
        if vaddr == 0 {
            return Ok(None);
        }

        let dynsym_section_header = self
            .section_header_table
            .iter()
            .find(|section_header| section_header.sh_addr == vaddr)
            .ok_or(ElfParserError::InvalidDynamicSectionTable)?;

        self.get_symbol_table_of_section(dynsym_section_header)
            .map(Some)
    }

    /// Query a single string from a section which is marked as SHT_STRTAB
    pub fn get_string_in_section(
        elf_bytes: &'a [u8],
        section_header: &Elf64Shdr,
        offset_in_section: Elf64Word,
        maximum_length: usize,
    ) -> Result<&'a [u8], ElfParserError> {
        if section_header.sh_type != SHT_STRTAB {
            return Err(ElfParserError::InvalidSectionHeader);
        }
        let offset_in_file =
            (section_header.sh_offset as usize).err_checked_add(offset_in_section as usize)?;
        let string_range = offset_in_file
            ..(section_header.sh_offset as usize)
                .err_checked_add(section_header.sh_size as usize)?
                .min(offset_in_file.err_checked_add(maximum_length)?);
        let unterminated_string_bytes = elf_bytes
            .get(string_range)
            .ok_or(ElfParserError::OutOfBounds)?;
        unterminated_string_bytes
            .iter()
            .position(|byte| *byte == 0x00)
            .and_then(|string_length| unterminated_string_bytes.get(0..string_length))
            .ok_or_else(|| {
                ElfParserError::StringTooLong(
                    String::from_utf8_lossy(unterminated_string_bytes).to_string(),
                    maximum_length,
                )
            })
    }

    /// Returns the string corresponding to the given `sh_name`
    pub fn section_name(&self, sh_name: Elf64Word) -> Result<&'a [u8], ElfParserError> {
        Self::get_string_in_section(
            self.elf_bytes,
            self.section_names_section_header
                .ok_or(ElfParserError::NoSectionNameStringTable)?,
            sh_name,
            SECTION_NAME_LENGTH_MAXIMUM,
        )
    }

    /// Returns the name of the `st_name` symbol
    pub fn symbol_name(&self, st_name: Elf64Word) -> Result<&'a [u8], ElfParserError> {
        Self::get_string_in_section(
            self.elf_bytes,
            self.symbol_names_section_header
                .ok_or(ElfParserError::NoStringTable)?,
            st_name,
            SYMBOL_NAME_LENGTH_MAXIMUM,
        )
    }

    /// Returns the symbol table
    pub fn symbol_table(&self) -> Result<Option<&'a [Elf64Sym]>, ElfParserError> {
        self.symbol_section_header
            .map(|section_header| self.get_symbol_table_of_section(section_header))
            .transpose()
    }

    /// Returns the name of the `st_name` dynamic symbol
    pub fn dynamic_symbol_name(&self, st_name: Elf64Word) -> Result<&'a [u8], ElfParserError> {
        Self::get_string_in_section(
            self.elf_bytes,
            self.dynamic_symbol_names_section_header
                .ok_or(ElfParserError::NoDynamicStringTable)?,
            st_name,
            SYMBOL_NAME_LENGTH_MAXIMUM,
        )
    }

    /// Returns the symbol table of a section which is marked as SHT_SYMTAB
    pub fn get_symbol_table_of_section(
        &self,
        section_header: &Elf64Shdr,
    ) -> Result<&'a [Elf64Sym], ElfParserError> {
        if section_header.sh_type != SHT_SYMTAB && section_header.sh_type != SHT_DYNSYM {
            return Err(ElfParserError::InvalidSectionHeader);
        }

        Self::slice_from_section_header(self.elf_bytes, section_header)
    }

    /// Returns the `&[T]` contained in the data described by the given program
    /// header
    pub fn slice_from_program_header<T: 'static>(
        bytes: &'a [u8],
        &Elf64Phdr {
            p_offset, p_filesz, ..
        }: &Elf64Phdr,
    ) -> Result<&'a [T], ElfParserError> {
        Self::slice_from_bytes(
            bytes,
            (p_offset as usize)..(p_offset as usize).err_checked_add(p_filesz as usize)?,
        )
    }

    /// Returns the `&[T]` contained in the section data described by the given
    /// section header
    pub fn slice_from_section_header<T: 'static>(
        bytes: &'a [u8],
        &Elf64Shdr {
            sh_offset, sh_size, ..
        }: &Elf64Shdr,
    ) -> Result<&'a [T], ElfParserError> {
        Self::slice_from_bytes(
            bytes,
            (sh_offset as usize)..(sh_offset as usize).err_checked_add(sh_size as usize)?,
        )
    }

    /// Returns the `&[T]` contained at `bytes[range]`
    pub fn slice_from_bytes<T: 'static>(
        bytes: &[u8],
        range: Range<usize>,
    ) -> Result<&[T], ElfParserError> {
        if range
            .len()
            .checked_rem(mem::size_of::<T>())
            .map(|remainder| remainder != 0)
            .unwrap_or(true)
        {
            return Err(ElfParserError::InvalidSize);
        }

        let bytes = bytes
            .get(range.clone())
            .ok_or(ElfParserError::OutOfBounds)?;

        let ptr = bytes.as_ptr();
        if (ptr as usize)
            .checked_rem(mem::align_of::<T>())
            .map(|remaining| remaining != 0)
            .unwrap_or(true)
        {
            return Err(ElfParserError::InvalidAlignment);
        }

        Ok(unsafe {
            slice::from_raw_parts(
                ptr.cast(),
                range.len().checked_div(mem::size_of::<T>()).unwrap_or(0),
            )
        })
    }

    fn program_header_for_vaddr(
        &self,
        vaddr: Elf64Addr,
    ) -> Result<Option<&'a Elf64Phdr>, ElfParserError> {
        for program_header in self.program_header_table.iter() {
            let Elf64Phdr {
                p_vaddr, p_memsz, ..
            } = program_header;

            if (*p_vaddr..p_vaddr.err_checked_add(*p_memsz)?).contains(&vaddr) {
                return Ok(Some(program_header));
            }
        }
        Ok(None)
    }
}

impl fmt::Debug for Elf64<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "{:#X?}", self.file_header)?;
        for program_header in self.program_header_table.iter() {
            writeln!(f, "{program_header:#X?}")?;
        }
        for section_header in self.section_header_table.iter() {
            let section_name = Self::get_string_in_section(
                self.elf_bytes,
                self.section_names_section_header.unwrap(),
                section_header.sh_name,
                SECTION_NAME_LENGTH_MAXIMUM,
            )
            .and_then(|name| core::str::from_utf8(name).map_err(|_| ElfParserError::InvalidString))
            .unwrap();
            writeln!(f, "{section_name}")?;
            writeln!(f, "{section_header:#X?}")?;
        }
        if let Some(section_header) = self.symbol_section_header {
            let symbol_table = self.get_symbol_table_of_section(section_header).unwrap();
            writeln!(f, "{symbol_table:#X?}")?;
            for symbol in symbol_table.iter() {
                if symbol.st_name != 0 {
                    let symbol_name = Self::get_string_in_section(
                        self.elf_bytes,
                        self.symbol_names_section_header.unwrap(),
                        symbol.st_name,
                        SYMBOL_NAME_LENGTH_MAXIMUM,
                    )
                    .and_then(|name| {
                        core::str::from_utf8(name).map_err(|_| ElfParserError::InvalidString)
                    })
                    .unwrap();
                    writeln!(f, "{symbol_name}")?;
                }
            }
        }
        Ok(())
    }
}

impl From<ArithmeticOverflow> for ElfParserError {
    fn from(_: ArithmeticOverflow) -> ElfParserError {
        ElfParserError::OutOfBounds
    }
}
