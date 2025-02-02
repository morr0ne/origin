//! Architecture-specific assembly code.

use core::arch::asm;
#[cfg(feature = "origin-signal")]
use linux_raw_sys::general::__NR_rt_sigreturn;
#[cfg(all(feature = "experimental-relocate", feature = "origin-start"))]
#[cfg(relocation_model = "pic")]
use linux_raw_sys::general::{__NR_mprotect, PROT_READ};
#[cfg(feature = "origin-thread")]
use {
    alloc::boxed::Box,
    core::any::Any,
    core::ffi::c_void,
    linux_raw_sys::general::{__NR_clone, __NR_exit, __NR_munmap},
    rustix::thread::RawPid,
};

/// The program entry point.
///
/// # Safety
///
/// This function must never be called explicitly. It is the first thing
/// executed in the program, and it assumes that memory is laid out according
/// to the operating system convention for starting a new program.
#[cfg(feature = "origin-start")]
#[naked]
#[no_mangle]
pub(super) unsafe extern "C" fn _start() -> ! {
    // Jump to `entry`, passing it the initial stack pointer value as an
    // argument, a null return address, a null frame pointer, and an aligned
    // stack pointer. On many architectures, the incoming frame pointer is
    // already null.
    asm!(
        "mov x0, sp",   // Pass the incoming `sp` as the arg to `entry`.
        "mov x30, xzr", // Set the return address to zero.
        "b {entry}",    // Jump to `entry`.
        entry = sym super::program::entry,
        options(noreturn),
    )
}

/// Perform a single load operation, outside the Rust memory model.
///
/// This function conceptually casts `ptr` to a `*const *mut c_void` and loads
/// a `*mut c_void` value from it. However, it does this using `asm`, and
/// `usize` types which don't carry provenance, as it's used by `relocate` to
/// perform relocations which cannot be expressed in the Rust memory model.
///
/// # Safety
///
/// This function must only be called during the relocation process, for
/// relocation purposes. And, `ptr` must contain the address of a memory
/// location that can be loaded from.
#[cfg(all(feature = "experimental-relocate", feature = "origin-start"))]
#[cfg(relocation_model = "pic")]
#[inline]
pub(super) unsafe fn relocation_load(ptr: usize) -> usize {
    let r0;

    // This is read-only but we don't use `readonly` because this memory access
    // happens outside the Rust memory model. As far as Rust knows, this is
    // just an arbitrary side-effecting opaque operation.
    asm!(
        "ldr {}, [{}]",
        out(reg) r0,
        in(reg) ptr,
        options(nostack, preserves_flags),
    );

    r0
}

/// Perform a single store operation, outside the Rust memory model.
///
/// This function conceptually casts `ptr` to a `*mut *mut c_void` and stores
/// a `*mut c_void` value to it. However, it does this using `asm`, and `usize`
/// types which don't carry provenance, as it's used by `relocate` to perform
/// relocations which cannot be expressed in the Rust memory model.
///
/// # Safety
///
/// This function must only be called during the relocation process, for
/// relocation purposes. And, `ptr` must contain the address of a memory
/// location that can be stored to.
#[cfg(all(feature = "experimental-relocate", feature = "origin-start"))]
#[cfg(relocation_model = "pic")]
#[inline]
pub(super) unsafe fn relocation_store(ptr: usize, value: usize) {
    asm!(
        "str {}, [{}]",
        in(reg) value,
        in(reg) ptr,
        options(nostack, preserves_flags),
    );
}

/// Mark "relro" memory as readonly.
///
/// "relro" is a relocation feature in which memory can be readonly after
/// relocations are applied.
///
/// This function conceptually casts `ptr` to a `*mut c_void` and does a
/// `rustix::mm::mprotect(ptr, len, MprotectFlags::READ)`. However, it does
/// this using `asm` and `usize` types which don't carry provenance, as it's
/// used by `relocate` to implement the "relro" feature which cannot be
/// expressed in the Rust memory model.
///
/// # Safety
///
/// This function must only be called during the relocation process, for
/// relocation purposes. And, `ptr` must contain the address of a memory
/// location that can be marked readonly.
#[cfg(all(feature = "experimental-relocate", feature = "origin-start"))]
#[cfg(relocation_model = "pic")]
#[inline]
pub(super) unsafe fn relocation_mprotect_readonly(ptr: usize, len: usize) {
    let r0: usize;

    // This is read-only but we don't use `readonly` because the side effects
    // happen outside the Rust memory model. As far as Rust knows, this is
    // just an arbitrary side-effecting opaque operation.
    asm!(
        "svc 0",
        in("x8") __NR_mprotect,
        inlateout("x0") ptr as usize => r0,
        in("x1") len,
        in("x2") PROT_READ,
        options(nostack, preserves_flags),
    );

    assert_eq!(r0, 0);
}

