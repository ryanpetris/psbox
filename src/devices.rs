//! Host device discovery and classification from the udev database.
//!
//! udev writes `/run/udev/data/c<major>:<minor>` (by atomic rename) after it
//! has finished processing a device, including permissions. One inotify
//! watch on that directory therefore reports devices that are ready to use,
//! along with the properties needed to classify them.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use crate::spec::DeviceClass;
use crate::sys::{cvt_int, path_cstr};

/// Host locations, overridable for tests.
#[derive(Debug, Clone)]
pub struct HostPaths {
    pub udev_data: PathBuf,
    pub sys_dev_char: PathBuf,
}

impl HostPaths {
    pub fn system() -> Self {
        Self {
            udev_data: "/run/udev/data".into(),
            sys_dev_char: "/sys/dev/char".into(),
        }
    }
}

/// A character device as described by udev and sysfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub major: u32,
    pub minor: u32,
    pub subsystem: String,
    /// Node name relative to `/dev`.
    pub devname: String,
    /// udev symlinks relative to `/dev`.
    pub links: Vec<String>,
    pub props: BTreeMap<String, String>,
    /// udev tags (`G:` lines), such as `uaccess` or `power-switch`.
    pub tags: Vec<String>,
    /// For `hidraw` nodes: whether the same HID device has an input device
    /// that is a gamepad.
    pub gamepad_hid: bool,
}

/// Parses a udev database file name such as `c81:2`.
pub fn parse_db_name(name: &str) -> Option<(u32, u32)> {
    let (maj, min) = name.strip_prefix('c')?.split_once(':')?;
    Some((maj.parse().ok()?, min.parse().ok()?))
}

/// A `/dev`-relative name that cannot escape `/dev` or hide in our
/// private `.psbox` directory.
pub fn valid_dev_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.starts_with(".psbox")
        && name.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

