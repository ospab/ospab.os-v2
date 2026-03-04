/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Limine boot protocol (from ospab.os v1).
*/
use core::ffi::c_char;
use core::ptr;

const LIMINE_COMMON_MAGIC: [u64; 2] = [0xc7b1dd30df4c8b88, 0x0a82e883a194f07b];

#[used]
#[link_section = ".limine_requests_start"]
static LIMINE_REQUESTS_START: [u64; 4] = [
    0xf6b8f4b39de7d1ae,
    0xfab91a6940fcb9cf,
    0x785c6ed015d3e316,
    0x181e920a7852b9d9,
];

#[used]
#[link_section = ".limine_requests_end"]
static LIMINE_REQUESTS_END: [u64; 2] = [0xadc0e0531bb10d03, 0x9572709f31764c62];

#[used]
#[link_section = ".limine_requests"]
static mut BASE_REVISION: [u64; 3] = [
    0xf9562b2d5c95a6c8,
    0x6a7b384944536bdc,
    2,
];

pub fn base_revision_supported() -> bool {
    unsafe { BASE_REVISION[2] == 0 }
}

pub fn get_base_revision_raw() -> u64 {
    unsafe { BASE_REVISION[2] }
}

#[repr(C)]
pub struct BootloaderInfoResponse {
    pub revision: u64,
    pub name: *const c_char,
    pub version: *const c_char,
}

#[repr(C)]
pub struct BootloaderInfoRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *mut BootloaderInfoResponse,
}

unsafe impl Sync for BootloaderInfoRequest {}

#[used]
#[link_section = ".limine_requests"]
static mut BOOTLOADER_INFO_REQUEST: BootloaderInfoRequest = BootloaderInfoRequest {
    id: [
        LIMINE_COMMON_MAGIC[0],
        LIMINE_COMMON_MAGIC[1],
        0xf55038d8e2a1202f,
        0x279426fcf5f59740,
    ],
    revision: 0,
    response: ptr::null_mut(),
};

pub fn bootloader_info() -> Option<&'static BootloaderInfoResponse> {
    unsafe {
        if BOOTLOADER_INFO_REQUEST.response.is_null() {
            None
        } else {
            Some(&*BOOTLOADER_INFO_REQUEST.response)
        }
    }
}

#[repr(C)]
pub struct HhdmResponse {
    pub revision: u64,
    pub offset: u64,
}

#[repr(C)]
pub struct HhdmRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *mut HhdmResponse,
}

unsafe impl Sync for HhdmRequest {}

#[used]
#[link_section = ".limine_requests"]
static mut HHDM_REQUEST: HhdmRequest = HhdmRequest {
    id: [
        LIMINE_COMMON_MAGIC[0],
        LIMINE_COMMON_MAGIC[1],
        0x48dcf1cb8ad2b852,
        0x63984e959a98244b,
    ],
    revision: 0,
    response: ptr::null_mut(),
};

pub fn hhdm_offset() -> Option<u64> {
    unsafe {
        if HHDM_REQUEST.response.is_null() {
            None
        } else {
            Some((*HHDM_REQUEST.response).offset)
        }
    }
}

#[repr(C)]
pub struct Framebuffer {
    pub address: *mut u8,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bpp: u16,
    pub memory_model: u8,
    pub red_mask_size: u8,
    pub red_mask_shift: u8,
    pub green_mask_size: u8,
    pub green_mask_shift: u8,
    pub blue_mask_size: u8,
    pub blue_mask_shift: u8,
    pub unused: [u8; 7],
    pub edid_size: u64,
    pub edid: *mut u8,
    pub mode_count: u64,
    pub modes: *mut *mut VideoMode,
}

#[repr(C)]
pub struct VideoMode {
    pub pitch: u64,
    pub width: u64,
    pub height: u64,
    pub bpp: u16,
    pub memory_model: u8,
    pub red_mask_size: u8,
    pub red_mask_shift: u8,
    pub green_mask_size: u8,
    pub green_mask_shift: u8,
    pub blue_mask_size: u8,
    pub blue_mask_shift: u8,
}

