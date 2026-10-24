//! A small seccomp deny-list for interfaces Landlock and namespaces do not cover.

/// Syscalls that return `EPERM` inside the sandbox.
fn denied_syscalls() -> Vec<libc::c_long> {
    vec![
        // The kernel keyring is per user, not per namespace.
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        // Kernel and system administration.
        libc::SYS_kexec_load,
        SYS_KEXEC_FILE_LOAD,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_reboot,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_acct,
        libc::SYS_quotactl,
        // Profiling and tracing interfaces with a large attack surface.
        libc::SYS_perf_event_open,
        libc::SYS_bpf,
    ]
}

#[cfg(target_arch = "x86_64")]
const SYS_KEXEC_FILE_LOAD: libc::c_long = libc::SYS_kexec_file_load;
/// The libc crate does not define it for aarch64 musl; this is its number
/// in the kernel's generic syscall table.
#[cfg(target_arch = "aarch64")]
const SYS_KEXEC_FILE_LOAD: libc::c_long = 294;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH_I386: u32 = 0x4000_0003;

/// The i386 numbers of the denied syscalls, by x86_64 number. i386 has no
/// `kexec_file_load`.
#[cfg(target_arch = "x86_64")]
const I386_NUMBERS: [(libc::c_long, u32); 14] = [
    (libc::SYS_keyctl, 288),
    (libc::SYS_add_key, 286),
    (libc::SYS_request_key, 287),
    (libc::SYS_kexec_load, 283),
    (libc::SYS_init_module, 128),
    (libc::SYS_finit_module, 350),
    (libc::SYS_delete_module, 129),
    (libc::SYS_reboot, 88),
    (libc::SYS_swapon, 87),
    (libc::SYS_swapoff, 115),
    (libc::SYS_acct, 51),
    (libc::SYS_quotactl, 131),
    (libc::SYS_perf_event_open, 336),
    (libc::SYS_bpf, 357),
];
#[cfg(target_arch = "x86_64")]
const I386_IOCTL: u32 = 54;

/// The checks for i386 system calls when `allow_32bit` is set, with the
/// architecture they apply to.
#[cfg(target_arch = "x86_64")]
fn compat_block(allow_32bit: bool) -> Option<(u32, Vec<libc::sock_filter>)> {
    allow_32bit.then(|| {
        let denied = denied_syscalls()
            .into_iter()
            .filter_map(|nr| I386_NUMBERS.iter().find(|(n, _)| *n == nr).map(|(_, i)| *i))
            .collect();
        (
            AUDIT_ARCH_I386,
            [vec![stmt(LD, OFFSET_NR)], checks(denied, I386_IOCTL)].concat(),
        )
    })
}

/// 32-bit ARM programs are not supported.
#[cfg(target_arch = "aarch64")]
fn compat_block(_allow_32bit: bool) -> Option<(u32, Vec<libc::sock_filter>)> {
    None
}

const OFFSET_NR: u32 = 0;
const OFFSET_ARCH: u32 = 4;
const OFFSET_ARG1_LO: u32 = 16 + 8;

const LD: u32 = libc::BPF_LD | libc::BPF_W | libc::BPF_ABS;
const JEQ: u32 = libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K;
const RET: u32 = libc::BPF_RET | libc::BPF_K;
const DENY: u32 = libc::SECCOMP_RET_ERRNO | libc::EPERM as u32;