/// Reads one device. Returns `None` when it is gone or has no node.
pub fn read_device(host: &HostPaths, major: u32, minor: u32) -> Option<Device> {
    let key = format!("{major}:{minor}");
    let db = fs::read_to_string(host.udev_data.join(format!("c{key}"))).ok()?;
    let sys = host.sys_dev_char.join(&key);
    let subsystem = fs::read_link(sys.join("subsystem"))
        .ok()?
        .file_name()?
        .to_string_lossy()
        .into_owned();
    let mut devname = None;
    let mut links = Vec::new();
    let mut props = BTreeMap::new();
    let mut tags = Vec::new();
    for line in db.lines() {
        if let Some(v) = line.strip_prefix("N:") {
            devname = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("S:") {
            links.push(v.to_string());
        } else if let Some(v) = line.strip_prefix("G:") {
            tags.push(v.to_string());
        } else if let Some((k, v)) = line.strip_prefix("E:").and_then(|kv| kv.split_once('=')) {
            props.insert(k.to_string(), v.to_string());
        }
    }
    if devname.is_none() {
        let uevent = fs::read_to_string(sys.join("uevent")).ok()?;
        devname = uevent
            .lines()
            .find_map(|l| l.strip_prefix("DEVNAME="))
            .map(str::to_string);
    }
    let devname = devname.filter(|n| valid_dev_name(n))?;
    links.retain(|l| valid_dev_name(l));
    let gamepad_hid = subsystem == "hidraw" && hid_has_gamepad(host, &sys);
    Some(Device {
        major,
        minor,
        subsystem,
        devname,
        links,
        props,
        tags,
        gamepad_hid,
    })
}

/// Whether the HID device a `hidraw` node belongs to (`<hid>/hidraw/hidrawN`)
/// also has a gamepad input device (`<hid>/input/inputM/eventK`).
fn hid_has_gamepad(host: &HostPaths, hidraw: &Path) -> bool {
    let Some(hid) = fs::canonicalize(hidraw)
        .ok()
        .and_then(|p| p.parent()?.parent().map(Path::to_path_buf))
    else {
        return false;
    };
    let Ok(inputs) = fs::read_dir(hid.join("input")) else {
        return false;
    };
    inputs.flatten().any(|input| {
        let Ok(nodes) = fs::read_dir(input.path()) else {
            return false;
        };
        nodes.flatten().any(|node| {
            let name = node.file_name();
            if !name.to_string_lossy().starts_with("event") {
                return false;
            }
            let Some((maj, min)) = fs::read_to_string(node.path().join("dev")).ok().and_then(|d| {
                d.trim()
                    .split_once(':')
                    .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
            }) else {
                return false;
            };
            read_device(host, maj, min).is_some_and(|d| matches(&d, DeviceClass::Gamepad))
        })
    })
}

/// Whether `dev` belongs to `class`.
pub fn matches(dev: &Device, class: DeviceClass) -> bool {
    let prop = |k: &str| dev.props.get(k).map(String::as_str);
    match class {
        DeviceClass::Gpu => dev.subsystem == "drm" && dev.devname.starts_with("dri/renderD"),
        DeviceClass::Camera => {
            dev.subsystem == "video4linux" && prop("ID_V4L_CAPABILITIES").is_some_and(|c| c.contains(":capture:"))
        }
        // udev also marks some keyboard side interfaces as joysticks (for
        // example the "System Control" interface of a keyboard module), so
        // anything that is a keyboard or a power/sleep key source is excluded.
        DeviceClass::Gamepad => {
            (dev.subsystem == "input"
                && dev.devname.starts_with("input/")
                && prop("ID_INPUT_JOYSTICK") == Some("1")
                && prop("ID_INPUT_KEYBOARD") != Some("1")
                && !dev.tags.iter().any(|t| t == "power-switch"))
                || dev.gamepad_hid
        }
        DeviceClass::Alsa => dev.subsystem == "sound" && dev.devname.starts_with("snd/"),
        // Set by systemd's 60-fido-id.rules (fido_id reads the report
        // descriptor), only on the interface that speaks FIDO.
        DeviceClass::SecurityKey => dev.subsystem == "hidraw" && prop("ID_SECURITY_TOKEN") == Some("1"),
        DeviceClass::Usb { vendor, product } => {
            let id = |k: &str| prop(k).and_then(|v| u16::from_str_radix(v, 16).ok());
            id("ID_VENDOR_ID") == Some(vendor) && id("ID_MODEL_ID") == Some(product)
        }
        DeviceClass::Audio => false,
    }
}

/// What a listing shows about a device beyond its node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Description {
    /// USB `vendor:product`, else PCI `vendor:device`.
    pub id: Option<String>,
    /// Vendor and model when udev knows them, else the kernel driver or the
    /// device's own name.
    pub name: String,
    /// The udev `ID_PATH`, such as `pci-0000:03:00.0`.
    pub path: Option<String>,
}

