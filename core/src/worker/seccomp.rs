//! Seccomp syscall filter for hostile workers.
//!
//! The filter is a hand-built classic-BPF program handed to
//! `bwrap --seccomp FD`. bubblewrap loads it *after* it has finished
//! building the namespaces and mounts and immediately before it
//! `execve`s the worker, which is the only correct point: installing
//! the filter earlier would deny bubblewrap the `mount` / `pivot_root`
//! / `unshare` calls its own setup needs.
//!
//! We deliberately do not link `libseccomp`. The program is small,
//! the encoding is stable kernel ABI, and an extra native dependency
//! in the launch path of every App is a worse trade than 200 lines of
//! explicit instruction emission.
//!
//! Shape:
//!
//! ```text
//!   ld  arch                    ; seccomp_data.arch
//!   jne EXPECTED_ARCH -> kill   ; refuse a foreign personality
//!   ld  nr
//!   jge 0x40000000    -> kill   ; refuse the x32 ABI
//!   jeq <denied nr>   -> errno  ; one per denied syscall
//!   ...
//!   ret ALLOW
//!   ret ERRNO(EPERM)
//!   ret KILL_PROCESS
//! ```
//!
//! Denied calls return `EPERM` rather than killing the process: a
//! hostile worker learns nothing from the difference, while a *benign*
//! worker that probes an unavailable feature (glibc trying `io_uring`,
//! a runtime probing `keyctl`) degrades instead of dying.

#![cfg(target_os = "linux")]

use super::policy::SeccompProfile;

const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_JGE: u16 = 0x30;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;

/// `offsetof(struct seccomp_data, nr)`.
const OFFSET_NR: u32 = 0;
/// `offsetof(struct seccomp_data, arch)`.
const OFFSET_ARCH: u32 = 4;
/// `offsetof(struct seccomp_data, args[0])`, low word. `seccomp_data`
/// stores each argument as a 64-bit little-endian value; the domain
/// argument of `socket`/`socketpair` fits in the low word, and the high
/// word is checked separately so a caller cannot smuggle a different
/// domain in the upper 32 bits.
const OFFSET_ARG0_LOW: u32 = 16;
const OFFSET_ARG0_HIGH: u32 = 20;

/// First syscall number of the x32 ABI on x86_64.
const X32_SYSCALL_BIT: u32 = 0x4000_0000;

/// `AF_UNIX` / `AF_LOCAL`. The only socket domain a hostile worker may
/// create: it is how a language runtime builds its event-loop
/// self-pipe, how `asyncio` and `multiprocessing` talk to themselves,
/// and how the SDK reaches the brokered egress endpoint. Every
/// routable domain — `AF_INET`, `AF_INET6`, `AF_NETLINK`, `AF_PACKET`,
/// anything else — is refused, which is what makes "no direct
/// networking" true at the syscall layer and not only at the namespace
/// layer.
const AF_UNIX: u32 = 1;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

/// One classic-BPF instruction, matching `struct sock_filter`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Instruction {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

/// Syscalls denied to every hostile worker.
///
/// Grouped by what they would buy an attacker who already has code
/// execution inside the sandbox:
///
/// * namespace / mount: rebuild the filesystem view, escape the
///   read-only root, or acquire capabilities in a fresh user
///   namespace (the classic bubblewrap escape);
/// * kernel modules and `kexec`: replace the kernel;
/// * `bpf` / `perf_event_open`: read kernel or other-process memory;
/// * `ptrace` / `process_vm_*` / `pidfd_getfd`: attach to a neighbour
///   process or steal one of its descriptors;
/// * keyring calls: reach the owner's kernel keyring;
/// * `io_uring`: a well-known filter-bypass surface, because queued
///   operations are not re-checked against this filter;
/// * time and power control: move the clock the audit chain depends on.
fn denied_syscalls(profile: SeccompProfile) -> Vec<i64> {
    let _ = profile;
    let mut denied: Vec<i64> = vec![
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_mount_setattr,
        libc::SYS_open_tree,
        libc::SYS_move_mount,
        libc::SYS_fsopen,
        libc::SYS_fsconfig,
        libc::SYS_fsmount,
        libc::SYS_fspick,
        libc::SYS_name_to_handle_at,
        libc::SYS_open_by_handle_at,
        libc::SYS_quotactl,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_kexec_load,
        libc::SYS_kexec_file_load,
        libc::SYS_reboot,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_acct,
        libc::SYS_syslog,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_pidfd_getfd,
        libc::SYS_add_key,
        libc::SYS_keyctl,
        libc::SYS_request_key,
        libc::SYS_userfaultfd,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_settimeofday,
        libc::SYS_clock_settime,
        libc::SYS_clock_adjtime,
        libc::SYS_adjtimex,
        libc::SYS_setdomainname,
        libc::SYS_sethostname,
    ];
    denied.sort_unstable();
    denied.dedup();
    denied
}

