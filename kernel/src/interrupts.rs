//! Interrupt descriptor table, exception handlers, 8259 PIC and PIT timer.
//!
//! The timer, system calls and the faults user code can raise go through the
//! assembly path in `trap.rs` (they may switch tasks). The rest keep
//! `x86-interrupt` handlers. Handlers must not allocate or take locks that
//! non-interrupt code may hold with interrupts enabled. The only lock taken
//! here (PICS) is held by other code only while interrupts are disabled.

use crate::cpu;
use crate::gdt;
use crate::keyboard;
use crate::kprintln;
use crate::trap::{self, TrapFrame};
use core::sync::atomic::{AtomicU64, Ordering};
use pic8259::ChainedPics;
use spin::{LazyLock, Mutex};
use x86_64::PrivilegeLevel;
use x86_64::VirtAddr;
use x86_64::instructions::port::Port;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = 40;
const TIMER_VECTOR: u8 = PIC_1_OFFSET; // IRQ0
const KEYBOARD_VECTOR: u8 = PIC_1_OFFSET + 1; // IRQ1
const SPURIOUS_MASTER_VECTOR: u8 = PIC_1_OFFSET + 7; // IRQ7
const MOUSE_VECTOR: u8 = PIC_2_OFFSET + 4; // IRQ12
const SPURIOUS_SLAVE_VECTOR: u8 = PIC_2_OFFSET + 7; // IRQ15
const _: () = assert!(KEYBOARD_VECTOR as u64 == trap::VEC_KEYBOARD);
const _: () = assert!(MOUSE_VECTOR as u64 == trap::VEC_MOUSE);

/// PIT input clock (Hz) and the divisor giving ~100 Hz.
const PIT_HZ: u32 = 1_193_182;
pub const TIMER_HZ: u32 = 100;
const PIT_DIVISOR: u16 = ((PIT_HZ + TIMER_HZ / 2) / TIMER_HZ) as u16; // 11932

// SAFETY: the offsets 32..48 do not overlap CPU exception vectors (0..32),
// which is the requirement of `ChainedPics::new`.
static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

static TICKS: AtomicU64 = AtomicU64::new(0);
static BREAKPOINTS: AtomicU64 = AtomicU64::new(0);
/// Lowest mapped address of the kernel stack; the page below it is the guard.
static STACK_BOTTOM: AtomicU64 = AtomicU64::new(0);

static IDT: LazyLock<InterruptDescriptorTable> = LazyLock::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.breakpoint.set_handler_fn(breakpoint);
    idt.segment_not_present.set_handler_fn(segment_not_present);
    idt.alignment_check.set_handler_fn(alignment_check);
    idt.machine_check.set_handler_fn(machine_check);
    // SAFETY: the IST index refers to the double-fault stack installed in the
    // TSS by `gdt::init`, which is not used by any other vector.
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
    }
    let addr = |f: unsafe extern "C" fn()| VirtAddr::new(f as usize as u64);
    // SAFETY: each address is a `trap.rs` stub, which builds a `TrapFrame`
    // matching what the CPU pushes for that vector (dummy error code where
    // the CPU pushes none) and never returns except through `iretq`.
    unsafe {
        idt.divide_error.set_handler_addr(addr(trap::stub_divide));
        idt.invalid_opcode
            .set_handler_addr(addr(trap::stub_invalid_opcode));
        idt.stack_segment_fault
            .set_handler_addr(addr(trap::stub_stack_segment));
        idt.general_protection_fault
            .set_handler_addr(addr(trap::stub_general_protection));
        idt.page_fault.set_handler_addr(addr(trap::stub_page_fault));
        idt.x87_floating_point
            .set_handler_addr(addr(trap::stub_x87));
        idt.simd_floating_point
            .set_handler_addr(addr(trap::stub_simd));
        idt[TIMER_VECTOR].set_handler_addr(addr(trap::stub_timer));
        idt[KEYBOARD_VECTOR].set_handler_addr(addr(trap::stub_keyboard));
        idt[MOUSE_VECTOR].set_handler_addr(addr(trap::stub_mouse));
        idt[trap::VEC_XHCI as u8].set_handler_addr(addr(trap::stub_xhci));
        // Ring 0 only: `int 0x81` from user code raises #GP instead.
        idt[trap::VEC_KERNEL_YIELD as u8].set_handler_addr(addr(trap::stub_kernel_yield));
        // The only gate user code may invoke directly.
        idt[trap::VEC_SYSCALL as u8]
            .set_handler_addr(addr(trap::stub_syscall))
            .set_privilege_level(PrivilegeLevel::Ring3);
    }
    idt[SPURIOUS_MASTER_VECTOR].set_handler_fn(spurious_master);
    idt[SPURIOUS_SLAVE_VECTOR].set_handler_fn(spurious_slave);
    idt[crate::apic::SPURIOUS_VECTOR].set_handler_fn(spurious_apic);
    idt
});

/// Loads the IDT. Requires `gdt::init` first (the double-fault IST).
/// Loads the IDT on a non-boot processor (docs/smp-design.md): one IDT
/// for every processor.
pub fn load_idt() {
    IDT.load();
}