#[repr(C)]
pub struct FramebufferResponse {
    pub revision: u64,
    pub framebuffer_count: u64,
    pub framebuffers: *mut *mut Framebuffer,
}

#[repr(C)]
pub struct FramebufferRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *mut FramebufferResponse,
}

unsafe impl Sync for FramebufferRequest {}

#[used]
#[link_section = ".limine_requests"]
static mut FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest {
    id: [
        LIMINE_COMMON_MAGIC[0],
        LIMINE_COMMON_MAGIC[1],
        0x9d5827dcd881dd75,
        0xa3148604f6fab11b,
    ],
    revision: 0,
    response: ptr::null_mut(),
};

pub fn framebuffer() -> Option<&'static Framebuffer> {
    unsafe {
        if FRAMEBUFFER_REQUEST.response.is_null() {
            return None;
        }
        let resp = &*FRAMEBUFFER_REQUEST.response;
        if resp.framebuffer_count == 0 || resp.framebuffers.is_null() {
            return None;
        }
        let fb_ptr = *resp.framebuffers;
        if fb_ptr.is_null() {
            None
        } else {
            Some(&*fb_ptr)
        }
    }
}

pub const MEMMAP_USABLE: u64 = 0;
pub const MEMMAP_RESERVED: u64 = 1;
pub const MEMMAP_ACPI_RECLAIMABLE: u64 = 2;
pub const MEMMAP_ACPI_NVS: u64 = 3;
pub const MEMMAP_BAD_MEMORY: u64 = 4;
pub const MEMMAP_BOOTLOADER_RECLAIMABLE: u64 = 5;
pub const MEMMAP_KERNEL_AND_MODULES: u64 = 6;
pub const MEMMAP_FRAMEBUFFER: u64 = 7;

#[repr(C)]
pub struct MemmapEntry {
    pub base: u64,
    pub length: u64,
    pub typ: u64,
}

#[repr(C)]
pub struct MemmapResponse {
    pub revision: u64,
    pub entry_count: u64,
    pub entries: *mut *mut MemmapEntry,
}

#[repr(C)]
pub struct MemmapRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *mut MemmapResponse,
}

unsafe impl Sync for MemmapRequest {}

#[used]
#[link_section = ".limine_requests"]
static mut MEMMAP_REQUEST: MemmapRequest = MemmapRequest {
    id: [
        LIMINE_COMMON_MAGIC[0],
        LIMINE_COMMON_MAGIC[1],
        0x67cf3d9d378a806f,
        0xe304acdfc50c3c62,
    ],
    revision: 0,
    response: ptr::null_mut(),
};

pub struct MemmapIterator {
    entries: *mut *mut MemmapEntry,
    count: usize,
    index: usize,
}

impl Iterator for MemmapIterator {
    type Item = &'static MemmapEntry;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.count {
            return None;
        }
        unsafe {
            let entry_ptr = *self.entries.add(self.index);
            self.index += 1;
            if entry_ptr.is_null() {
                None
            } else {
                Some(&*entry_ptr)
            }
        }
    }
}

pub fn memory_map() -> Option<MemmapIterator> {
    unsafe {
        if MEMMAP_REQUEST.response.is_null() {
            return None;
        }
        let resp = &*MEMMAP_REQUEST.response;
        if resp.entry_count == 0 || resp.entries.is_null() {
            return None;
        }
        Some(MemmapIterator {
            entries: resp.entries,
            count: resp.entry_count as usize,
            index: 0,
        })
    }
}

#[repr(C)]
pub struct LimineFile {
    pub revision: u64,
    pub address: *mut u8,
    pub size: u64,
    pub path: *const c_char,
    pub cmdline: *const c_char,
    pub media_type: u32,
    pub unused: u32,
    pub tftp_ip: u32,
    pub tftp_port: u32,
    pub partition_index: u32,
    pub mbr_disk_id: u32,
    pub gpt_disk_uuid: [u8; 16],
    pub gpt_part_uuid: [u8; 16],
    pub part_uuid: [u8; 16],
}

