use super::*;

/// Decode the emitted program back into `(code, jt, jf, k)` tuples so
/// the assertions read like the instruction stream they check.
fn decoded(profile: SeccompProfile) -> Vec<(u16, u8, u8, u32)> {
    program(profile)
        .into_iter()
        .map(|instruction| {
            (
                instruction.code,
                instruction.jt,
                instruction.jf,
                instruction.k,
            )
        })
        .collect()
}

#[test]
fn program_validates_the_architecture_before_reading_the_syscall() {
    let program = decoded(SeccompProfile::Strict);
    assert_eq!(program[0].0, BPF_LD | BPF_W | BPF_ABS);
    assert_eq!(program[0].3, OFFSET_ARCH);
    assert_eq!(program[1].0, BPF_JMP | BPF_JEQ | BPF_K);
    assert_eq!(program[1].3, AUDIT_ARCH);
    assert_eq!(program[2].3, OFFSET_NR);
}

#[test]
fn foreign_architecture_and_x32_are_killed_not_merely_denied() {
    let program = decoded(SeccompProfile::Strict);
    let kill = program.last().expect("trailing returns");
    assert_eq!(kill.0, BPF_RET | BPF_K);
    assert_eq!(kill.3, SECCOMP_RET_KILL_PROCESS);

    let kill_index = program.len() - 1;
    // Instruction 1 falls through to kill on mismatch, instruction 3
    // jumps to it for the x32 bit.
    assert_eq!(1 + 1 + program[1].2 as usize, kill_index);
    assert_eq!(3 + 1 + program[3].1 as usize, kill_index);
    assert_eq!(program[3].3, X32_SYSCALL_BIT);
}

#[test]
fn every_escape_syscall_is_denied() {
    let program = decoded(SeccompProfile::StrictNetwork);
    let compared: Vec<u32> = program
        .iter()
        .filter(|(code, _, _, _)| *code == (BPF_JMP | BPF_JEQ | BPF_K))
        .map(|(_, _, _, k)| *k)
        .collect();
    for syscall in [
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_mount,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_ptrace,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_keyctl,
        libc::SYS_io_uring_setup,
        libc::SYS_process_vm_readv,
        libc::SYS_pidfd_getfd,
        libc::SYS_init_module,
        libc::SYS_clock_settime,
    ] {
        assert!(
            compared.contains(&(syscall as u32)),
            "syscall {syscall} is not filtered"
        );
    }
}

#[test]
fn socket_creation_is_filtered_by_domain_rather_than_refused() {
    // Both profiles allow `AF_UNIX` and nothing else: a runtime's event
    // loop needs a self-pipe, and the SDK reaches the egress broker over
    // a Unix socket, but no routable domain may be created at all.
    for profile in [SeccompProfile::Strict, SeccompProfile::StrictNetwork] {
        let denied = denied_syscalls(profile);
        assert!(!denied.contains(&libc::SYS_socket));
        assert!(!denied.contains(&libc::SYS_socketpair));
        assert!(denied.contains(&libc::SYS_unshare));

        let program = decoded(profile);
        let compared: Vec<u32> = program
            .iter()
            .filter(|(code, _, _, _)| *code == (BPF_JMP | BPF_JEQ | BPF_K))
            .map(|(_, _, _, k)| *k)
            .collect();
        assert!(compared.contains(&(libc::SYS_socket as u32)));
        assert!(compared.contains(&(libc::SYS_socketpair as u32)));
        assert!(compared.contains(&AF_UNIX));
    }
}

#[test]
fn the_domain_check_reads_both_halves_of_the_argument() {
    let program = decoded(SeccompProfile::Strict);
    let loads: Vec<u32> = program
        .iter()
        .filter(|(code, _, _, _)| *code == (BPF_LD | BPF_W | BPF_ABS))
        .map(|(_, _, _, k)| *k)
        .collect();
    // arch, nr, and both words of args[0]: a domain hidden in the upper
    // 32 bits must not slip past a low-word-only comparison.
    assert!(loads.contains(&OFFSET_ARCH));
    assert!(loads.contains(&OFFSET_NR));
    assert!(loads.contains(&OFFSET_ARG0_LOW));
    assert!(loads.contains(&OFFSET_ARG0_HIGH));
}

#[test]
fn a_non_unix_domain_falls_through_to_the_denial() {
    let program = decoded(SeccompProfile::Strict);
    // The instruction comparing AF_UNIX jumps nowhere on match (it falls
    // through to an ALLOW) and jumps to the ERRNO return otherwise.
    let index = program
        .iter()
        .position(|(code, _, _, k)| *code == (BPF_JMP | BPF_JEQ | BPF_K) && *k == AF_UNIX)
        .expect("domain comparison");
    let (_, jt, jf, _) = program[index];
    assert_eq!(jt, 0, "AF_UNIX must fall through to the allow");
    let target = index + 1 + jf as usize;
    assert_eq!(
        program[target].3,
        SECCOMP_RET_ERRNO | (libc::EPERM as u32),
        "a non-AF_UNIX domain must land on the denial"
    );
}

#[test]
fn denied_calls_return_eperm_and_the_default_allows() {
    let program = decoded(SeccompProfile::Strict);
    let kill = program.len() - 1;
    assert_eq!(program[kill].3, SECCOMP_RET_KILL_PROCESS);
    assert_eq!(program[kill - 1].3, SECCOMP_RET_ERRNO | (libc::EPERM as u32));
    // The fall-through after the last syscall comparison allows.
    let allow = program
        .iter()
        .position(|(code, _, _, k)| *code == (BPF_RET | BPF_K) && *k == SECCOMP_RET_ALLOW)
        .expect("allow return");
    assert!(allow < kill);
}

#[test]
fn jumps_stay_inside_the_program() {
    for profile in [SeccompProfile::Strict, SeccompProfile::StrictNetwork] {
        let program = decoded(profile);
        for (index, (code, jt, jf, _)) in program.iter().enumerate() {
            if *code & BPF_JMP == 0 || *code == (BPF_RET | BPF_K) {
                continue;
            }
            assert!(index + 1 + (*jt as usize) < program.len(), "jt escapes");
            assert!(index + 1 + (*jf as usize) < program.len(), "jf escapes");
        }
    }
}

#[test]
fn encoding_is_eight_bytes_per_instruction() {
    let program = program(SeccompProfile::Strict);
    let bytes = encode(&program);
    assert_eq!(bytes.len(), program.len() * 8);
    assert_eq!(encoded(SeccompProfile::Strict), bytes);
}
