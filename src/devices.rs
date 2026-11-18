//! Host device discovery, classification, and processed udev notifications.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::spec::DeviceClass;

mod udev;
#[cfg(fuzzing)]
pub use udev::fuzz;
pub use udev::{Event, Received, Sender, Watcher};

/// Host locations, overridable for tests.
#[derive(Debug, Clone)]
pub struct HostPaths {
    pub dev: PathBuf,
    pub udev_data: PathBuf,
    pub sys_dev_char: PathBuf,
}

impl HostPaths {
    pub fn system() -> Self {
        Self {
            dev: "/dev".into(),
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
    pub devpath: String,
    pub initialized: Option<u64>,
    pub current_tags: Vec<String>,
    /// Node name relative to `/dev`.
    pub devname: String,
    /// udev alias names relative to `/dev`.
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
        && !name.bytes().any(|b| b == 0 || b.is_ascii_whitespace())
        && !name.starts_with('/')
        && !name.starts_with(".psbox")
        && name.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

/// Reads one device for a listing, omitting unavailable or invalid records.
pub fn read_device(host: &HostPaths, major: u32, minor: u32) -> Option<Device> {
    read_device_checked(host, major, minor).ok().flatten()
}

/// Absence is distinct from an unreadable or invalid record, so the watcher
/// can report failures while withdrawing a device it cannot safely classify.
pub fn read_device_checked(host: &HostPaths, major: u32, minor: u32) -> io::Result<Option<Device>> {
    let read = || -> io::Result<Device> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid device database or sysfs record");
        let key = format!("{major}:{minor}");
        let db = fs::read(host.udev_data.join(format!("c{key}")))?;
        let mut db = parse_database(&db).ok_or_else(invalid)?;
        let sys = host.sys_dev_char.join(&key);
        let subsystem_link = fs::read_link(sys.join("subsystem"))?;
        let subsystem = subsystem_link
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(invalid)?;
        if !valid_subsystem(subsystem) {
            return Err(invalid());
        }
        let canonical = fs::canonicalize(&sys)?;
        let sys_root = host.sys_dev_char.parent().and_then(Path::parent).ok_or_else(invalid)?;
        let devpath = format!(
            "/{}",
            canonical
                .strip_prefix(sys_root)
                .ok()
                .and_then(|p| p.to_str())
                .ok_or_else(invalid)?
        );
        if !udev::valid_devpath(&devpath) {
            return Err(invalid());
        }
        let uevent = fs::read_to_string(sys.join("uevent"))?;
        let mut kernel = BTreeMap::new();
        for line in uevent.lines() {
            let (key, value) = line.split_once('=').ok_or_else(invalid)?;
            if !valid_key(key) || value.contains('\0') || kernel.insert(key, value).is_some() {
                return Err(invalid());
            }
        }
        if kernel.get("MAJOR").and_then(|v| v.parse::<u32>().ok()) != Some(major)
            || kernel.get("MINOR").and_then(|v| v.parse::<u32>().ok()) != Some(minor)
        {
            return Err(invalid());
        }
        let devname = kernel
            .get("DEVNAME")
            .map(|s| s.to_string())
            .or(db.devname)
            .ok_or_else(invalid)?;
        if !valid_dev_name(&devname) {
            return Err(invalid());
        }
        // udev properties augment the kernel's metadata. Optional identity
        // comes only from current sysfs, never a stale database value.
        let mut props: BTreeMap<String, String> = kernel.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
        props.append(&mut db.props);
        for key in ["DEVTYPE", "DRIVER"] {
            props.remove(key);
            if let Some(value) = kernel.get(key) {
                props.insert(key.into(), (*value).into());
            }
        }
        if !props.contains_key("DRIVER") {
            match fs::read_link(sys.join("driver")) {
                Ok(driver) => {
                    props.insert(
                        "DRIVER".into(),
                        driver.file_name().and_then(|n| n.to_str()).ok_or_else(invalid)?.into(),
                    );
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        let gamepad_hid = subsystem == "hidraw" && hid_has_gamepad(host, &sys);
        Ok(Device {
            major,
            minor,
            subsystem: subsystem.into(),
            devname,
            devpath,
            initialized: db.initialized,
            current_tags: db.current_tags,
            links: db.links,
            props,
            tags: db.tags,
            gamepad_hid,
        })
    };
    match read() {
        Ok(device) => Ok(Some(device)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[derive(Default)]
struct Database {
    devname: Option<String>,
    initialized: Option<u64>,
    links: Vec<String>,
    tags: Vec<String>,
    current_tags: Vec<String>,
    props: BTreeMap<String, String>,
}

fn sorted(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

fn valid_key(key: &str) -> bool {
    !key.is_empty() && !key.contains(['=', '\0', '\r', '\n'])
}

fn valid_subsystem(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}

fn valid_tag(tag: &str) -> bool {
    !tag.is_empty() && tag.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}

fn parse_database(bytes: &[u8]) -> Option<Database> {
    let mut db = Database::default();
    let mut initialized_seen = false;
    for line in bytes.split_inclusive(|&byte| byte == b'\n') {
        let line = match line.strip_suffix(b"\n") {
            Some(line) => line.strip_suffix(b"\r").unwrap_or(line),
            None => line,
        };
        if let Some(alias) = line.strip_prefix(b"S:") {
            if let Ok(alias) = std::str::from_utf8(alias)
                && valid_dev_name(alias)
            {
                db.links.push(alias.into());
            }
            continue;
        }
        let line = std::str::from_utf8(line).ok()?;
        if line.contains('\0') {
            return None;
        }
        let (kind, value) = line.split_once(':')?;
        match kind {
            "N" if valid_dev_name(value) && db.devname.is_none() => db.devname = Some(value.into()),
            "I" => {
                if initialized_seen {
                    return None;
                }
                initialized_seen = true;
                db.initialized = Some(value.parse::<u64>().ok()?).filter(|n| *n != 0);
            }
            "G" if valid_tag(value) => db.tags.push(value.into()),
            "Q" if valid_tag(value) => db.current_tags.push(value.into()),
            "N" | "G" | "Q" => return None,
            "E" => {
                let (key, value) = value.split_once('=')?;
                if !valid_key(key) || db.props.insert(key.into(), value.into()).is_some() {
                    return None;
                }
            }
            _ => {}
        }
    }
    sorted(&mut db.links);
    sorted(&mut db.tags);
    sorted(&mut db.current_tags);
    Some(db)
}

impl Device {
    pub fn processing(&self) -> bool {
        self.props.keys().any(|key| udev::processing_property(key.as_bytes()))
    }
}

fn sysfs_devnum(node: &Path) -> Option<(u32, u32)> {
    let dev = fs::read_to_string(node.join("dev")).ok()?;
    let (major, minor) = dev.trim().split_once(':')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// hidraw nodes whose classification depends on this input event node.
pub fn hidraw_siblings(host: &HostPaths, input: &Device) -> Vec<(u32, u32)> {
    if input.subsystem != "input" || !input.devname.starts_with("input/event") {
        return Vec::new();
    }
    let Some((hid, _)) = input.devpath.trim_start_matches('/').rsplit_once("/input/") else {
        return Vec::new();
    };
    let Some(sys) = host.sys_dev_char.parent().and_then(Path::parent) else {
        return Vec::new();
    };
    fs::read_dir(sys.join(hid).join("hidraw"))
        .into_iter()
        .flatten()
        .filter_map(|entry| sysfs_devnum(&entry.ok()?.path()))
        .collect()
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
            sysfs_devnum(&node.path())
                .and_then(|(maj, min)| read_device(host, maj, min))
                .is_some_and(|d| !d.processing() && matches(&d, DeviceClass::Gamepad))
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

/// The `/dev/.psbox` entry name for a device.
pub fn hidden_name(major: u32, minor: u32) -> String {
    format!("c{major}-{minor}")
}

/// `/dev/<name>` on the host.
pub fn host_node(host: &HostPaths, dev: &Device) -> PathBuf {
    host.dev.join(&dev.devname)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fake_host(dir: &Path) -> HostPaths {
        let host = HostPaths {
            dev: dir.join("dev"),
            udev_data: dir.join("udev"),
            sys_dev_char: dir.join("sys/dev/char"),
        };
        fs::create_dir_all(&host.udev_data).unwrap();
        fs::create_dir_all(dir.join("class")).unwrap();
        host
    }

    fn add(host: &HostPaths, root: &Path, key: &str, subsystem: &str, db: &str) {
        fs::write(host.udev_data.join(format!("c{key}")), db).unwrap();
        let sys = root.join("sys/devices").join(key);
        fs::create_dir_all(&sys).unwrap();
        fs::create_dir_all(&host.sys_dev_char).unwrap();
        symlink(&sys, host.sys_dev_char.join(key)).unwrap();
        let (major, minor) = key.split_once(':').unwrap();
        fs::write(sys.join("uevent"), format!("MAJOR={major}\nMINOR={minor}\n")).unwrap();
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
            let dir = root.join("sys/devices").join(hid);
            let node = dir.join("hidraw/hidraw");
            fs::create_dir_all(&node).unwrap();
            fs::write(node.join("dev"), hidraw).unwrap();
            let sys = host.sys_dev_char.join(hidraw);
            fs::create_dir_all(sys.parent().unwrap()).unwrap();
            symlink(&node, &sys).unwrap();
            fs::create_dir_all(root.join("class/hidraw")).unwrap();
            symlink(root.join("class/hidraw"), node.join("subsystem")).unwrap();
            let (major, minor) = hidraw.split_once(':').unwrap();
            fs::write(node.join("uevent"), format!("MAJOR={major}\nMINOR={minor}\n")).unwrap();
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
        let mut input = read_device(&host, 13, 80).unwrap();
        input.devpath = "/devices/hid-pad/input/input9/event20".into();
        assert_eq!(hidraw_siblings(&host, &input), [(243, 0)]);
        assert!(pad.gamepad_hid);
        assert_eq!(classify(&pad, &[DeviceClass::Gamepad]), vec![DeviceClass::Gamepad]);
        let key = read_device(&host, 243, 1).unwrap();
        assert!(classify(&key, &[DeviceClass::Gamepad]).is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn database_dotted_properties_kernel_metadata_and_optional_identity_are_normalized() {
        let root = tempdir("metadata");
        let host = fake_host(&root);
        add(
            &host,
            &root,
            "189:0",
            "usb",
            "I:123\nE:CUSTOM-PROPERTY=value\nE:CUSTOM.PROPERTY=dotted\nE:.INPUT_CLASS=joystick\nE:DEVTYPE=stale\nE:DRIVER=stale\nE:USEC_INITIALIZED=999\nE:WL_SEAT=game\n",
        );
        let sys = root.join("sys/devices/189:0");
        fs::write(
            sys.join("uevent"),
            "MAJOR=189\nMINOR=0\nDEVNAME=bus/usb/001/001\nPRODUCT=1234/5678/100\nBUSNUM=001\nDEVNUM=001\nTYPE=0/0/0\n",
        )
        .unwrap();
        let d = read_device_checked(&host, 189, 0).unwrap().unwrap();
        assert_eq!(d.props["CUSTOM-PROPERTY"], "value");
        assert_eq!(d.props["CUSTOM.PROPERTY"], "dotted");
        assert_eq!(d.props[".INPUT_CLASS"], "joystick");
        assert_eq!(d.props["PRODUCT"], "1234/5678/100");
        assert_eq!(d.props["BUSNUM"], "001");
        assert_eq!(d.props["DEVNUM"], "001");
        assert_eq!(d.props["TYPE"], "0/0/0");
        assert_eq!(d.props["WL_SEAT"], "game");
        assert!(!d.props.contains_key("DEVTYPE"));
        assert!(!d.props.contains_key("DRIVER"));
        assert_eq!(d.initialized, Some(123));
        symlink("/sys/bus/usb/drivers/usb", sys.join("driver")).unwrap();
        assert_eq!(
            read_device_checked(&host, 189, 0).unwrap().unwrap().props["DRIVER"],
            "usb"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_database_is_distinct_from_absence_and_does_not_skip_policy_fields() {
        let root = tempdir("invalid-db");
        let host = fake_host(&root);
        add(
            &host,
            &root,
            "13:70",
            "input",
            "N:input/event6\nE:ID_INPUT_JOYSTICK=1\n",
        );
        for bad in [
            "E:ID_INPUT_KEYBOARD",
            "E:=1",
            "G:bad tag",
            "N:../outside",
            "I:invalid",
            "EID_INPUT_KEYBOARD=1",
        ] {
            fs::write(
                host.udev_data.join("c13:70"),
                format!("N:input/event6\nE:ID_INPUT_JOYSTICK=1\n{bad}\n"),
            )
            .unwrap();
            assert_eq!(
                read_device_checked(&host, 13, 70).unwrap_err().kind(),
                io::ErrorKind::InvalidData,
                "{bad}"
            );
        }
        fs::remove_file(host.udev_data.join("c13:70")).unwrap();
        assert!(read_device_checked(&host, 13, 70).unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsupported_aliases_do_not_hide_a_valid_device() {
        let root = tempdir("unsupported-aliases");
        let host = fake_host(&root);
        add(&host, &root, "13:70", "input", "");
        fs::write(
            host.udev_data.join("c13:70"),
            b"N:input/event6\nS:.psbox/private\nS:../outside\nS:/absolute\nS:bad name\nS:bad\0name\nS:\xff\nS:input/pad\nE:ID_INPUT_JOYSTICK=1\n",
        )
        .unwrap();
        let device = read_device_checked(&host, 13, 70).unwrap().unwrap();
        assert_eq!(device.devname, "input/event6");
        assert_eq!(device.links, ["input/pad"]);
        assert!(matches(&device, DeviceClass::Gamepad));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn database_rejects_duplicate_singletons_and_deduplicates_lists() {
        for text in [
            "N:input/event6\nN:input/event6\n",
            "I:0\nI:123\n",
            "I:123\nI:456\n",
            "E:ID_INPUT_KEYBOARD=1\nE:ID_INPUT_KEYBOARD=0\n",
        ] {
            assert!(parse_database(text.as_bytes()).is_none(), "{text}");
        }
        let db = parse_database(b"S:input/pad\nS:input/pad\nG:seat\nG:seat\nQ:seat\nQ:seat\n").unwrap();
        assert_eq!(db.links, ["input/pad"]);
        assert_eq!(db.tags, ["seat"]);
        assert_eq!(db.current_tags, ["seat"]);
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
}
