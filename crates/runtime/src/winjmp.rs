//! Win64 setjmp/longjmp, hand-rolled (P20 Windows port).
//!
//! msvcrt's x64 `longjmp` performs SEH unwinding unless the jmp_buf's
//! frame slot is NULL, and zig's mingw import set doesn't even expose
//! `_setjmp` — so the runtime carries its own pair with exactly the
//! semantics the unwind scheme needs: save/restore the Win64 callee-saved
//! register set (RBX, RBP, RSI, RDI, R12-R15, RSP, RIP, XMM6-XMM15) and
//! nothing else. Buffer layout: 10 GPR slots (80 bytes) then 10 XMM slots
//! (160 bytes) = 240 bytes, stored unaligned-safely; both the backend
//! (512-byte jmpbuf alloca) and the GC's 256-byte spill buffer fit it.
//!
//! `vs_longjmp(buf, val)` never returns; a `val` of 0 arrives as 1, like
//! the C contract.

// SAFETY note: this is the one arch/OS-specific corner of the runtime;
// the symbols only exist on x86_64 Windows builds.
#[cfg(all(windows, target_arch = "x86_64"))]
std::arch::global_asm!(
    ".globl vs_setjmp",
    "vs_setjmp:",
    "mov [rcx], rbx",
    "mov [rcx+8], rbp",
    "mov [rcx+16], rsi",
    "mov [rcx+24], rdi",
    "mov [rcx+32], r12",
    "mov [rcx+40], r13",
    "mov [rcx+48], r14",
    "mov [rcx+56], r15",
    "lea rax, [rsp+8]", // RSP after this call returns
    "mov [rcx+64], rax",
    "mov rax, [rsp]", // return address
    "mov [rcx+72], rax",
    "movups [rcx+80], xmm6",
    "movups [rcx+96], xmm7",
    "movups [rcx+112], xmm8",
    "movups [rcx+128], xmm9",
    "movups [rcx+144], xmm10",
    "movups [rcx+160], xmm11",
    "movups [rcx+176], xmm12",
    "movups [rcx+192], xmm13",
    "movups [rcx+208], xmm14",
    "movups [rcx+224], xmm15",
    "xor eax, eax",
    "ret",
    ".globl vs_longjmp",
    "vs_longjmp:",
    "mov rbx, [rcx]",
    "mov rbp, [rcx+8]",
    "mov rsi, [rcx+16]",
    "mov rdi, [rcx+24]",
    "mov r12, [rcx+32]",
    "mov r13, [rcx+40]",
    "mov r14, [rcx+48]",
    "mov r15, [rcx+56]",
    "movups xmm6, [rcx+80]",
    "movups xmm7, [rcx+96]",
    "movups xmm8, [rcx+112]",
    "movups xmm9, [rcx+128]",
    "movups xmm10, [rcx+144]",
    "movups xmm11, [rcx+160]",
    "movups xmm12, [rcx+176]",
    "movups xmm13, [rcx+192]",
    "movups xmm14, [rcx+208]",
    "movups xmm15, [rcx+224]",
    "mov eax, edx", // return value; 0 becomes 1 per the C contract
    "test eax, eax",
    "jnz 2f",
    "mov eax, 1",
    "2:",
    "mov rsp, [rcx+64]",
    "jmp qword ptr [rcx+72]",
);
