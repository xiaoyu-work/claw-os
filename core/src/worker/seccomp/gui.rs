use std::os::fd::{FromRawFd, OwnedFd};

use super::*;

const NOTIFY: u32 = 0x7fc0_0000;
const ALU_AND: u16 = 0x54;
const NEW_LISTENER: u32 = 1 << 3;

pub(crate) fn architecture() -> u32 {
    AUDIT_ARCH
}

pub(crate) fn program() -> Vec<Instruction> {
    let mut result = Vec::new();
    let mut jumps = Vec::new();
    let mut load = |offset| {
        result.push(Instruction {
            code: BPF_LD | BPF_W | BPF_ABS,
            jt: 0,
            jf: 0,
            k: offset,
        });
    };
    load(OFFSET_ARCH);
    let mut branch = |result: &mut Vec<Instruction>, value, yes, no| {
        jumps.push((result.len(), yes, no));
        result.push(Instruction {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 0,
            k: value,
        });
    };
    // Labels: allow, refuse, kill, notify, socket, prctl.
    branch(&mut result, AUDIT_ARCH, None, Some(2));
    result.push(Instruction {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: OFFSET_NR,
    });
    jumps.push((result.len(), Some(2), None));
    result.push(Instruction {
        code: BPF_JMP | BPF_JGE | BPF_K,
        jt: 0,
        jf: 0,
        k: X32_SYSCALL_BIT,
    });
    let mut branch = |result: &mut Vec<Instruction>, value, yes, no| {
        jumps.push((result.len(), yes, no));
        result.push(Instruction {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 0,
            k: value,
        });
    };
    for number in [libc::SYS_connect, libc::SYS_pidfd_send_signal] {
        branch(&mut result, number as u32, Some(3), None);
    }
    for number in [
        libc::SYS_bind,
        libc::SYS_seccomp,
        libc::SYS_process_madvise,
        libc::SYS_process_mrelease,
    ] {
        branch(&mut result, number as u32, Some(1), None);
    }
    branch(&mut result, libc::SYS_socket as u32, Some(4), None);
    branch(&mut result, libc::SYS_socketpair as u32, Some(4), None);
    branch(&mut result, libc::SYS_prctl as u32, Some(5), None);
    let allow = result.len();
    result.push(ret(SECCOMP_RET_ALLOW));
    let socket = result.len();
    result.push(ld(OFFSET_ARG0_HIGH));
    branch(&mut result, 0, None, Some(1));
    result.push(ld(OFFSET_ARG0_LOW));
    branch(&mut result, libc::AF_UNIX as u32, None, Some(1));
    result.push(ld(OFFSET_ARG0_HIGH + 8));
    branch(&mut result, 0, None, Some(1));
    result.push(ld(OFFSET_ARG0_LOW + 8));
    result.push(Instruction {
        code: ALU_AND,
        jt: 0,
        jf: 0,
        k: 0xf,
    });
    branch(&mut result, libc::SOCK_STREAM as u32, Some(0), None);
    branch(&mut result, libc::SOCK_SEQPACKET as u32, Some(0), Some(1));
    let prctl = result.len();
    result.push(ld(OFFSET_ARG0_HIGH));
    branch(&mut result, 0, None, Some(1));
    result.push(ld(OFFSET_ARG0_LOW));
    branch(&mut result, libc::PR_SET_SECCOMP as u32, Some(1), Some(0));
    // Returns must follow every branch: classic BPF has no backward jumps.
    let allow_return = result.len();
    result.push(ret(SECCOMP_RET_ALLOW));
    let refuse = result.len();
    result.push(ret(SECCOMP_RET_ERRNO | libc::EPERM as u32));
    let kill = result.len();
    result.push(ret(SECCOMP_RET_KILL_PROCESS));
    let notify = result.len();
    result.push(ret(NOTIFY));
    let labels = [allow_return, refuse, kill, notify, socket, prctl];
    for (at, yes, no) in jumps {
        if let Some(label) = yes {
            result[at].jt =
                u8::try_from(labels[label] - at - 1).expect("bounded forward GUI seccomp branch");
        }
        if let Some(label) = no {
            result[at].jf =
                u8::try_from(labels[label] - at - 1).expect("bounded forward GUI seccomp branch");
        }
    }
    debug_assert_eq!(result[allow], ret(SECCOMP_RET_ALLOW));
    result
}

fn ld(offset: u32) -> Instruction {
    Instruction {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: offset,
    }
}

fn ret(value: u32) -> Instruction {
    Instruction {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: value,
    }
}

pub(crate) fn install() -> std::io::Result<OwnedFd> {
    if unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } != 1
        || unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) } != 2
        || std::fs::read_dir("/proc/self/task")?.count() != 1
    {
        return Err(std::io::Error::other(
            "GUI transport requires the existing worker filter and a single gated OS runner",
        ));
    }
    let program = program();
    let filter = libc::sock_fprog {
        len: u16::try_from(program.len())
            .map_err(|_| std::io::Error::other("GUI syscall filter exceeds kernel bounds"))?,
        filter: program.as_ptr().cast::<libc::sock_filter>().cast_mut(),
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            1_u32,
            NEW_LISTENER,
            std::ptr::addr_of!(filter),
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/seccomp/gui.rs"
    ));
}
