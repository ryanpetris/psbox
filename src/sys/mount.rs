//! New mount API: detached trees, filesystem contexts, and attachment.

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Component, Path, PathBuf};

use super::{cstr, cvt, path_cstr};

pub const MOVE_MOUNT_F_EMPTY_PATH: libc::c_uint = 0x04;
pub const MOVE_MOUNT_T_EMPTY_PATH: libc::c_uint = 0x40;
pub const MOVE_MOUNT_BENEATH: libc::c_uint = 0x200;

const FSOPEN_CLOEXEC: libc::c_uint = 0x1;
const FSMOUNT_CLOEXEC: libc::c_uint = 0x1;
const FSCONFIG_SET_FLAG: libc::c_uint = 0;
const FSCONFIG_SET_STRING: libc::c_uint = 1;
const FSCONFIG_CMD_CREATE: libc::c_uint = 6;

pub const MOUNT_ATTR_RDONLY: u64 = 0x1;
pub const MOUNT_ATTR_NOSUID: u64 = 0x2;
pub const MOUNT_ATTR_NODEV: u64 = 0x4;

pub const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
pub const RESOLVE_NO_SYMLINKS: u64 = 0x04;
pub const RESOLVE_BENEATH: u64 = 0x08;
pub const RESOLVE_IN_ROOT: u64 = 0x10;

/// Resolution for paths inside a sandbox root: confined to it, and never
/// through a symlink, which an app could have planted to redirect psbox.
pub const RESOLVE_SAFE_IN_ROOT: u64 = RESOLVE_IN_ROOT | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS;

/// `struct open_how` for `openat2`.
#[repr(C)]
pub struct OpenHow {
    pub flags: u64,
    pub mode: u64,
    pub resolve: u64,
}

#[repr(C)]
struct MountAttr {
    attr_set: u64,
    attr_clr: u64,
    propagation: u64,
    userns_fd: u64,
}

fn owned(fd: libc::c_long) -> OwnedFd {
    // SAFETY: the caller passes a freshly returned, valid descriptor.
    unsafe { OwnedFd::from_raw_fd(fd as RawFd) }
}

/// Clones the mount at `path` (recursively when asked) as a detached tree.
pub fn clone_tree(path: &Path, recursive: bool) -> io::Result<OwnedFd> {
    let c = path_cstr(path)?;
    let mut flags = (libc::OPEN_TREE_CLONE | libc::OPEN_TREE_CLOEXEC) as libc::c_uint;
    if recursive {
        flags |= libc::AT_RECURSIVE as libc::c_uint;
    }
    // SAFETY: valid C string and flags; returns a new fd or -1.
    let fd = cvt(unsafe { libc::syscall(libc::SYS_open_tree, libc::AT_FDCWD, c.as_ptr(), flags) })?;
    Ok(owned(fd))
}

/// Sets and clears mount attributes on a mount fd (detached or attached).
pub fn set_attr_fd(fd: BorrowedFd<'_>, recursive: bool, set: u64, clear: u64) -> io::Result<()> {
    let mut flags = libc::AT_EMPTY_PATH as libc::c_uint;
    if recursive {
        flags |= libc::AT_RECURSIVE as libc::c_uint;
    }
    let attr = MountAttr {
        attr_set: set,
        attr_clr: clear,
        propagation: 0,
        userns_fd: 0,
    };
    // SAFETY: attr is a correctly sized, initialized mount_attr.
    cvt(unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            fd.as_raw_fd(),
            c"".as_ptr(),
            flags,
            &attr as *const MountAttr,
            std::mem::size_of::<MountAttr>(),
        )
    })?;
    Ok(())
}

/// Sets mount attributes on the mount at `path`.
pub fn set_attr_path(path: &Path, recursive: bool, set: u64) -> io::Result<()> {
    let c = path_cstr(path)?;
    let flags = if recursive {
        libc::AT_RECURSIVE as libc::c_uint
    } else {
        0
    };
    let attr = MountAttr {
        attr_set: set,
        attr_clr: 0,
        propagation: 0,
        userns_fd: 0,
    };
    // SAFETY: attr is a correctly sized, initialized mount_attr.
    cvt(unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            libc::AT_FDCWD,
            c.as_ptr(),
            flags,
            &attr as *const MountAttr,
            std::mem::size_of::<MountAttr>(),
        )
    })?;
    Ok(())
}

/// Attaches a detached tree onto the object referred to by `target` (an O_PATH fd).
pub fn attach(tree: BorrowedFd<'_>, target: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: both fds are valid; empty paths with the EMPTY_PATH flags.
    cvt(unsafe {
        libc::syscall(
            libc::SYS_move_mount,
            tree.as_raw_fd(),
            c"".as_ptr(),
            target.as_raw_fd(),
            c"".as_ptr(),
            MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH,
        )
    })?;
    Ok(())
}