/// Describes a device for `psbox devices`.
pub fn describe(host: &HostPaths, d: &Device) -> Description {
    let prop = |k: &str| d.props.get(k).filter(|v| !v.is_empty()).map(|v| v.replace('_', " "));
    let sys = host
        .sys_dev_char
        .join(format!("{}:{}", d.major, d.minor))
        .join("device");
    let read = |p: &Path| {
        fs::read_to_string(p)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let hex = |p: &Path| read(p).map(|v| v.trim_start_matches("0x").to_string());

    let usb = d.props.get("ID_VENDOR_ID").zip(d.props.get("ID_MODEL_ID"));
    let pci = || {
        [sys.clone(), sys.join("device")]
            .iter()
            .find_map(|dir| hex(&dir.join("vendor")).zip(hex(&dir.join("device"))))
    };
    let id = match usb {
        Some((v, p)) => Some(format!("{v}:{p}")),
        None => pci().map(|(v, p)| format!("{v}:{p}")),
    };
    let model = [
        prop("ID_VENDOR_FROM_DATABASE").or_else(|| prop("ID_VENDOR")),
        prop("ID_MODEL_FROM_DATABASE").or_else(|| prop("ID_MODEL")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");
    let name = Some(model)
        .filter(|m| !m.is_empty())
        .or_else(|| read(&sys.join("name")))
        .or_else(|| {
            fs::read_link(sys.join("driver"))
                .ok()
                .and_then(|l| l.file_name().map(|n| n.to_string_lossy().into_owned()))
        })
        .or_else(|| read(&sys.join("id")))
        .unwrap_or_default();
    Description {
        id,
        name,
        path: d.props.get("ID_PATH").cloned(),
    }
}

/// Classes from `classes` that `dev` belongs to.
pub fn classify(dev: &Device, classes: &[DeviceClass]) -> Vec<DeviceClass> {
    classes.iter().copied().filter(|c| matches(dev, *c)).collect()
}

/// The relative symlink target from `/dev/<name>` to `/dev/.psbox/<hidden>`.
pub fn link_target(name: &str, hidden: &str) -> String {
    let depth = name.matches('/').count();
    format!("{}.psbox/{hidden}", "../".repeat(depth))
}

/// Every character device currently in the udev database.
pub fn scan(host: &HostPaths) -> Vec<(u32, u32)> {
    let Ok(dir) = fs::read_dir(&host.udev_data) else {
        return Vec::new();
    };
    dir.filter_map(|e| e.ok())
        .filter_map(|e| parse_db_name(&e.file_name().to_string_lossy()))
        .collect()
}

/// A change reported by [`Watcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The device was added or its udev data changed.
    Updated(u32, u32),
    Removed(u32, u32),
    /// Events were lost; reconcile everything.
    Rescan,
}

/// Watches the udev database directory.
pub struct Watcher(OwnedFd);

impl Watcher {
    pub fn new(host: &HostPaths) -> io::Result<Self> {
        // SAFETY: plain syscall.
        let fd = cvt_int(unsafe { libc::inotify_init1(libc::IN_CLOEXEC) })?;
        // SAFETY: fd was just created.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let path = path_cstr(&host.udev_data)?;
        let mask = libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_CLOSE_WRITE | libc::IN_DELETE | libc::IN_MOVED_FROM;
        // SAFETY: valid fd and path.
        cvt_int(unsafe { libc::inotify_add_watch(fd.as_raw_fd(), path.as_ptr(), mask) })?;
        Ok(Self(fd))
    }

    /// Blocks for the next batch of changes.
    pub fn next(&self) -> io::Result<Vec<Change>> {
        let mut buf = [0u8; 8192];
        // SAFETY: reading into a local buffer.
        let n = unsafe { libc::read(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(parse_events(&buf[..n as usize]))
    }
}

fn parse_events(buf: &[u8]) -> Vec<Change> {
    let header = std::mem::size_of::<libc::inotify_event>();
    let mut out = Vec::new();
    let mut off = 0;
    while off + header <= buf.len() {
        // SAFETY: bounds checked above; read_unaligned copes with packing.
        let ev: libc::inotify_event = unsafe { std::ptr::read_unaligned(buf[off..].as_ptr().cast()) };
        let name_end = (off + header + ev.len as usize).min(buf.len());
        let raw = &buf[off + header..name_end];
        let name = String::from_utf8_lossy(raw.split(|b| *b == 0).next().unwrap_or_default());
        off = name_end;
        if ev.mask & libc::IN_Q_OVERFLOW != 0 {
            out.push(Change::Rescan);
            continue;
        }
        let Some((maj, min)) = parse_db_name(&name) else {
            continue;
        };
        if ev.mask & (libc::IN_DELETE | libc::IN_MOVED_FROM) != 0 {
            out.push(Change::Removed(maj, min));
        } else {
            out.push(Change::Updated(maj, min));
        }
    }
    out
}

/// The `/dev/.psbox` entry name for a device.
pub fn hidden_name(major: u32, minor: u32) -> String {
    format!("c{major}-{minor}")
}

/// `/dev/<name>` on the host.
pub fn host_node(dev: &Device) -> PathBuf {
    Path::new("/dev").join(&dev.devname)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fake_host(dir: &Path) -> HostPaths {
        let host = HostPaths {
            udev_data: dir.join("udev"),
            sys_dev_char: dir.join("sys"),
        };
        fs::create_dir_all(&host.udev_data).unwrap();
        fs::create_dir_all(dir.join("class")).unwrap();
        host
    }

    fn add(host: &HostPaths, root: &Path, key: &str, subsystem: &str, db: &str) {
        fs::write(host.udev_data.join(format!("c{key}")), db).unwrap();
        let sys = host.sys_dev_char.join(key);
        fs::create_dir_all(&sys).unwrap();
        let class = root.join("class").join(subsystem);
        fs::create_dir_all(&class).unwrap();
        symlink(&class, sys.join("subsystem")).unwrap();
    }

    fn tempdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("psbox-devtest-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn security_keys_are_the_fido_hidraw_interface() {
        let root = tempdir("fido");
        let host = fake_host(&root);
        // A security key: the FIDO interface, its OTP interface (a keyboard) as
        // hidraw and input, and an ordinary keyboard's hidraw node.
        add(
            &host,
            &root,
            "243:17",
            "hidraw",
            "N:hidraw17\nE:ID_FIDO_TOKEN=1\nE:ID_SECURITY_TOKEN=1\nG:uaccess\nG:security-device\n",
        );
        add(&host, &root, "243:16", "hidraw", "N:hidraw16\nG:seat\n");
        add(
            &host,
            &root,
            "13:256",
            "input",
            "N:input/event256\nE:ID_INPUT_KEYBOARD=1\nE:ID_VENDOR_ID=abcd\n",
        );
        add(&host, &root, "243:4", "hidraw", "N:hidraw4\n");
        let class = |key: (u32, u32)| {
            classify(
                &read_device(&host, key.0, key.1).unwrap(),
                &[DeviceClass::SecurityKey, DeviceClass::Gamepad],
            )
        };
        assert_eq!(class((243, 17)), vec![DeviceClass::SecurityKey]);
        assert!(class((243, 16)).is_empty());
        assert!(class((13, 256)).is_empty());
        assert!(class((243, 4)).is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn classifies_from_udev_and_sysfs() {
        let root = tempdir("classify");
        let host = fake_host(&root);
        add(
            &host,
            &root,
            "81:0",
            "video4linux",
            "N:video0\nS:v4l/by-id/usb-cam-video-index0\nE:ID_V4L_CAPABILITIES=:capture:\n",
        );
        add(
            &host,
            &root,
            "81:1",
            "video4linux",
            "N:video1\nE:ID_V4L_CAPABILITIES=:video_output:\n",
        );
        add(&host, &root, "226:128", "drm", "N:dri/renderD128\n");
        add(&host, &root, "226:0", "drm", "N:dri/card0\n");
        add(
            &host,
            &root,
            "13:70",
            "input",
            "N:input/event6\nE:ID_INPUT_JOYSTICK=1\n",
        );
        add(
            &host,
            &root,
            "13:64",
            "input",
            "N:input/event0\nE:ID_INPUT_KEYBOARD=1\n",
        );
        add(
            &host,
            &root,
            "13:74",
            "input",
            "N:input/event10\nE:ID_INPUT_JOYSTICK=1\nE:ID_INPUT_KEY=1\nG:power-switch\nG:uaccess\n",
        );
        add(
            &host,
            &root,
            "166:0",
            "tty",
            "N:ttyACM0\nE:ID_VENDOR_ID=0483\nE:ID_MODEL_ID=5740\n",
        );

        let all = [
            DeviceClass::Gpu,
            DeviceClass::Camera,
            DeviceClass::Gamepad,
            DeviceClass::Usb {
                vendor: 0x0483,
                product: 0x5740,
            },
        ];
        let class = |key: (u32, u32)| classify(&read_device(&host, key.0, key.1).unwrap(), &all);
        assert_eq!(class((81, 0)), vec![DeviceClass::Camera]);
        assert!(class((81, 1)).is_empty());
        assert_eq!(class((226, 128)), vec![DeviceClass::Gpu]);
        assert!(class((226, 0)).is_empty());
        assert_eq!(class((13, 70)), vec![DeviceClass::Gamepad]);
        assert!(class((13, 64)).is_empty(), "keyboards are never gamepads");
        assert!(
            class((13, 74)).is_empty(),
            "power-key interfaces flagged as joysticks are excluded"
        );
        assert_eq!(
            class((166, 0)),
            vec![DeviceClass::Usb {
                vendor: 0x0483,
                product: 0x5740
            }]
        );

        let cam = read_device(&host, 81, 0).unwrap();
        assert_eq!(cam.links, vec!["v4l/by-id/usb-cam-video-index0"]);
        let mut found = scan(&host);
        found.sort();
        assert_eq!(found.len(), 8);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn hidraw_of_a_gamepad_is_a_gamepad() {
        let root = tempdir("hidraw");
        let host = fake_host(&root);
        // A controller's HID device with a hidraw node and a joystick input,
        // and a security key's HID device with only a hidraw node.
        for (hid, hidraw, event) in [("hid-pad", "243:0", Some("13:80")), ("hid-key", "243:1", None)] {
            let dir = root.join("devices").join(hid);
            let node = dir.join("hidraw/hidraw");
            fs::create_dir_all(&node).unwrap();
            let sys = host.sys_dev_char.join(hidraw);
            fs::create_dir_all(sys.parent().unwrap()).unwrap();
            symlink(&node, &sys).unwrap();
            fs::create_dir_all(root.join("class/hidraw")).unwrap();
            symlink(root.join("class/hidraw"), node.join("subsystem")).unwrap();
            fs::write(
                host.udev_data.join(format!("c{hidraw}")),
                format!("N:hidraw{}\n", &hidraw[4..]),
            )
            .unwrap();
            if let Some(ev) = event {
                let ev_dir = dir.join("input/input9/event20");
                fs::create_dir_all(&ev_dir).unwrap();
                fs::write(ev_dir.join("dev"), format!("{ev}\n")).unwrap();
                add(&host, &root, ev, "input", "N:input/event20\nE:ID_INPUT_JOYSTICK=1\n");
            }
        }
        let pad = read_device(&host, 243, 0).unwrap();
        assert!(pad.gamepad_hid);
        assert_eq!(classify(&pad, &[DeviceClass::Gamepad]), vec![DeviceClass::Gamepad]);
        let key = read_device(&host, 243, 1).unwrap();
        assert!(classify(&key, &[DeviceClass::Gamepad]).is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rejects_unsafe_names() {
        assert!(valid_dev_name("dri/renderD128"));
        assert!(!valid_dev_name("../etc/passwd"));
        assert!(!valid_dev_name("/abs"));
        assert!(!valid_dev_name(".psbox/x"));
        assert!(!valid_dev_name("a//b"));
    }

    #[test]
    fn link_targets_are_relative() {
        assert_eq!(link_target("video0", "c81-0"), ".psbox/c81-0");
        assert_eq!(link_target("dri/renderD128", "c226-128"), "../.psbox/c226-128");
        assert_eq!(link_target("v4l/by-id/x", "c81-0"), "../../.psbox/c81-0");
    }

    #[test]
    fn parses_names_and_events() {
        assert_eq!(parse_db_name("c81:2"), Some((81, 2)));
        assert_eq!(parse_db_name("b8:0"), None);
        assert_eq!(parse_db_name("+usb:1-1"), None);

        let mut buf = Vec::new();
        for (mask, name) in [
            (libc::IN_MOVED_TO, "c81:0"),
            (libc::IN_DELETE, "c13:70"),
            (libc::IN_CREATE, ".#c1:2tmp"),
        ] {
            let mut padded = name.as_bytes().to_vec();
            padded.resize(16, 0);
            let ev = libc::inotify_event {
                wd: 1,
                mask,
                cookie: 0,
                len: padded.len() as u32,
            };
            // SAFETY: inotify_event is plain old data.
            let bytes: [u8; std::mem::size_of::<libc::inotify_event>()] = unsafe { std::mem::transmute(ev) };
            buf.extend_from_slice(&bytes);
            buf.extend_from_slice(&padded);
        }
        assert_eq!(
            parse_events(&buf),
            vec![Change::Updated(81, 0), Change::Removed(13, 70)]
        );
    }
}