/// Syscalls whose first argument is a socket domain and is therefore
/// checked rather than refused outright.
fn domain_checked_syscalls() -> [i64; 2] {
    [libc::SYS_socket, libc::SYS_socketpair]
}

/// Emit the filter program for `profile`.
///
/// Layout:
///
/// ```text
///   0 ld  arch
///   1 jne EXPECTED_ARCH        -> kill
///   2 ld  nr
///   3 jge 0x40000000           -> kill        ; the x32 ABI
///   4 jeq socket               -> domain
///   5 jeq socketpair           -> domain
///   6.. jeq <denied nr>        -> errno
///     ret ALLOW
///   domain:
///     ld  args[0].hi
///     jne 0                    -> errno
///     ld  args[0].lo
///     jne AF_UNIX              -> errno
///     ret ALLOW
///     ret ERRNO(EPERM)
///     ret KILL_PROCESS
/// ```
pub fn program(profile: SeccompProfile) -> Vec<Instruction> {
    let denied = denied_syscalls(profile);
    let checked = domain_checked_syscalls();

    // Fixed prologue (4) + one compare per domain-checked syscall +
    // one compare per denied call + the allow fall-through, then the
    // domain block (5) and the three returns.
    let allow_index = 4 + checked.len() + denied.len();
    let domain_index = allow_index + 1;
    let domain_allow_index = domain_index + 4;
    let errno_index = domain_allow_index + 1;
    let kill_index = errno_index + 1;

    let mut program = Vec::with_capacity(kill_index + 1);
    program.push(Instruction {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: OFFSET_ARCH,
    });
    program.push(Instruction {
        code: BPF_JMP | BPF_JEQ | BPF_K,
        jt: 0,
        jf: jump(kill_index, 1),
        k: AUDIT_ARCH,
    });
    program.push(Instruction {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: OFFSET_NR,
    });
    program.push(Instruction {
        code: BPF_JMP | BPF_JGE | BPF_K,
        jt: jump(kill_index, 3),
        jf: 0,
        k: X32_SYSCALL_BIT,
    });
    for (index, syscall) in checked.iter().enumerate() {
        program.push(Instruction {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: jump(domain_index, 4 + index),
            jf: 0,
            k: *syscall as u32,
        });
    }
    let denied_start = 4 + checked.len();
    for (index, syscall) in denied.iter().enumerate() {
        program.push(Instruction {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: jump(errno_index, denied_start + index),
            jf: 0,
            k: *syscall as u32,
        });
    }
    program.push(Instruction {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    });
    // Domain check. The high word must be zero and the low word must be
    // AF_UNIX; anything else — including a domain smuggled above 2^32 —
    // is refused.
    program.push(Instruction {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: OFFSET_ARG0_HIGH,
    });
    program.push(Instruction {
        code: BPF_JMP | BPF_JEQ | BPF_K,
        jt: 0,
        jf: jump(errno_index, domain_index + 1),
        k: 0,
    });
    program.push(Instruction {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: OFFSET_ARG0_LOW,
    });
    program.push(Instruction {
        code: BPF_JMP | BPF_JEQ | BPF_K,
        jt: 0,
        jf: jump(errno_index, domain_index + 3),
        k: AF_UNIX,
    });
    program.push(Instruction {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    });
    program.push(Instruction {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ERRNO | (libc::EPERM as u32 & 0xffff),
    });
    program.push(Instruction {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_KILL_PROCESS,
    });
    debug_assert_eq!(program.len(), kill_index + 1);
    program
}

/// Classic BPF jump offsets are relative to the *next* instruction and
/// only 8 bits wide. Every jump we emit targets one of the three
/// trailing returns, so the distance is bounded by the denied-syscall
/// count; the saturating conversion is a belt-and-braces guard that
/// would produce a stricter (earlier) branch rather than a wild one.
fn jump(target: usize, from: usize) -> u8 {
    u8::try_from(target.saturating_sub(from + 1)).unwrap_or(u8::MAX)
}

/// Serialize the program to the `struct sock_filter[]` byte layout the
/// kernel and bubblewrap expect (native endianness, packed).
pub fn encode(program: &[Instruction]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(program.len() * 8);
    for instruction in program {
        bytes.extend_from_slice(&instruction.code.to_ne_bytes());
        bytes.push(instruction.jt);
        bytes.push(instruction.jf);
        bytes.extend_from_slice(&instruction.k.to_ne_bytes());
    }
    bytes
}

/// The filter bytes for `profile`, ready to be written to the
/// descriptor bubblewrap reads.
pub fn encoded(profile: SeccompProfile) -> Vec<u8> {
    encode(&program(profile))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/seccomp.rs"
    ));
}