/// A filesystem configuration context from `fsopen`.
pub struct FsContext(OwnedFd);

impl FsContext {
    pub fn new(fstype: &str) -> io::Result<Self> {
        let c = cstr(fstype)?;
        // SAFETY: valid C string.
        let fd = cvt(unsafe { libc::syscall(libc::SYS_fsopen, c.as_ptr(), FSOPEN_CLOEXEC) })?;
        Ok(Self(owned(fd)))
    }

    pub fn set_string(&self, key: &str, value: &str) -> io::Result<()> {
        let k = cstr(key)?;
        let v = cstr(value)?;
        self.config(FSCONFIG_SET_STRING, &k, v.as_ptr().cast())
    }

    pub fn set_flag(&self, key: &str) -> io::Result<()> {
        let k = cstr(key)?;
        self.config(FSCONFIG_SET_FLAG, &k, std::ptr::null())
    }

    fn config(&self, cmd: libc::c_uint, key: &CStr, value: *const libc::c_void) -> io::Result<()> {
        // SAFETY: key and value are valid for the duration of the call.
        cvt(unsafe { libc::syscall(libc::SYS_fsconfig, self.0.as_raw_fd(), cmd, key.as_ptr(), value, 0) })
            .map_err(|e| io::Error::new(e.kind(), format!("fsconfig {}: {e}", key.to_string_lossy())))?;
        Ok(())
    }

    /// Creates the superblock and returns a detached mount with `attrs`.
    pub fn mount(self, attrs: u64) -> io::Result<OwnedFd> {
        // SAFETY: create command takes no key/value.
        cvt(unsafe {
            libc::syscall(
                libc::SYS_fsconfig,
                self.0.as_raw_fd(),
                FSCONFIG_CMD_CREATE,
                std::ptr::null::<libc::c_char>(),
                std::ptr::null::<libc::c_void>(),
                0,
            )
        })?;
        // SAFETY: fsmount on a created context.
        let fd = cvt(unsafe { libc::syscall(libc::SYS_fsmount, self.0.as_raw_fd(), FSMOUNT_CLOEXEC, attrs) })?;
        Ok(owned(fd))
    }
}

/// A detached tmpfs with the given root mode.
pub fn tmpfs(mode: u32, attrs: u64) -> io::Result<OwnedFd> {
    let ctx = FsContext::new("tmpfs")?;
    ctx.set_string("mode", &format!("{mode:o}"))?;
    ctx.mount(attrs)
}

/// `openat2` relative to `dirfd`, with the given resolve flags.
pub fn openat2(dirfd: BorrowedFd<'_>, path: &Path, flags: i32, resolve: u64) -> io::Result<OwnedFd> {
    let c = path_cstr(path)?;
    let how = OpenHow {
        flags: (flags | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve,
    };
    // SAFETY: how is a valid open_how of the size passed.
    let fd = cvt(unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd.as_raw_fd(),
            c.as_ptr(),
            &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    })?;
    Ok(owned(fd))
}

/// The kind of mount point to create for an attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointKind {
    Dir,
    File,
}

/// Creates `rel` (and missing parents) beneath `root`, resolving every
/// component inside `root` and refusing symlinks, so nothing an app planted
/// can redirect where the point lands. Returns an O_PATH fd for the final
/// component.
pub fn make_point(root: BorrowedFd<'_>, rel: &Path, kind: PointKind) -> io::Result<OwnedFd> {
    let parts: Vec<_> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(p) => Some(p),
            _ => None,
        })
        .collect();
    let mut prefix = std::path::PathBuf::new();
    for (i, part) in parts.iter().enumerate() {
        let parent = if prefix.as_os_str().is_empty() {
            openat2(
                root,
                Path::new("."),
                libc::O_PATH | libc::O_DIRECTORY,
                RESOLVE_SAFE_IN_ROOT,
            )?
        } else {
            openat2(root, &prefix, libc::O_PATH | libc::O_DIRECTORY, RESOLVE_SAFE_IN_ROOT)?
        };
        prefix.push(part);
        let last = i + 1 == parts.len();
        let name = path_cstr(Path::new(part))?;
        let exists = openat2(root, &prefix, libc::O_PATH, RESOLVE_SAFE_IN_ROOT);
        if exists.is_ok() {
            continue;
        }
        if last && kind == PointKind::File {
            // SAFETY: parent is an O_PATH dir fd; mknodat creates an empty regular file.
            let r = unsafe { libc::mknodat(parent.as_raw_fd(), name.as_ptr(), libc::S_IFREG | 0o600, 0) };
            if r < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
                return Err(io::Error::last_os_error());
            }
        } else {
            // SAFETY: parent is a valid dir fd.
            let r = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) };
            if r < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
                return Err(io::Error::last_os_error());
            }
        }
    }
    openat2(root, rel, libc::O_PATH, RESOLVE_SAFE_IN_ROOT)
}