pub fn init_idt(kernel_stack_bottom: u64) {
    STACK_BOTTOM.store(kernel_stack_bottom, Ordering::Relaxed);
    IDT.load();
}

/// IRQ0 (the PIT) in the master PIC's mask: masked while the local APIC
/// timer is the tick source.
fn irq0_mask() -> u8 {
    u8::from(crate::apic::active())
}

/// Remaps the PICs, then starts the tick at 100 Hz: the local APIC timer
/// when it can be calibrated (docs/h3-design.md), else the PIT on IRQ0.
/// Interrupts must still be disabled; the caller enables them afterwards.
pub fn init_timer(mem: &mut crate::memory::Memory) {
    let mut pics = PICS.lock();
    // SAFETY: interrupts are disabled and this is the only PIC user. The
    // remapped vectors have handlers in the IDT; every line is masked.
    unsafe {
        pics.initialize();
        pics.write_masks(0b1111_1111, 0b1111_1111);
    }
    drop(pics);
    match crate::apic::init(mem, TIMER_VECTOR) {
        Ok((per_tick, port)) => {
            kprintln!(
                "ZOO_OS INFO timer source=lapic per-tick={} pm-timer={:#x}",
                per_tick,
                port
            );
            return;
        }
        Err(why) => kprintln!("ZOO_OS INFO timer source=pit reason={}", why),
    }
    // SAFETY: as above; only IRQ0 (the timer) is opened.
    unsafe { PICS.lock().write_masks(0b1111_1110, 0b1111_1111) };
    // SAFETY: ports 0x43/0x40 are the PIT command and channel-0 data ports.
    // Command 0x34: channel 0, low then high byte, mode 2 (rate generator).
    unsafe {
        Port::<u8>::new(0x43).write(0x34);
        Port::<u8>::new(0x40).write((PIT_DIVISOR & 0xFF) as u8);
        Port::<u8>::new(0x40).write((PIT_DIVISOR >> 8) as u8);
    }
}

/// Drains stale keyboard bytes and unmasks IRQ1. Safe to call with
/// interrupts on: the PIC lock is taken with interrupts disabled, so the
/// timer handler cannot spin on it.
pub fn enable_keyboard() -> usize {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let drained = keyboard::drain_controller();
        let mouse = crate::mouse::init();
        crate::serial_println!("ZOO_OS INFO mouse ps2={}", mouse);
        // SAFETY: IRQ0 (timer, unless the local APIC drives the tick),
        // IRQ1 (keyboard), IRQ2 (the slave PIC) and IRQ12 (mouse) are
        // opened; every one has a handler in the IDT.
        unsafe {
            if mouse {
                PICS.lock()
                    .write_masks(0b1111_1000 | irq0_mask(), 0b1110_1111)
            } else {
                PICS.lock()
                    .write_masks(0b1111_1100 | irq0_mask(), 0b1111_1111)
            }
        };
        drained
    })
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn breakpoints() -> u64 {
    BREAKPOINTS.load(Ordering::Relaxed)
}

// ---- recoverable --------------------------------------------------------

extern "x86-interrupt" fn breakpoint(_frame: InterruptStackFrame) {
    BREAKPOINTS.fetch_add(1, Ordering::Relaxed);
}

/// Counts a tick and acknowledges IRQ0. Called from `trap_dispatch`.
pub fn on_timer_tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    crate::xhci::poll();
    crate::serial::pump();
    crate::console::render();
    if crate::apic::active() {
        crate::apic::eoi();
        return;
    }
    // SAFETY: TIMER_VECTOR is the vector the PIC raised. Main code only
    // holds PICS with interrupts disabled, so this lock cannot deadlock.
    unsafe { PICS.lock().notify_end_of_interrupt(TIMER_VECTOR) };
}

/// Local APIC spurious interrupt: no EOI is sent for it.
extern "x86-interrupt" fn spurious_apic(_frame: InterruptStackFrame) {}

/// IRQ12. Called from `trap_dispatch`, which then wakes input waiters.
pub fn on_mouse_irq() {
    crate::mouse::on_interrupt();
    // SAFETY: MOUSE_VECTOR is the vector the slave PIC raised; same lock
    // rule as the timer handler. The EOI goes to both PICs.
    unsafe { PICS.lock().notify_end_of_interrupt(MOUSE_VECTOR) };
}

/// IRQ1. Called from `trap_dispatch`, which then wakes input waiters.
pub fn on_keyboard_irq() {
    keyboard::on_interrupt();
    // SAFETY: KEYBOARD_VECTOR is the vector the PIC raised; same lock rule
    // as the timer handler.
    unsafe { PICS.lock().notify_end_of_interrupt(KEYBOARD_VECTOR) };
}

/// Reads a PIC's in-service register (OCW3 0x0B, then read command port).
fn pic_in_service(command_port: u16) -> u8 {
    // SAFETY: 0x20/0xA0 are the PIC command ports; OCW3 only selects which
    // register the next read returns.
    unsafe {
        let mut port = Port::<u8>::new(command_port);
        port.write(0x0B);
        port.read()
    }
}