#[repr(C)]
pub struct ModuleResponse {
    pub revision: u64,
    pub module_count: u64,
    pub modules: *mut *mut LimineFile,
}

#[repr(C)]
pub struct ModuleRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *mut ModuleResponse,
}

unsafe impl Sync for ModuleRequest {}

#[used]
#[link_section = ".limine_requests"]
static mut MODULE_REQUEST: ModuleRequest = ModuleRequest {
    id: [
        LIMINE_COMMON_MAGIC[0],
        LIMINE_COMMON_MAGIC[1],
        0x3e7e279702be32af,
        0xca1c4f3bd1280cee,
    ],
    revision: 0,
    response: ptr::null_mut(),
};

pub struct ModuleIterator {
    modules: *mut *mut LimineFile,
    count: usize,
    index: usize,
}

impl Iterator for ModuleIterator {
    type Item = &'static LimineFile;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.count {
            return None;
        }
        unsafe {
            let module_ptr = *self.modules.add(self.index);
            self.index += 1;
            if module_ptr.is_null() {
                None
            } else {
                Some(&*module_ptr)
            }
        }
    }
}

pub fn modules() -> Option<ModuleIterator> {
    unsafe {
        if MODULE_REQUEST.response.is_null() {
            return None;
        }
        let resp = &*MODULE_REQUEST.response;
        if resp.module_count == 0 || resp.modules.is_null() {
            return None;
        }
        Some(ModuleIterator {
            modules: resp.modules,
            count: resp.module_count as usize,
            index: 0,
        })
    }
}

// ─── RSDP (ACPI) request ────────────────────────────────────────────────────

#[repr(C)]
pub struct RsdpResponse {
    pub revision: u64,
    /// Physical address of the RSDP structure
    pub address: *const u8,
}

#[repr(C)]
pub struct RsdpRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *mut RsdpResponse,
}

unsafe impl Sync for RsdpRequest {}

#[used]
#[link_section = ".limine_requests"]
static mut RSDP_REQUEST: RsdpRequest = RsdpRequest {
    id: [
        LIMINE_COMMON_MAGIC[0],
        LIMINE_COMMON_MAGIC[1],
        0xc5e77b6b397e7b43,
        0x27637845accdcf3c,
    ],
    revision: 0,
    response: ptr::null_mut(),
};

/// Get the RSDP physical address from the bootloader.
pub fn rsdp_address() -> Option<*const u8> {
    unsafe {
        if RSDP_REQUEST.response.is_null() {
            None
        } else {
            let addr = (*RSDP_REQUEST.response).address;
            if addr.is_null() { None } else { Some(addr) }
        }
    }
}

// ─── Kernel file request (gives access to kernel cmdline) ────────────────────

#[repr(C)]
pub struct KernelFileResponse {
    pub revision: u64,
    pub kernel_file: *mut LimineFile,
}

#[repr(C)]
pub struct KernelFileRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *mut KernelFileResponse,
}

unsafe impl Sync for KernelFileRequest {}

#[used]
#[link_section = ".limine_requests"]
static mut KERNEL_FILE_REQUEST: KernelFileRequest = KernelFileRequest {
    id: [
        LIMINE_COMMON_MAGIC[0],
        LIMINE_COMMON_MAGIC[1],
        0xad97e90e83f1ed67,
        0x31eb5d1c5ff23b69,
    ],
    revision: 0,
    response: ptr::null_mut(),
};

/// Read the kernel command line provided by Limine (from the boot entry `cmdline:` field).
/// Returns a `&'static str` slice. Empty string if not available.
pub fn kernel_cmdline() -> &'static str {
    unsafe {
        if KERNEL_FILE_REQUEST.response.is_null() { return ""; }
        let resp = &*KERNEL_FILE_REQUEST.response;
        if resp.kernel_file.is_null() { return ""; }
        let file = &*resp.kernel_file;
        if file.cmdline.is_null() { return ""; }
        // Convert C string to Rust str
        let mut len = 0usize;
        let ptr = file.cmdline as *const u8;
        while *ptr.add(len) != 0 { len += 1; }
        match core::str::from_utf8(core::slice::from_raw_parts(ptr, len)) {
            Ok(s) => s,
            Err(_) => "",
        }
    }
}

