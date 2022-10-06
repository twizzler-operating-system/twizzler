__qemu_ram_start_addr = 0x40000000;

__qemu_kernel_ram_addr = 0x40080000;

ENTRY(_start)
SECTIONS {
    . = __qemu_ram_start_addr;

    .text : { 
        KEEP(*(.text._start))
        *(.text)
    }

    .rodata : { *(.rodata*) }

    .data : { *(.data) }

    .bss : { *(.bss) }
    . = ALIGN(8);
    
    . += 0x4000;
    __stack_top = .;
}
