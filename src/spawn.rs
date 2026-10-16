//! Starting processes inside a running sandbox without an in-sandbox agent.
//!
//! The supervisor's forker thread (see `forker.rs`) has joined the sandbox's
//! PID namespace, so its `clone3` children land there, directly in the
//! workload cgroup. The child then joins the sandbox's mount and IPC
//! namespaces through the init's pidfd, applies seccomp, and execs. The
//! supervisor stays the real parent, so it reads the exit status and
//! delivers signals through a pidfd.
//!
//! The supervisor is multithreaded, so everything between `fork` and
//! `execve` uses only raw syscalls on data prepared before the fork.

use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;

use crate::sys::process::clone3;
use crate::sys::{cstr, cvt_int, path_cstr};

/// Everything needed to start one process, owned so it can move to the
/// forker thread.
pub struct SpawnJob {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub stdio: [OwnedFd; 3],
    /// Descriptors passed on as 3, 4, ... in order (at most `MAX_EXTRA_FDS`).
    pub extra_fds: Vec<OwnedFd>,
    /// The sandbox init's pidfd; the caller keeps it open.
    pub init_pidfd: RawFd,
    /// Also join the init's network namespace (when the sandbox has one).
    pub join_net: bool,
    /// The workload cgroup directory.
    pub cgroup: OwnedFd,
    pub seccomp: Vec<libc::sock_filter>,
}

pub struct Child {
    pub pid: libc::pid_t,
    pub pidfd: OwnedFd,
}

const STAGE_SETNS: u32 = 1;
const STAGE_SETSID: u32 = 3;
const STAGE_STDIO: u32 = 4;
const STAGE_SECCOMP: u32 = 6;
const STAGE_EXEC: u32 = 7;

fn stage_name(stage: u32) -> &'static str {
    match stage {
        STAGE_SETNS => "join sandbox namespaces",
        STAGE_SETSID => "start session",
        STAGE_STDIO => "set up stdio",
        STAGE_SECCOMP => "apply seccomp",
        STAGE_EXEC => "exec",
        _ => "spawn",
    }
}

/// The most descriptors a spawn passes beyond stdio.
pub const MAX_EXTRA_FDS: usize = 4;

/// The lowest number for the error pipe and temporary copies, kept clear of
/// the 3.. range where extra descriptors are placed.
const HIGH_FD: i32 = 64;

/// Everything the child touches, prepared before `fork`.
struct Raw<'a> {
    init_pidfd: i32,
    ns_flags: i32,
    err_fd: i32,
    stdio: [i32; 3],
    extra: [i32; MAX_EXTRA_FDS],
    n_extra: usize,
    cwd: &'a CStr,
    candidates: &'a [CString],
    argv: *const *const libc::c_char,
    envp: *const *const libc::c_char,
    filter: &'a libc::sock_fprog,
}

fn executable_candidates(argv0: &str, env: &BTreeMap<String, String>) -> io::Result<Vec<CString>> {
    if argv0.contains('/') {
        return Ok(vec![cstr(argv0)?]);
    }
    let path = env.get("PATH").map(String::as_str).unwrap_or("/usr/local/bin:/usr/bin");
    path.split(':')
        .filter(|d| !d.is_empty())
        .map(|d| cstr(&format!("{d}/{argv0}")))
        .collect()
}

/// Starts a process in the sandbox. Must run on the forker thread so the
/// child lands in the sandbox's PID namespace. Returns once the child has
/// exec'd, or with the error of the step that failed.
pub fn spawn(job: SpawnJob) -> io::Result<Child> {
    let argv0 = job
        .argv
        .first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
    let argv: Vec<CString> = job.argv.iter().map(|a| cstr(a)).collect::<io::Result<_>>()?;
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());
    let envp: Vec<CString> = job
        .env
        .iter()
        .map(|(k, v)| cstr(&format!("{k}={v}")))
        .collect::<io::Result<_>>()?;
    let mut envp_ptrs: Vec<*const libc::c_char> = envp.iter().map(|e| e.as_ptr()).collect();
    envp_ptrs.push(std::ptr::null());
    let candidates = executable_candidates(argv0, &job.env)?;
    let cwd = path_cstr(&job.cwd)?;
    let filter = libc::sock_fprog {
        len: job.seccomp.len() as u16,
        filter: job.seccomp.as_ptr() as *mut _,
    };

    let mut pipe = [0i32; 2];
    // SAFETY: pipe has room for two fds.
    cvt_int(unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) })?;
    // SAFETY: both fds were just created.
    let (err_rd, err_wr) = unsafe { (OwnedFd::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
    // SAFETY: duplicating a valid fd to a number at or above HIGH_FD; the
    // original write end is closed right away so only the child holds one.
    let err_wr = {
        let high = cvt_int(unsafe { libc::fcntl(err_wr.as_raw_fd(), libc::F_DUPFD_CLOEXEC, HIGH_FD) })?;
        drop(err_wr);
        unsafe { OwnedFd::from_raw_fd(high) }
    };
    if job.extra_fds.len() > MAX_EXTRA_FDS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many descriptors to pass",
        ));
    }
    let mut extra = [-1; MAX_EXTRA_FDS];
    for (slot, fd) in extra.iter_mut().zip(&job.extra_fds) {
        *slot = fd.as_raw_fd();
    }

    let mut ns_flags = libc::CLONE_NEWNS | libc::CLONE_NEWIPC;
    if job.join_net {
        ns_flags |= libc::CLONE_NEWNET;
    }
    let raw = Raw {
        init_pidfd: job.init_pidfd,
        ns_flags,
        err_fd: err_wr.as_raw_fd(),
        stdio: [
            job.stdio[0].as_raw_fd(),
            job.stdio[1].as_raw_fd(),
            job.stdio[2].as_raw_fd(),
        ],
        extra,
        n_extra: job.extra_fds.len(),
        cwd: &cwd,
        candidates: &candidates,
        argv: argv_ptrs.as_ptr(),
        envp: envp_ptrs.as_ptr(),
        filter: &filter,
    };

    // SAFETY: the child only runs `child`, which uses raw syscalls on `raw`.
    let cloned = match unsafe { clone3(0, job.cgroup.as_raw_fd()) }? {
        Some(c) => c,
        // SAFETY: we are the freshly cloned child.
        None => unsafe { child(&raw) },
    };
    drop(err_wr);

    // The pipe closes on a successful exec (end of file); a failure writes
    // one 8-byte report, which a pipe delivers whole.
    let mut report = [0u8; 8];
    let n = loop {
        // SAFETY: reading into a local buffer.
        let n = unsafe { libc::read(err_rd.as_raw_fd(), report.as_mut_ptr().cast(), report.len()) };
        if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        break n;
    };
    match n {
        0 => Ok(Child {
            pid: cloned.pid,
            pidfd: cloned.pidfd,
        }),
        8 => {
            let stage = u32::from_ne_bytes(report[..4].try_into().expect("4 bytes"));
            let errno = i32::from_ne_bytes(report[4..].try_into().expect("4 bytes"));
            let _ = wait(cloned.pidfd.as_fd());
            let e = io::Error::from_raw_os_error(errno);
            Err(io::Error::new(e.kind(), format!("{}: {e}", stage_name(stage))))
        }
        _ => {
            let e = io::Error::last_os_error();
            let _ = wait(cloned.pidfd.as_fd());
            Err(io::Error::other(format!("cannot read the exec report: {e}")))
        }
    }
}