/// Opens the directory `rel` inside `root` for changing it (not `O_PATH`,
/// so it can be `fchmod`ed), refusing symlinks.
pub fn open_dir_in(root: BorrowedFd<'_>, rel: &Path) -> io::Result<OwnedFd> {
    openat2(root, rel, libc::O_RDONLY | libc::O_DIRECTORY, RESOLVE_SAFE_IN_ROOT)
}

/// Writes `data` to the file `name` in the directory `dir`, replacing its
/// contents, and never following a symlink at `name`.
pub fn write_at(dir: BorrowedFd<'_>, name: &str, data: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let c = cstr(name)?;
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: valid dir fd and C string; returns a new fd or -1.
    let fd = cvt(unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), flags, 0o644) } as libc::c_long)?;
    std::fs::File::from(owned(fd)).write_all(data)
}

/// Creates the symlink `name` to `target` in the directory `dir`, leaving
/// anything already there.
pub fn symlink_at(target: &Path, dir: BorrowedFd<'_>, name: &std::ffi::OsStr) -> io::Result<()> {
    let target = path_cstr(target)?;
    let name = path_cstr(Path::new(name))?;
    // SAFETY: valid C strings and dir fd.
    let r = unsafe { libc::symlinkat(target.as_ptr(), dir.as_raw_fd(), name.as_ptr()) };
    if r < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A path naming exactly the file behind `fd`, for interfaces that only
/// take paths (socket addresses, overlay layers).
pub fn fd_path(fd: BorrowedFd<'_>) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()))
}

/// Opens the host path `path` (O_PATH). Beneath `guard`, a directory a
/// sandbox can write to, no symlink is followed: an app could have planted
/// one to point a configured mount at any host path. `guard` itself, and
/// the path outside any guard, resolve normally, so links the user made
/// still work.
pub fn open_source(path: &Path, guard: Option<&Path>) -> io::Result<OwnedFd> {
    let beneath = guard.and_then(|g| {
        let rest = path.strip_prefix(g).ok()?;
        (!rest.as_os_str().is_empty()).then_some((g, rest))
    });
    let open = |p: &Path| -> io::Result<OwnedFd> {
        let c = path_cstr(p)?;
        // SAFETY: valid C string.
        let fd = cvt(unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) } as libc::c_long)?;
        Ok(owned(fd))
    };
    match beneath {
        Some((guard, rest)) => openat2(
            open(guard)?.as_fd(),
            rest,
            libc::O_PATH,
            RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS,
        ),
        None => open(path),
    }
}

/// Clones the mount tree at an O_PATH fd as a detached mount.
pub fn clone_tree_fd(fd: BorrowedFd<'_>, recursive: bool) -> io::Result<OwnedFd> {
    let mut flags =
        (libc::OPEN_TREE_CLONE | libc::OPEN_TREE_CLOEXEC) as libc::c_uint | libc::AT_EMPTY_PATH as libc::c_uint;
    if recursive {
        flags |= libc::AT_RECURSIVE as libc::c_uint;
    }
    // SAFETY: valid fd, empty path with AT_EMPTY_PATH; returns a new fd or -1.
    let fd = cvt(unsafe { libc::syscall(libc::SYS_open_tree, fd.as_raw_fd(), c"".as_ptr(), flags) })?;
    Ok(owned(fd))
}

/// Creates the directory `path` and its missing parents. Beneath `guard`
/// (see [`open_source`]) each component is created without following
/// symlinks; elsewhere this is `create_dir_all`.
pub fn create_dir_guarded(path: &Path, guard: Option<&Path>) -> io::Result<()> {
    let Some((guard, rest)) = guard.and_then(|g| Some((g, path.strip_prefix(g).ok()?))) else {
        return std::fs::create_dir_all(path);
    };
    std::fs::create_dir_all(guard)?;
    let mut dir = open_dir(guard)?;
    for part in rest.components().filter_map(|c| match c {
        Component::Normal(p) => Some(p),
        _ => None,
    }) {
        let name = path_cstr(Path::new(part))?;
        // SAFETY: valid dir fd and C string.
        let r = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o755) };
        if r < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
            return Err(io::Error::last_os_error());
        }
        dir = openat2(
            dir.as_fd(),
            Path::new(part),
            libc::O_PATH | libc::O_DIRECTORY,
            RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS,
        )?;
    }
    Ok(())
}

/// Opens a directory as an O_PATH root anchor.
pub fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    let c = path_cstr(path)?;
    // SAFETY: valid C string.
    let fd =
        cvt(unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) } as libc::c_long)?;
    Ok(owned(fd))
}