fn stmt(code: u32, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code: code as u16,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(code: u32, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter {
        code: code as u16,
        jt,
        jf,
        k,
    }
}

/// Builds the BPF program. Terminal-injection ioctls (`TIOCSTI`,
/// `TIOCLINUX`) are denied by argument. System calls of any architecture
/// but the native one kill the process, except i386 calls on x86_64 when
/// `allow_32bit` is set, which get the same deny-list.
pub fn program(allow_32bit: bool) -> Vec<libc::sock_filter> {
    let native = native_block();
    let compat = compat_block(allow_32bit);
    let mut p = vec![
        stmt(LD, OFFSET_ARCH),
        jump(JEQ, AUDIT_ARCH, 1 + compat.is_some() as u8, 0),
    ];
    if let Some((arch, _)) = &compat {
        let skip = u8::try_from(1 + native.len()).expect("the native block is under 255 instructions");
        p.push(jump(JEQ, *arch, skip, 0));
    }
    p.push(stmt(RET, libc::SECCOMP_RET_KILL_PROCESS));
    p.extend(native);
    if let Some((_, block)) = compat {
        p.extend(block);
    }
    p
}

/// The checks for native system calls, ending in a return.
fn native_block() -> Vec<libc::sock_filter> {
    let mut p = vec![stmt(LD, OFFSET_NR)];
    #[cfg(target_arch = "x86_64")]
    {
        // Deny the x32 ABI entirely.
        let jge = libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K;
        p.extend([jump(jge, 0x4000_0000, 0, 1), stmt(RET, DENY)]);
    }
    let denied = denied_syscalls().into_iter().map(|nr| nr as u32).collect();
    p.extend(checks(denied, libc::SYS_ioctl as u32));
    p
}

/// With the syscall number loaded: denies each of `denied`, denies `ioctl`
/// with a terminal-injection request, and allows the rest.
fn checks(denied: Vec<u32>, ioctl: u32) -> Vec<libc::sock_filter> {
    let mut p = Vec::new();
    for nr in denied {
        p.push(jump(JEQ, nr, 0, 1));
        p.push(stmt(RET, DENY));
    }
    p.push(jump(JEQ, ioctl, 0, 5));
    p.push(stmt(LD, OFFSET_ARG1_LO));
    p.push(jump(JEQ, libc::TIOCSTI as u32, 2, 0));
    p.push(jump(JEQ, libc::TIOCLINUX as u32, 1, 0));
    p.push(stmt(RET, libc::SECCOMP_RET_ALLOW));
    p.push(stmt(RET, DENY));
    p.push(stmt(RET, libc::SECCOMP_RET_ALLOW));
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `p` on a system call, interpreting the instructions it uses.
    fn run(p: &[libc::sock_filter], arch: u32, nr: u32, arg1: u32) -> u32 {
        let (mut pc, mut acc) = (0, 0);
        loop {
            let f = p[pc];
            pc += 1;
            match f.code as u32 {
                LD => {
                    acc = match f.k {
                        OFFSET_NR => nr,
                        OFFSET_ARCH => arch,
                        OFFSET_ARG1_LO => arg1,
                        k => panic!("load from offset {k}"),
                    }
                }
                JEQ => pc += if acc == f.k { f.jt } else { f.jf } as usize,
                c if c == libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K => {
                    pc += if acc >= f.k { f.jt } else { f.jf } as usize
                }
                RET => return f.k,
                c => panic!("instruction {c:#x}"),
            }
        }
    }

    const ALLOW: u32 = libc::SECCOMP_RET_ALLOW;
    const KILL: u32 = libc::SECCOMP_RET_KILL_PROCESS;

    #[test]
    fn programs_fit() {
        for allow_32bit in [false, true] {
            assert!(program(allow_32bit).len() < libc::BPF_MAXINSNS as usize);
        }
    }

    #[test]
    fn native_calls_are_filtered() {
        for allow_32bit in [false, true] {
            let p = program(allow_32bit);
            let call = |nr: libc::c_long, arg1| run(&p, AUDIT_ARCH, nr as u32, arg1);
            for nr in denied_syscalls() {
                assert_eq!(call(nr, 0), DENY, "{nr}");
            }
            assert_eq!(call(libc::SYS_read, 0), ALLOW);
            assert_eq!(call(libc::SYS_ioctl, libc::TIOCSTI as u32), DENY);
            assert_eq!(call(libc::SYS_ioctl, libc::TIOCLINUX as u32), DENY);
            assert_eq!(call(libc::SYS_ioctl, libc::TIOCGWINSZ as u32), ALLOW);
            assert_eq!(run(&p, 0x4000_0028, 0, 0), KILL);
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn x32_is_denied() {
        let p = program(false);
        assert_eq!(run(&p, AUDIT_ARCH, 0x4000_0000 | libc::SYS_read as u32, 0), DENY);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn i386_calls_kill_unless_allowed() {
        const STATX: u32 = 383;
        assert_eq!(run(&program(false), AUDIT_ARCH_I386, STATX, 0), KILL);

        let p = program(true);
        let call = |nr, arg1| run(&p, AUDIT_ARCH_I386, nr, arg1);
        for (_, nr) in I386_NUMBERS {
            assert_eq!(call(nr, 0), DENY, "{nr}");
        }
        assert_eq!(call(STATX, 0), ALLOW);
        assert_eq!(call(I386_IOCTL, libc::TIOCSTI as u32), DENY);
        assert_eq!(call(I386_IOCTL, libc::TIOCLINUX as u32), DENY);
        assert_eq!(call(I386_IOCTL, libc::TIOCGWINSZ as u32), ALLOW);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn every_denied_syscall_has_an_i386_number() {
        for nr in denied_syscalls()
            .into_iter()
            .filter(|nr| *nr != libc::SYS_kexec_file_load)
        {
            assert!(I386_NUMBERS.iter().any(|(n, _)| *n == nr), "{nr}");
        }
    }
}