/// A wrapper around the Linux `clone` system call.
///
/// This can't be implemented in `rustix` because the child starts executing at
/// the same point as the parent and we need to use inline asm to have the
/// child jump to our new-thread entrypoint.
#[cfg(feature = "origin-thread")]
#[inline]
pub(super) unsafe fn clone(
    flags: u32,
    child_stack: *mut c_void,
    parent_tid: *mut RawPid,
    newtls: *mut c_void,
    child_tid: *mut RawPid,
    fn_: *mut Box<dyn FnOnce() -> Option<Box<dyn Any>>>,
) -> isize {
    let r0;
    asm!(
        "svc 0",              // Do the `clone` system call.
        "cbnz x0, 0f",        // Branch if we're in the parent thread.

        // Child thread.
        "mov x0, {fn_}",      // Pass `fn_` as the first argument.
        "mov x29, xzr",       // Zero the frame address.
        "mov x30, xzr",       // Zero the return address.
        "b {entry}",          // Call `entry`.

        // Parent thread.
        "0:",

        entry = sym super::thread::entry,
        fn_ = in(reg) fn_,
        in("x8") __NR_clone,
        inlateout("x0") flags as usize => r0,
        in("x1") child_stack,
        in("x2") parent_tid,
        in("x3") newtls,
        in("x4") child_tid,
        options(nostack)
    );
    r0
}

/// Write a value to the platform thread-pointer register.
#[cfg(feature = "origin-thread")]
#[inline]
pub(super) unsafe fn set_thread_pointer(ptr: *mut c_void) {
    asm!("msr tpidr_el0, {}", in(reg) ptr);
    debug_assert_eq!(thread_pointer(), ptr);
}

/// Read the value of the platform thread-pointer register.
#[cfg(feature = "origin-thread")]
#[inline]
pub(super) fn thread_pointer() -> *mut c_void {
    let ptr;
    unsafe {
        asm!("mrs {}, tpidr_el0", out(reg) ptr, options(nostack, preserves_flags, readonly));
    }
    ptr
}

/// TLS data starts at the location pointed to by the thread pointer.
#[cfg(feature = "origin-thread")]
pub(super) const TLS_OFFSET: usize = 0;

/// `munmap` the current thread, then carefully exit the thread without
/// touching the deallocated stack.
#[cfg(feature = "origin-thread")]
#[inline]
pub(super) unsafe fn munmap_and_exit_thread(map_addr: *mut c_void, map_len: usize) -> ! {
    asm!(
        "svc 0",
        "mov x0, xzr",
        "mov x8, {__NR_exit}",
        "svc 0",
        "udf #16",
        __NR_exit = const __NR_exit,
        in("x8") __NR_munmap,
        in("x0") map_addr,
        in("x1") map_len,
        options(noreturn, nostack)
    );
}

/// Invoke the `__NR_rt_sigreturn` system call to return control from a signal
/// handler.
///
/// # Safety
///
/// This function must never be called other than by the `sa_restorer`
/// mechanism.
#[cfg(feature = "origin-signal")]
#[naked]
pub(super) unsafe extern "C" fn return_from_signal_handler() {
    asm!(
        "mov x8, {__NR_rt_sigreturn}",
        "svc 0",
        "udf #16",
        __NR_rt_sigreturn = const __NR_rt_sigreturn,
        options(noreturn)
    );
}

/// Invoke the appropriate system call to return control from a signal
/// handler that does not use `SA_SIGINFO`. On aarch64, this uses the same
/// sequence as the `SA_SIGINFO` case.
///
/// # Safety
///
/// This function must never be called other than by the `sa_restorer`
/// mechanism.
#[cfg(feature = "origin-signal")]
pub(super) use return_from_signal_handler as return_from_signal_handler_noinfo;