/// # Safety
/// Must only be called in a freshly forked child. Never returns.
unsafe fn child(r: &Raw<'_>) -> ! {
    unsafe fn fail(r: &Raw<'_>, stage: u32, errno: i32) -> ! {
        let mut report = [0u8; 8];
        report[..4].copy_from_slice(&stage.to_ne_bytes());
        report[4..].copy_from_slice(&errno.to_ne_bytes());
        unsafe {
            libc::write(r.err_fd, report.as_ptr().cast(), report.len());
            libc::_exit(127)
        }
    }
    unsafe fn errno() -> i32 {
        unsafe { *libc::__errno_location() }
    }

    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);

        if libc::setns(r.init_pidfd, r.ns_flags) < 0 {
            fail(r, STAGE_SETNS, errno());
        }
        if libc::setsid() < 0 {
            fail(r, STAGE_SETSID, errno());
        }
        for (i, fd) in r.stdio.iter().enumerate() {
            if libc::dup2(*fd, i as i32) < 0 {
                fail(r, STAGE_STDIO, errno());
            }
        }
        // Copy extra descriptors out of the way first, so placing one at
        // 3 + i can never clobber another that has not been placed yet.
        let mut tmp = [-1; MAX_EXTRA_FDS];
        for (slot, fd) in tmp.iter_mut().zip(&r.extra).take(r.n_extra) {
            *slot = libc::fcntl(*fd, libc::F_DUPFD_CLOEXEC, HIGH_FD);
            if *slot < 0 {
                fail(r, STAGE_STDIO, errno());
            }
        }
        for (i, fd) in tmp.iter().enumerate().take(r.n_extra) {
            if libc::dup2(*fd, 3 + i as i32) < 0 {
                fail(r, STAGE_STDIO, errno());
            }
        }
        if libc::isatty(0) == 1 {
            libc::ioctl(0, libc::TIOCSCTTY, 0);
        }

        let first_private = 3 + r.n_extra as u32;
        libc::syscall(
            libc::SYS_close_range,
            first_private,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        );
        if libc::chdir(r.cwd.as_ptr()) < 0 {
            libc::chdir(c"/".as_ptr());
        }
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        if libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            0,
            r.filter as *const libc::sock_fprog,
        ) < 0
        {
            fail(r, STAGE_SECCOMP, errno());
        }

        let mut last = libc::ENOENT;
        for c in r.candidates {
            libc::execve(c.as_ptr(), r.argv, r.envp);
            let e = errno();
            if e != libc::ENOENT && e != libc::ENOTDIR {
                last = e;
            }
        }
        fail(r, STAGE_EXEC, last)
    }
}

/// How a spawned process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
}

/// Reaps the process behind `pidfd`, blocking until it exits.
pub fn wait(pidfd: BorrowedFd<'_>) -> io::Result<Exit> {
    loop {
        // SAFETY: siginfo_t is plain old data filled by waitid.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: waitid on our own child's pidfd.
        let r = unsafe { libc::waitid(libc::P_PIDFD, pidfd.as_raw_fd() as libc::id_t, &mut info, libc::WEXITED) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        // SAFETY: waitid filled a CLD_* siginfo.
        let status = unsafe { info.si_status() };
        return Ok(if info.si_code == libc::CLD_EXITED {
            Exit::Code(status)
        } else {
            Exit::Signal(status)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_follow_path() {
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), "/a::/b".to_string());
        let c = executable_candidates("tool", &env).unwrap();
        assert_eq!(c, vec![cstr("/a/tool").unwrap(), cstr("/b/tool").unwrap()]);
        assert_eq!(executable_candidates("./x", &env).unwrap(), vec![cstr("./x").unwrap()]);
    }
}
