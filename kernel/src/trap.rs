//! The single assembly entry path for traps that may switch tasks
//! (docs/m3-design.md §2): the timer, system calls (`int 0x80`) and the
//! faults user code can raise.
//!
//! Each vector has a stub that makes the stack look the same (a dummy error
//! code where the CPU pushes none, then the vector number) and jumps to
//! `trap_common`, which saves every general register, calls `trap_dispatch`,
//! and resumes whichever frame it returns. Returning a different task's frame
//! is the context switch.

use crate::{interrupts, process};
use core::arch::naked_asm;

pub const VEC_DIVIDE: u64 = 0;
pub const VEC_INVALID_OPCODE: u64 = 6;
pub const VEC_STACK_SEGMENT: u64 = 12;
pub const VEC_GENERAL_PROTECTION: u64 = 13;
pub const VEC_PAGE_FAULT: u64 = 14;
pub const VEC_TIMER: u64 = 32;
pub const VEC_SYSCALL: u64 = 0x80;

/// Registers as saved on the kernel stack by `trap_common` and the CPU.
/// Field order is the reverse of the push order (last pushed = lowest
/// address = first field). Must stay in sync with `trap_common`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    /// Pushed by the stub.
    pub vector: u64,
    /// Pushed by the CPU for some exceptions, otherwise 0 from the stub.
    pub error: u64,
    // Pushed by the CPU (always, in 64-bit mode).
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl TrapFrame {
    pub fn is_user_mode(&self) -> bool {
        self.cs & 3 == 3
    }
}

/// Saves the general registers, calls the dispatcher and resumes the frame
/// it returns.
///
/// Safety conditions for the assembly:
/// - Entered only from a stub below, with the CPU-pushed frame, an error code
///   and the vector already on the stack; interrupts are disabled (all gates
///   pointing here are interrupt gates).
/// - Stack alignment: the CPU aligns RSP to 16 before pushing its 5-word
///   frame; with the 2 stub words and 15 registers the total is 22 words, so
///   RSP is 16-byte aligned at the `call`, as the System V ABI requires.
/// - `trap_dispatch` must return a pointer to a complete `TrapFrame` that is
///   at the top of a valid kernel stack; everything above it is discarded.
#[unsafe(naked)]
unsafe extern "C" fn trap_common() {
    naked_asm!(
        "push rax",
        "push rbx",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push rbp",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov rdi, rsp",
        "cld",
        "call {dispatch}",
        "mov rsp, rax",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rbp",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rbx",
        "pop rax",
        // Drop the vector and error code.
        "add rsp, 16",
        "iretq",
        dispatch = sym trap_dispatch,
    );
}

macro_rules! stub {
    // The CPU pushes no error code: push a zero to keep the layout uniform.
    ($name:ident, $vector:expr) => {
        #[unsafe(naked)]
        pub unsafe extern "C" fn $name() {
            naked_asm!(
                "push 0",
                "push {vector}",
                "jmp {common}",
                vector = const $vector,
                common = sym trap_common,
            );
        }
    };
    // The CPU already pushed an error code.
    ($name:ident, $vector:expr, error_code) => {
        #[unsafe(naked)]
        pub unsafe extern "C" fn $name() {
            naked_asm!(
                "push {vector}",
                "jmp {common}",
                vector = const $vector,
                common = sym trap_common,
            );
        }
    };
}

stub!(stub_divide, VEC_DIVIDE);
stub!(stub_invalid_opcode, VEC_INVALID_OPCODE);
stub!(stub_stack_segment, VEC_STACK_SEGMENT, error_code);
stub!(stub_general_protection, VEC_GENERAL_PROTECTION, error_code);
stub!(stub_page_fault, VEC_PAGE_FAULT, error_code);
stub!(stub_timer, VEC_TIMER);
stub!(stub_syscall, VEC_SYSCALL);

/// Called by `trap_common` with interrupts disabled. Returns the frame to
/// resume: the same one, or another task's after a switch.
extern "C" fn trap_dispatch(frame: *mut TrapFrame) -> *mut TrapFrame {
    // SAFETY: `trap_common` passes a pointer to the frame it just finished
    // pushing on the current stack; nothing else refers to it during this
    // call.
    let f = unsafe { &mut *frame };
    match f.vector {
        VEC_TIMER => {
            interrupts::on_timer_tick();
            process::on_timer(frame)
        }
        VEC_SYSCALL if f.is_user_mode() => process::on_syscall(frame),
        _ if f.is_user_mode() => process::on_user_fault(frame),
        _ => interrupts::kernel_trap_fault(f),
    }
}