/// Check if "key=VALUE" or bare "key" is present in the kernel cmdline.
pub fn cmdline_has(key: &str) -> bool {
    for tok in kernel_cmdline().split_whitespace() {
        if tok == key { return true; }
        // Check tok starts with "key="
        let kb = key.as_bytes();
        let tb = tok.as_bytes();
        if tb.len() > kb.len() && &tb[..kb.len()] == kb && tb[kb.len()] == b'=' {
            return true;
        }
    }
    false
}

/// Get value for key= in the kernel cmdline. Returns "" if not found.
pub fn cmdline_get<'a>(key: &str) -> &'a str {
    let cmdline = kernel_cmdline();
    for tok in cmdline.split_whitespace() {
        if tok.starts_with(key) && tok.as_bytes().get(key.len()) == Some(&b'=') {
            return unsafe {
                // Safety: cmdline is 'static, so the slice is too
                let s = &tok[key.len() + 1..];
                core::mem::transmute::<&str, &'a str>(s)
            };
        }
    }
    ""
}

pub fn get_module(index: usize) -> Option<&'static LimineFile> {
    unsafe {
        if MODULE_REQUEST.response.is_null() {
            return None;
        }
        let resp = &*MODULE_REQUEST.response;
        if index >= resp.module_count as usize || resp.modules.is_null() {
            return None;
        }
        let module_ptr = *resp.modules.add(index);
        if module_ptr.is_null() {
            None
        } else {
            Some(&*module_ptr)
        }
    }
}

pub fn module_count() -> usize {
    unsafe {
        if MODULE_REQUEST.response.is_null() {
            return 0;
        }
        let resp = &*MODULE_REQUEST.response;
        resp.module_count as usize
    }
}

// ─── Executable Address (kernel physical/virtual base) ──────────────────────

#[repr(C)]
pub struct ExecutableAddressResponse {
    pub revision: u64,
    pub physical_base: u64,
    pub virtual_base: u64,
}

#[repr(C)]
pub struct ExecutableAddressRequest {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: *mut ExecutableAddressResponse,
}

unsafe impl Sync for ExecutableAddressRequest {}

#[used]
#[link_section = ".limine_requests"]
static mut EXECUTABLE_ADDRESS_REQUEST: ExecutableAddressRequest = ExecutableAddressRequest {
    id: [
        LIMINE_COMMON_MAGIC[0],
        LIMINE_COMMON_MAGIC[1],
        0x71ba76863cc55f63,
        0xb2644a48c516a487,
    ],
    revision: 0,
    response: ptr::null_mut(),
};

/// Get the physical base address where the kernel was actually loaded.
/// This may differ from the linker script's KERNEL_PHYS if Limine slides
/// the kernel (KASLR or relocation).
pub fn kernel_phys_base() -> u64 {
    unsafe {
        if EXECUTABLE_ADDRESS_REQUEST.response.is_null() {
            0x200000 // fallback to linker script default
        } else {
            (*EXECUTABLE_ADDRESS_REQUEST.response).physical_base
        }
    }
}

/// Get the virtual base address of the kernel (should match linker script).
pub fn kernel_virt_base() -> u64 {
    unsafe {
        if EXECUTABLE_ADDRESS_REQUEST.response.is_null() {
            0xFFFF_FFFF_8020_0000 // fallback to linker script default
        } else {
            (*EXECUTABLE_ADDRESS_REQUEST.response).virtual_base
        }
    }
}

/// Compute the offset to subtract from a kernel virtual address to get physical.
/// `phys = virt - kernel_virt_offset()`
///
/// Previously hardcoded as `0xFFFF_FFFF_8000_0000` which assumed KERNEL_PHYS=0x200000.
/// Now dynamically computed from Limine's actual load address.
#[inline]
pub fn kernel_virt_offset() -> u64 {
    kernel_virt_base() - kernel_phys_base()
}
