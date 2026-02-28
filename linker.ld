/* Linker script for ospab.os v2 (AETERNA) - Limine boot protocol */

ENTRY(_start)

KERNEL_PHYS = 0x0000000000200000;
KERNEL_VIRT = 0xffffffff80200000;

PHDRS
{
    text    PT_LOAD FLAGS(5);
    rodata  PT_LOAD FLAGS(4);
    data    PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = KERNEL_VIRT;

    .text ALIGN(4K) : AT(ADDR(.text) - KERNEL_VIRT + KERNEL_PHYS)
    {
        *(.text .text.*)
    } :text

    .limine_requests ALIGN(8) : AT(ADDR(.limine_requests) - KERNEL_VIRT + KERNEL_PHYS)
    {
        KEEP(*(.limine_requests_start))
        KEEP(*(.limine_requests))
        KEEP(*(.limine_requests_end))
    } :text

    .rodata ALIGN(4K) : AT(ADDR(.rodata) - KERNEL_VIRT + KERNEL_PHYS)
    {
        *(.rodata .rodata.*)
    } :rodata

    .data ALIGN(4K) : AT(ADDR(.data) - KERNEL_VIRT + KERNEL_PHYS)
    {
        *(.data .data.*)
    } :data

    .bss ALIGN(4K) : AT(ADDR(.bss) - KERNEL_VIRT + KERNEL_PHYS)
    {
        *(COMMON)
        *(.bss .bss.*)
    } :data

    /DISCARD/ :
    {
        *(.eh_frame*)
        *(.note .note.*)
        *(.comment)
    }

    _kernel_start = KERNEL_VIRT;
    _kernel_end = .;
    _kernel_phys_start = KERNEL_PHYS;
}
