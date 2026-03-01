/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
GDT + TSS for double-fault stack (from ospab.os v1).
*/
use spin::Lazy;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 1;
const STACK_SIZE: usize = 4096 * 5;

#[repr(C, align(4096))]
struct Stack {
    data: [u8; STACK_SIZE],
}

static DOUBLE_FAULT_STACK: Stack = Stack { data: [0; STACK_SIZE] };

static TSS: Lazy<TaskStateSegment> = Lazy::new(|| {
    let mut tss = TaskStateSegment::new();
    let stack_start = VirtAddr::from_ptr(&DOUBLE_FAULT_STACK);
    let stack_end = stack_start + STACK_SIZE as u64;
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = stack_end;
    tss
});

static GDT: Lazy<(GlobalDescriptorTable, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let code_selector = gdt.add_entry(Descriptor::kernel_code_segment());
    let data_selector = gdt.add_entry(Descriptor::kernel_data_segment());
    let tss_selector = gdt.add_entry(Descriptor::tss_segment(&TSS));
    (gdt, Selectors { code_selector, data_selector, tss_selector })
});

struct Selectors {
    code_selector: SegmentSelector,
    data_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

pub fn init() {
    use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
    use x86_64::instructions::tables::load_tss;

    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        SS::set_reg(GDT.1.data_selector);
        DS::set_reg(GDT.1.data_selector);
        ES::set_reg(GDT.1.data_selector);
        load_tss(GDT.1.tss_selector);
    }
}