extern "x86-interrupt" fn spurious_master(_frame: InterruptStackFrame) {
    // A real IRQ7 would be in service; it is masked, so treat any other case
    // as spurious, which must not be acknowledged.
    if pic_in_service(0x20) & 0x80 != 0 {
        // SAFETY: acknowledging an IRQ that is genuinely in service.
        unsafe { PICS.lock().notify_end_of_interrupt(SPURIOUS_MASTER_VECTOR) };
    }
}

extern "x86-interrupt" fn spurious_slave(_frame: InterruptStackFrame) {
    if pic_in_service(0xA0) & 0x80 != 0 {
        // SAFETY: acknowledging an IRQ that is genuinely in service.
        unsafe { PICS.lock().notify_end_of_interrupt(SPURIOUS_SLAVE_VECTOR) };
    } else {
        // Spurious from the slave: the master still saw IRQ2 and needs an EOI.
        // SAFETY: plain EOI command to the master PIC command port.
        unsafe { Port::<u8>::new(0x20).write(0x20) };
    }
}

// ---- fatal --------------------------------------------------------------

fn fatal(name: &str, frame: &InterruptStackFrame, error: Option<u64>) -> ! {
    kprintln!("ZOO_OS EXCEPTION {}", name);
    details(frame, error);
    cpu::halt_forever()
}

fn details(frame: &InterruptStackFrame, error: Option<u64>) {
    kprintln!(
        "ZOO_OS INFO exception rip={:#018x} rsp={:#018x} error={:#x}",
        frame.instruction_pointer.as_u64(),
        frame.stack_pointer.as_u64(),
        error.unwrap_or(0)
    );
}

extern "x86-interrupt" fn segment_not_present(frame: InterruptStackFrame, error: u64) {
    fatal("segment-not-present", &frame, Some(error))
}

extern "x86-interrupt" fn alignment_check(frame: InterruptStackFrame, error: u64) {
    fatal("alignment-check", &frame, Some(error))
}

extern "x86-interrupt" fn machine_check(frame: InterruptStackFrame) -> ! {
    fatal("machine-check", &frame, None)
}

/// Short name of a trap vector handled by `trap.rs`, for fault reports.
pub fn fault_name(vector: u64) -> &'static str {
    match vector {
        trap::VEC_DIVIDE => "divide-error",
        trap::VEC_INVALID_OPCODE => "invalid-opcode",
        trap::VEC_STACK_SEGMENT => "stack-segment-fault",
        trap::VEC_GENERAL_PROTECTION => "general-protection",
        trap::VEC_PAGE_FAULT => "page-fault",
        trap::VEC_X87 => "x87-floating-point",
        trap::VEC_SIMD => "simd-floating-point",
        _ => "unknown-trap",
    }
}

/// `(access, cause)` words for a page-fault error code.
pub fn page_fault_kind(error: u64) -> (&'static str, &'static str) {
    let code = PageFaultErrorCode::from_bits_truncate(error);
    let access = if code.contains(PageFaultErrorCode::INSTRUCTION_FETCH) {
        "execute"
    } else if code.contains(PageFaultErrorCode::CAUSED_BY_WRITE) {
        "write"
    } else {
        "read"
    };
    let cause = if code.contains(PageFaultErrorCode::PROTECTION_VIOLATION) {
        "protection"
    } else {
        "not-present"
    };
    (access, cause)
}

/// A fault on the `trap.rs` path while in kernel mode: fatal. Prints the
/// same markers as the M2 handlers did, then halts.
pub fn kernel_trap_fault(f: &TrapFrame) -> ! {
    if f.vector == trap::VEC_PAGE_FAULT {
        let (access, cause) = page_fault_kind(f.error);
        kprintln!(
            "ZOO_OS EXCEPTION page-fault access={} cause={} addr={:#018x}",
            access,
            cause,
            Cr2::read_raw()
        );
    } else {
        kprintln!("ZOO_OS EXCEPTION {}", fault_name(f.vector));
    }
    kprintln!(
        "ZOO_OS INFO exception rip={:#018x} rsp={:#018x} error={:#x}",
        f.rip,
        f.rsp,
        f.error
    );
    cpu::halt_forever()
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, error: u64) -> ! {
    // A kernel stack overflow faults on the guard page; pushing the page-fault
    // frame onto the same stack faults again, giving this double fault. CR2
    // then still holds an address in the guard page.
    let cr2 = Cr2::read_raw();
    let bottom = STACK_BOTTOM.load(Ordering::Relaxed);
    if bottom != 0 && (bottom - 4096..bottom).contains(&cr2) {
        kprintln!("ZOO_OS EXCEPTION double-fault stack-guard-hit");
    } else {
        kprintln!("ZOO_OS EXCEPTION double-fault cr2={:#018x}", cr2);
    }
    kprintln!(
        "ZOO_OS INFO double_fault cr2={:#018x} stack_bottom={:#018x}",
        cr2,
        bottom
    );
    details(&frame, Some(error));
    cpu::halt_forever()
}
