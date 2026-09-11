use super::*;

fn evaluate(number: i64, architecture: u32, args: [u64; 6]) -> u32 {
    let mut words = [0_u32; 16];
    words[0] = number as u32;
    words[1] = architecture;
    for (index, value) in args.into_iter().enumerate() {
        words[4 + index * 2] = value as u32;
        words[5 + index * 2] = (value >> 32) as u32;
    }
    let instructions = program();
    let mut at = 0;
    let mut accumulator = 0;
    for _ in 0..instructions.len() {
        let instruction = instructions[at];
        match instruction.code {
            0x20 => accumulator = words[instruction.k as usize / 4],
            0x15 => {
                at += usize::from(if accumulator == instruction.k {
                    instruction.jt
                } else {
                    instruction.jf
                });
            }
            0x35 => {
                at += usize::from(if accumulator >= instruction.k {
                    instruction.jt
                } else {
                    instruction.jf
                });
            }
            ALU_AND => accumulator &= instruction.k,
            0x06 => return instruction.k,
            _ => panic!("unexpected GUI filter instruction"),
        }
        at += 1;
    }
    panic!("GUI filter did not terminate")
}

#[test]
fn connect_and_pidfd_signals_cannot_run_without_root_mediation() {
    for number in [libc::SYS_connect, libc::SYS_pidfd_send_signal] {
        assert_eq!(evaluate(number, AUDIT_ARCH, [0; 6]), NOTIFY);
    }
    assert_eq!(
        evaluate(libc::SYS_read, AUDIT_ARCH, [0; 6]),
        SECCOMP_RET_ALLOW
    );
    assert_eq!(
        evaluate(libc::SYS_sendmsg, AUDIT_ARCH, [0; 6]),
        SECCOMP_RET_ALLOW
    );
}

#[test]
fn gui_filter_refuses_alias_listeners_datagrams_and_filter_takeover() {
    let denied = SECCOMP_RET_ERRNO | libc::EPERM as u32;
    for number in [
        libc::SYS_bind,
        libc::SYS_seccomp,
        libc::SYS_process_madvise,
        libc::SYS_process_mrelease,
    ] {
        assert_eq!(evaluate(number, AUDIT_ARCH, [0; 6]), denied);
    }
    for number in [libc::SYS_socket, libc::SYS_socketpair] {
        for kind in [libc::SOCK_STREAM, libc::SOCK_SEQPACKET] {
            for flags in [
                0,
                libc::SOCK_CLOEXEC,
                libc::SOCK_NONBLOCK,
                libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            ] {
                assert_eq!(
                    evaluate(number, AUDIT_ARCH, [1, (kind | flags) as u64, 0, 0, 0, 0]),
                    SECCOMP_RET_ALLOW
                );
            }
        }
        assert_eq!(
            evaluate(number, AUDIT_ARCH, [1, libc::SOCK_DGRAM as u64, 0, 0, 0, 0]),
            denied
        );
        assert_eq!(
            evaluate(number, AUDIT_ARCH, [libc::AF_INET as u64, 1, 0, 0, 0, 0]),
            denied
        );
        assert_eq!(
            evaluate(number, AUDIT_ARCH, [1, 1 | (1_u64 << 32), 0, 0, 0, 0]),
            denied
        );
    }
    assert_eq!(
        evaluate(
            libc::SYS_prctl,
            AUDIT_ARCH,
            [libc::PR_SET_SECCOMP as u64, 0, 0, 0, 0, 0]
        ),
        denied
    );
    assert_eq!(
        evaluate(
            libc::SYS_prctl,
            AUDIT_ARCH,
            [libc::PR_SET_DUMPABLE as u64, 0, 0, 0, 0, 0]
        ),
        SECCOMP_RET_ALLOW
    );
    assert_eq!(
        evaluate(libc::SYS_connect, 0, [0; 6]),
        SECCOMP_RET_KILL_PROCESS
    );
    assert_eq!(
        evaluate(i64::from(X32_SYSCALL_BIT), AUDIT_ARCH, [0; 6]),
        SECCOMP_RET_KILL_PROCESS
    );
}
