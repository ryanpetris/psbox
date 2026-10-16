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

const OFFSET_NR: u32 = 0;
const OFFSET_ARCH: u32 = 4;
const OFFSET_ARG1_LO: u32 = 16 + 8;

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
/// `TIOCLINUX`) are denied by argument.
pub fn program() -> Vec<libc::sock_filter> {
    use libc::{BPF_ABS, BPF_JEQ, BPF_JMP, BPF_K, BPF_LD, BPF_RET, BPF_W};
    let ld = BPF_LD | BPF_W | BPF_ABS;
    let jeq = BPF_JMP | BPF_JEQ | BPF_K;
    let ret = BPF_RET | BPF_K;
    let deny = libc::SECCOMP_RET_ERRNO | libc::EPERM as u32;

    let mut p = vec![
        stmt(ld, OFFSET_ARCH),
        jump(jeq, AUDIT_ARCH, 1, 0),
        stmt(ret, libc::SECCOMP_RET_KILL_PROCESS),
        stmt(ld, OFFSET_NR),
    ];
    #[cfg(target_arch = "x86_64")]
    {
        // Deny the x32 ABI entirely.
        use libc::BPF_JGE;
        p.push(jump(BPF_JMP | BPF_JGE | BPF_K, 0x4000_0000, 0, 1));
        p.push(stmt(ret, deny));
    }
    for nr in denied_syscalls() {
        p.push(jump(jeq, nr as u32, 0, 1));
        p.push(stmt(ret, deny));
    }
    p.push(jump(jeq, libc::SYS_ioctl as u32, 0, 5));
    p.push(stmt(ld, OFFSET_ARG1_LO));
    p.push(jump(jeq, libc::TIOCSTI as u32, 2, 0));
    p.push(jump(jeq, libc::TIOCLINUX as u32, 1, 0));
    p.push(stmt(ret, libc::SECCOMP_RET_ALLOW));
    p.push(stmt(ret, deny));
    p.push(stmt(ret, libc::SECCOMP_RET_ALLOW));
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_fits_and_ends_with_allow() {
        let p = program();
        assert!(p.len() < libc::BPF_MAXINSNS as usize);
        assert_eq!(p.last().unwrap().k, libc::SECCOMP_RET_ALLOW);
    }

    #[test]
    fn ioctl_jumps_land_on_returns() {
        let p = program();
        let ioctl = p.iter().position(|f| f.k == libc::SYS_ioctl as u32).unwrap();
        // jf skips the whole ioctl block to the trailing allow.
        assert_eq!(ioctl + 1 + p[ioctl].jf as usize, p.len() - 1);
        let sti = ioctl + 2;
        assert_eq!(
            p[sti + 1 + p[sti].jt as usize].k,
            libc::SECCOMP_RET_ERRNO | libc::EPERM as u32
        );
        let linux = ioctl + 3;
        assert_eq!(
            p[linux + 1 + p[linux].jt as usize].k,
            libc::SECCOMP_RET_ERRNO | libc::EPERM as u32
        );
    }
}
