//! Device mounts and notifications based on current host state.

use super::*;
use std::collections::BTreeSet;

type Key = (u32, u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NodeId {
    filesystem: u64,
    inode: u64,
}

impl NodeId {
    fn from_metadata(meta: &fs::Metadata) -> Self {
        Self {
            filesystem: meta.dev(),
            inode: meta.ino(),
        }
    }
}

fn node_id(dev: &devices::Device, meta: &fs::Metadata) -> Result<NodeId> {
    if meta.mode() & libc::S_IFMT != libc::S_IFCHR || meta.rdev() != libc::makedev(dev.major, dev.minor) {
        bail!(
            "/dev/{} is not character device {}:{}",
            dev.devname,
            dev.major,
            dev.minor
        );
    }
    Ok(NodeId::from_metadata(meta))
}

#[derive(Clone)]
struct Snapshot {
    device: devices::Device,
    node: NodeId,
}

impl Snapshot {
    fn same_node(&self, other: &Self) -> bool {
        self.node == other.node && self.device.devname == other.device.devname
    }
}

struct Published {
    snapshot: Snapshot,
    classes: Vec<String>,
    names: Vec<String>,
    /// A complete publication owes a remove even if the add send failed.
    exposed: bool,
    /// Cleanup must finish before this key can publish again.
    cleaning_up: bool,
    mounted: bool,
}

impl Published {
    fn aliases(&self) -> impl Iterator<Item = &String> {
        self.names.iter().filter(|name| **name != self.snapshot.device.devname)
    }
}

#[derive(Default)]
pub(super) struct DeviceState {
    published: BTreeMap<Key, Published>,
}

impl DeviceState {
    fn status(&self) -> Vec<PublishedDevice> {
        self.published
            .values()
            .filter(|p| p.mounted)
            .map(|p| PublishedDevice {
                node: if p.names.contains(&p.snapshot.device.devname) {
                    p.snapshot.device.devname.clone()
                } else {
                    format!(
                        ".psbox/{}",
                        devices::hidden_name(p.snapshot.device.major, p.snapshot.device.minor)
                    )
                },
                links: p.aliases().cloned().collect(),
                classes: p.classes.clone(),
            })
            .collect()
    }
}

trait DeviceIo {
    fn keys(&self) -> Vec<Key>;
    fn snapshot(&self, key: Key) -> Result<Option<Snapshot>>;
    fn hidraw_siblings(&self, input: &devices::Device) -> Vec<Key>;
    fn alias_target(&self, name: &str) -> Option<(Key, NodeId)>;
    fn attach(&mut self, snapshot: Snapshot, policy: &[DeviceClass]) -> Result<Snapshot>;
    fn link_matches(&self, name: &str, target: &str) -> io::Result<bool>;
    fn link(&mut self, name: &str, target: &str) -> io::Result<()>;
    fn unlink(&mut self, name: &str, target: &str) -> io::Result<()>;
    fn detach(&mut self, hidden: &str) -> Result<()>;
    fn send(&mut self, device: &devices::Device, action: &str) -> io::Result<()>;
    fn report(&mut self, message: &str);
}

struct HostDevices<'a> {
    host: &'a HostPaths,
    dev_rw: BorrowedFd<'a>,
    init_pidfd: BorrowedFd<'a>,
    events: &'a mut DeviceEvents,
    key: &'a str,
}

impl DeviceIo for HostDevices<'_> {
    fn keys(&self) -> Vec<Key> {
        devices::scan(self.host)
    }

    fn snapshot(&self, key: Key) -> Result<Option<Snapshot>> {
        let Some(device) = devices::read_device_checked(self.host, key.0, key.1)? else {
            return Ok(None);
        };
        let meta = match fs::metadata(devices::host_node(self.host, &device)) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let node = node_id(&device, &meta)?;
        Ok(Some(Snapshot { device, node }))
    }

    fn hidraw_siblings(&self, input: &devices::Device) -> Vec<Key> {
        devices::hidraw_siblings(self.host, input)
    }

    fn alias_target(&self, name: &str) -> Option<(Key, NodeId)> {
        if root::reserved_dev_name(name) {
            return None;
        }
        let meta = fs::metadata(self.host.dev.join(name)).ok()?;
        (meta.mode() & libc::S_IFMT == libc::S_IFCHR).then(|| {
            let rdev = meta.rdev();
            ((libc::major(rdev), libc::minor(rdev)), NodeId::from_metadata(&meta))
        })
    }

    fn attach(&mut self, snapshot: Snapshot, policy: &[DeviceClass]) -> Result<Snapshot> {
        let dev = &snapshot.device;
        let tree = mount::clone_tree(&devices::host_node(self.host, dev), false).context("clone device node")?;
        let actual = node_id(dev, &fs::File::from(tree.try_clone()?).metadata()?)?;
        // Re-read policy inputs and links after cloning, in case the host node
        // changed between the database read and opening the mount.
        let current = self.snapshot((dev.major, dev.minor))?.filter(|current| {
            actual == current.node
                && current.same_node(&snapshot)
                && !current.device.processing()
                && policy.iter().any(|&class| devices::matches(&current.device, class))
        });
        let Some(current) = current else {
            bail!("/dev/{} changed while preparing its mount", dev.devname);
        };
        mount::set_attr_fd(tree.as_fd(), false, MOUNT_ATTR_NOSUID, 0)?;
        let hidden_rel = Path::new(".psbox").join(devices::hidden_name(dev.major, dev.minor));
        mount::make_point(self.dev_rw, &hidden_rel, PointKind::File)?;
        inject::attach(self.init_pidfd, tree.as_fd(), &Path::new("/dev").join(&hidden_rel)).context("attach node")?;
        Ok(current)
    }

    fn link(&mut self, name: &str, target: &str) -> io::Result<()> {
        publish_link(self.dev_rw, name, target)
    }

    fn link_matches(&self, name: &str, target: &str) -> io::Result<bool> {
        link_matches(self.dev_rw, name, target)
    }

    fn unlink(&mut self, name: &str, target: &str) -> io::Result<()> {
        unlink_if_ours(self.dev_rw, name, target)
    }

    fn detach(&mut self, hidden: &str) -> Result<()> {
        let hidden_rel = Path::new(".psbox").join(hidden);
        inject::detach(self.init_pidfd, &Path::new("/dev").join(&hidden_rel)).context("detach node")?;
        let c = path_cstr(&hidden_rel)?;
        // SAFETY: valid directory fd and relative path. Only the empty mount point remains.
        if unsafe { libc::unlinkat(self.dev_rw.as_raw_fd(), c.as_ptr(), 0) } < 0 {
            let e = io::Error::last_os_error();
            self.report(&format!("remove empty device mount point: {e}"));
        }
        Ok(())
    }

    fn send(&mut self, device: &devices::Device, action: &str) -> io::Result<()> {
        self.events.send(device, action)
    }

    fn report(&mut self, message: &str) {
        eprintln!("psbox[{}]: {message}", self.key);
    }
}

pub(super) struct DeviceEvents {
    sender: Option<devices::Sender>,
    seq: u64,
}

impl DeviceEvents {
    pub(super) fn new(sender: Option<devices::Sender>) -> Self {
        Self { sender, seq: 0 }
    }

    fn send(&mut self, dev: &devices::Device, action: &str) -> io::Result<()> {
        if let Some(sender) = &self.sender {
            self.seq = self
                .seq
                .checked_add(1)
                .ok_or_else(|| io::Error::other("udev sequence exhausted"))?;
            sender.send(dev, action, self.seq)?;
        }
        Ok(())
    }
}

struct Reconcile<'a> {
    state: &'a mut DeviceState,
    io: &'a mut dyn DeviceIo,
    policy: &'a [DeviceClass],
}

impl Reconcile<'_> {
    fn update(&mut self, event: Option<devices::Event>) {
        match event {
            Some(event) => self.check(event.key, !event.failed),
            None => self.scan(),
        }
    }

    fn scan(&mut self) {
        let mut keys = self.io.keys();
        keys.extend(self.state.published.keys().copied());
        keys.sort_unstable();
        keys.dedup();
        for key in keys {
            let snapshot = self.snapshot(key);
            self.check_snapshot(key, snapshot, true);
        }
    }

    fn check(&mut self, key: Key, may_publish: bool) {
        let snapshot = self.snapshot(key);
        let may_publish = may_publish && snapshot.as_ref().is_none_or(|s| !s.device.processing());
        let mut related = self.alias_neighbors(key, snapshot.as_ref());
        if !may_publish {
            // A failed worker can leave no database entry naming the alias it took over.
            related.extend(self.state.published.iter().filter_map(|(&owner, p)| {
                (owner != key
                    && !p.cleaning_up
                    && p.aliases()
                        .any(|name| self.io.alias_target(name) != Some((owner, p.snapshot.node))))
                .then_some(owner)
            }));
        }
        if self.policy.contains(&DeviceClass::Gamepad) {
            let previous = self.state.published.get(&key).map(|p| &p.snapshot.device);
            let current = snapshot.as_ref().map(|s| &s.device);
            if previous.is_none() && current.is_none() {
                // A vanished input record could have qualified a hidraw node even if its own mount failed.
                related.extend(
                    self.state
                        .published
                        .iter()
                        .filter_map(|(&key, p)| p.snapshot.device.gamepad_hid.then_some(key)),
                );
            }
            for input in previous
                .filter(|old| current.is_none_or(|new| new.devpath != old.devpath))
                .into_iter()
                .chain(current)
            {
                related.extend(self.io.hidraw_siblings(input));
            }
        }
        self.check_snapshot(key, snapshot, may_publish);
        for key in related {
            let snapshot = self.snapshot(key);
            self.check_snapshot(key, snapshot, may_publish);
        }
    }

    fn alias_neighbors(&self, key: Key, snapshot: Option<&Snapshot>) -> BTreeSet<Key> {
        let previous = self.state.published.get(&key);
        let mut related = BTreeSet::new();
        if previous.is_none() {
            // An unpublished device can give up a claim on removal or a rules change.
            related.extend(self.state.published.iter().filter_map(|(&key, p)| {
                let target = Some((key, p.snapshot.node));
                (!p.cleaning_up
                    && p.snapshot
                        .device
                        .links
                        .iter()
                        .any(|name| !p.names.contains(name) && self.io.alias_target(name) == target))
                .then_some(key)
            }));
        }

        let aliases: BTreeSet<_> = previous
            .map(|p| &p.snapshot)
            .into_iter()
            .chain(snapshot)
            .flat_map(|s| &s.device.links)
            .collect();
        for name in aliases {
            let target = self.io.alias_target(name).map(|(key, _)| key);
            related.extend(target);
            related.extend(
                self.state
                    .published
                    .iter()
                    .filter_map(|(&owner, p)| (Some(owner) != target && p.names.contains(name)).then_some(owner)),
            );
        }
        related.remove(&key);
        related
    }

    fn snapshot(&mut self, key: Key) -> Option<Snapshot> {
        match self.io.snapshot(key) {
            Ok(snapshot) => snapshot,
            Err(e) => {
                self.io.report(&format!("read device {}:{}: {e:#}", key.0, key.1));
                None
            }
        }
    }

    fn check_snapshot(&mut self, key: Key, snapshot: Option<Snapshot>, may_publish: bool) {
        let mut written = Vec::new();
        self.reconcile(key, snapshot, may_publish, &mut written);
        if !written.is_empty() {
            // A new link can displace an alias before its prior owner is checked.
            for (&owner, p) in &mut self.state.published {
                if owner != key {
                    p.names.retain(|name| !written.contains(name));
                }
            }
        }
    }

    fn reconcile(&mut self, key: Key, snapshot: Option<Snapshot>, may_publish: bool, written: &mut Vec<String>) {
        let desired = snapshot.and_then(|snapshot| {
            let classes = devices::classify(&snapshot.device, self.policy);
            (!classes.is_empty()).then_some((snapshot, classes))
        });
        if let Some(p) = self.state.published.get_mut(&key) {
            if let Some((current, classes)) = &desired
                && !p.cleaning_up
                && p.snapshot.same_node(current)
            {
                let may_add = may_publish && !current.device.processing();
                if let Err(e) = refresh_links(self.io, &mut p.names, current, written, may_add) {
                    self.io
                        .report(&format!("update /dev/{}: {e:#}", current.device.devname));
                    self.remove(key);
                    return;
                }
                if may_add {
                    p.snapshot = current.clone();
                    p.classes = classes.iter().map(ToString::to_string).collect();
                }
                return;
            }
            if !self.remove(key) {
                return;
            }
        }
        if let Some((snapshot, _)) = desired
            && may_publish
            && !snapshot.device.processing()
        {
            self.publish(key, snapshot, written);
        }
    }

    fn publish(&mut self, key: Key, snapshot: Snapshot, written: &mut Vec<String>) {
        let name = snapshot.device.devname.clone();
        let snapshot = match self.io.attach(snapshot, self.policy) {
            Ok(snapshot) => snapshot,
            Err(e) => {
                self.io.report(&format!("attach /dev/{name}: {e:#}"));
                return;
            }
        };
        let dev = &snapshot.device;
        let classes: Vec<_> = devices::classify(dev, self.policy)
            .iter()
            .map(ToString::to_string)
            .collect();
        let mut names = Vec::new();
        let linked = refresh_links(self.io, &mut names, &snapshot, written, true);
        if linked.is_ok() {
            self.io
                .report(&format!("published /dev/{name} ({})", classes.join(", ")));
            let mut dev = dev.clone();
            dev.links = names.iter().filter(|name| **name != dev.devname).cloned().collect();
            if let Err(e) = self.io.send(&dev, "add") {
                self.io.report(&format!("notify add /dev/{name}: {e}"));
            }
        }
        self.state.published.insert(
            key,
            Published {
                snapshot,
                classes,
                names,
                exposed: linked.is_ok(),
                cleaning_up: false,
                mounted: true,
            },
        );
        if let Err(e) = linked {
            self.io.report(&format!("publish /dev/{name}: {e:#}"));
            self.remove(key);
        }
    }

    fn remove(&mut self, key: Key) -> bool {
        let p = self.state.published.get_mut(&key).expect("device to remove");
        if !p.cleaning_up {
            // Keep the exposed aliases for the remove event across cleanup retries.
            p.snapshot.device.links = p.aliases().cloned().collect();
        }
        p.cleaning_up = true;
        let hidden = devices::hidden_name(key.0, key.1);
        p.names.retain(
            |name| match self.io.unlink(name, &devices::link_target(name, &hidden)) {
                Ok(()) => false,
                Err(e) => {
                    self.io.report(&format!("unlink /dev/{name}: {e}"));
                    true
                }
            },
        );
        if p.mounted {
            match self.io.detach(&hidden) {
                Ok(()) => p.mounted = false,
                Err(e) => self
                    .io
                    .report(&format!("detach /dev/{}: {e:#}", p.snapshot.device.devname)),
            }
        }
        if p.mounted || !p.names.is_empty() {
            return false;
        }

        let p = self.state.published.remove(&key).expect("cleaned-up device");
        let dev = &p.snapshot.device;
        if p.exposed {
            if let Err(e) = self.io.send(dev, "remove") {
                self.io.report(&format!("notify remove /dev/{}: {e}", dev.devname));
            }
            self.io.report(&format!("removed /dev/{}", dev.devname));
        }
        true
    }
}

fn refresh_links(
    io: &mut dyn DeviceIo,
    published: &mut Vec<String>,
    snapshot: &Snapshot,
    written: &mut Vec<String>,
    may_add: bool,
) -> Result<()> {
    let dev = &snapshot.device;
    let owner = ((dev.major, dev.minor), snapshot.node);
    let hidden = devices::hidden_name(dev.major, dev.minor);
    let mut names = vec![dev.devname.clone()];
    names.extend(
        dev.links
            .iter()
            .filter(|name| **name != dev.devname && io.alias_target(name) == Some(owner))
            .cloned(),
    );
    if !may_add {
        names.retain(|name| published.contains(name));
    }
    for name in &names {
        let target = devices::link_target(name, &hidden);
        if may_add && !io.link_matches(name, &target)? {
            io.link(name, &target).with_context(|| format!("link /dev/{name}"))?;
            written.push(name.clone());
        }
        if !published.contains(name) {
            published.push(name.clone());
        }
    }
    for name in published.iter().filter(|name| !names.contains(name)) {
        io.unlink(name, &devices::link_target(name, &hidden))
            .with_context(|| format!("unlink /dev/{name}"))?;
    }
    *published = names;
    Ok(())
}

impl Supervisor {
    pub(super) fn update_devices(&self, host: &HostPaths, event: Option<devices::Event>) {
        let mut state = self.reconciling.lock().unwrap();
        let plan = self.plan();
        let mut events = self.device_events.lock().unwrap();
        let mut io = HostDevices {
            host,
            dev_rw: self.dev_rw.as_fd(),
            init_pidfd: self.init_pidfd.as_fd(),
            events: &mut events,
            key: &plan.key,
        };
        let mut reconcile = Reconcile {
            state: &mut state,
            io: &mut io,
            policy: &plan.node_classes,
        };
        reconcile.update(event);
        self.state.lock().unwrap().devices = state.status();
    }

    pub(super) fn watch_devices(&self, watcher: devices::Watcher) {
        let host = HostPaths::system();
        let run = || -> io::Result<()> {
            let mut recovery_reported = false;
            loop {
                let ready = poll_readable(&[watcher.as_fd(), self.init_pidfd.as_fd()], -1)?;
                if ready[1] {
                    return Ok(());
                }
                let mut recovery = None;
                // Coalesce recovery within a batch without waiting for host traffic to stop.
                for _ in 0..64 {
                    match watcher.receive() {
                        Ok(devices::Received::Event(event)) => self.update_devices(&host, Some(event)),
                        Ok(devices::Received::Ignored) => {}
                        Ok(devices::Received::Invalid) => {
                            recovery = Some("malformed or truncated processed udev event");
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) if e.raw_os_error() == Some(libc::ENOBUFS) => {
                            recovery = Some("processed udev events lost");
                            break;
                        }
                        Err(e) => return Err(e),
                    }
                }
                if let Some(reason) = recovery {
                    if !recovery_reported {
                        eprintln!("psbox[{}]: {reason}; reconciling devices", self.key());
                        recovery_reported = true;
                    }
                    self.update_devices(&host, None);
                }
            }
        };
        if let Err(e) = run() {
            eprintln!("psbox[{}]: device watch failed: {e}", self.key());
        }
    }
}

/// Atomically creates or replaces `/dev/<name>` as a symlink to `target`.
/// The device is already attached at the target, so the first event an app
/// sees for `name` refers to a working node.
fn publish_link(dev: BorrowedFd<'_>, name: &str, target: &str) -> io::Result<()> {
    let path = Path::new(name);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        mount::make_point(dev, parent, PointKind::Dir)?;
    }
    let tmp = c".psbox/.link";
    let target = path_cstr(Path::new(target))?;
    let dst = path_cstr(path)?;
    // SAFETY: valid dirfd and C strings.
    unsafe {
        libc::unlinkat(dev.as_raw_fd(), tmp.as_ptr(), 0);
        cvt_int(libc::symlinkat(target.as_ptr(), dev.as_raw_fd(), tmp.as_ptr()))?;
        if let Err(e) = cvt_int(libc::renameat(
            dev.as_raw_fd(),
            tmp.as_ptr(),
            dev.as_raw_fd(),
            dst.as_ptr(),
        )) {
            libc::unlinkat(dev.as_raw_fd(), tmp.as_ptr(), 0);
            return Err(e);
        }
    }
    Ok(())
}

fn link_matches(dev: BorrowedFd<'_>, name: &str, target: &str) -> io::Result<bool> {
    let c = path_cstr(Path::new(name))?;
    let mut buf = [0u8; 4096];
    // SAFETY: reading the link into a local buffer.
    let n = unsafe { libc::readlinkat(dev.as_raw_fd(), c.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        let e = io::Error::last_os_error();
        return match e.raw_os_error() {
            Some(libc::ENOENT | libc::EINVAL) => Ok(false),
            _ => Err(e),
        };
    }
    Ok(&buf[..n as usize] == target.as_bytes())
}

fn unlink_if_ours(dev: BorrowedFd<'_>, name: &str, target: &str) -> io::Result<()> {
    if link_matches(dev, name, target)? {
        let c = path_cstr(Path::new(name))?;
        // SAFETY: valid dirfd and path.
        cvt_int(unsafe { libc::unlinkat(dev.as_raw_fd(), c.as_ptr(), 0) })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: Key = (13, 70);
    const OTHER: Key = (13, 71);
    const NODE: &str = "input/event6";
    const LINK: &str = "input/by-id/pad";

    fn snapshot() -> Snapshot {
        Snapshot {
            device: devices::Device {
                major: KEY.0,
                minor: KEY.1,
                subsystem: "input".into(),
                devpath: "/devices/input/input6/event6".into(),
                initialized: Some(123),
                current_tags: vec!["seat".into()],
                devname: NODE.into(),
                links: vec![LINK.into()],
                props: BTreeMap::from([("ID_INPUT_JOYSTICK".into(), "1".into())]),
                tags: vec!["seat".into()],
                gamepad_hid: false,
            },
            node: NodeId {
                filesystem: 1,
                inode: 9,
            },
        }
    }

    #[derive(Default)]
    struct Test {
        state: DeviceState,
        io: Fake,
    }

    #[derive(Default)]
    struct Fake {
        host: BTreeMap<Key, Snapshot>,
        alias_targets: BTreeMap<String, Option<(Key, NodeId)>>,
        mounts: HashSet<Key>,
        links: BTreeMap<String, String>,
        operations: Vec<String>,
        notifications: Vec<(String, devices::Device)>,
        reports: Vec<String>,
        hidraw: Vec<Key>,
        fail_link: Option<&'static str>,
        fail_unlink: bool,
        fail_detach: bool,
        fail_send: bool,
        fail_read: bool,
    }

    impl DeviceIo for Fake {
        fn keys(&self) -> Vec<Key> {
            self.host.keys().copied().collect()
        }

        fn snapshot(&self, key: Key) -> Result<Option<Snapshot>> {
            if self.fail_read {
                bail!("invalid database");
            }
            Ok(self.host.get(&key).cloned())
        }

        fn hidraw_siblings(&self, input: &devices::Device) -> Vec<Key> {
            if input.subsystem == "input" {
                self.hidraw.clone()
            } else {
                Vec::new()
            }
        }

        fn alias_target(&self, name: &str) -> Option<(Key, NodeId)> {
            if let Some(target) = self.alias_targets.get(name) {
                return *target;
            }
            let mut claimants = self
                .host
                .iter()
                .filter(|(_, s)| s.device.links.iter().any(|alias| alias == name));
            let (&key, snapshot) = claimants.next()?;
            assert!(claimants.next().is_none(), "set the host target for a shared alias");
            Some((key, snapshot.node))
        }

        fn attach(&mut self, snapshot: Snapshot, _: &[DeviceClass]) -> Result<Snapshot> {
            self.operations.push(format!("attach {}", snapshot.node.inode));
            assert!(self.mounts.insert((snapshot.device.major, snapshot.device.minor)));
            Ok(snapshot)
        }

        fn link(&mut self, name: &str, target: &str) -> io::Result<()> {
            self.operations.push(format!("link {name}"));
            if self.fail_link == Some(name) {
                return Err(io::Error::other("link failed"));
            }
            self.links.insert(name.into(), target.into());
            Ok(())
        }

        fn link_matches(&self, name: &str, target: &str) -> io::Result<bool> {
            Ok(self.links.get(name).is_some_and(|actual| actual == target))
        }

        fn unlink(&mut self, name: &str, target: &str) -> io::Result<()> {
            self.operations.push(format!("unlink {name}"));
            if self.fail_unlink {
                return Err(io::Error::other("unlink failed"));
            }
            if self.links.get(name).is_some_and(|actual| actual == target) {
                self.links.remove(name);
            }
            Ok(())
        }

        fn detach(&mut self, hidden: &str) -> Result<()> {
            self.operations.push("detach".into());
            if self.fail_detach {
                bail!("detach failed");
            }
            let key = *self
                .mounts
                .iter()
                .find(|&&(a, b)| devices::hidden_name(a, b) == hidden)
                .unwrap();
            assert!(self.mounts.remove(&key));
            Ok(())
        }

        fn send(&mut self, d: &devices::Device, action: &str) -> io::Result<()> {
            self.operations.push(action.into());
            let mounted = self.mounts.contains(&(d.major, d.minor));
            let target = |name: &str| devices::link_target(name, &devices::hidden_name(d.major, d.minor));
            for name in std::iter::once(&d.devname).chain(&d.links) {
                assert_eq!(
                    self.links.get(name).is_some_and(|actual| actual == &target(name)),
                    mounted
                );
            }
            assert_eq!(mounted, action == "add");
            self.notifications.push((action.into(), d.clone()));
            if self.fail_send {
                Err(io::Error::other("send failed"))
            } else {
                Ok(())
            }
        }

        fn report(&mut self, message: &str) {
            self.reports.push(message.into());
        }
    }

    impl Test {
        fn reconcile(&mut self) -> Reconcile<'_> {
            Reconcile {
                state: &mut self.state,
                io: &mut self.io,
                policy: &[DeviceClass::Gamepad],
            }
        }

        fn shared_alias(target: Key) -> Self {
            let mut t = Self::default();
            let first = snapshot();
            let mut second = first.clone();
            second.device.minor = OTHER.1;
            second.device.devname = "input/event7".into();
            second.node.inode += 1;
            t.io.host.extend([(KEY, first), (OTHER, second)]);
            t.set_alias_target(target);
            t
        }

        fn set_alias_target(&mut self, key: Key) {
            self.io
                .alias_targets
                .insert(LINK.into(), Some((key, self.io.host[&key].node)));
        }

        fn assert_alias_owner(&self, key: Key) {
            let target = devices::link_target(LINK, &devices::hidden_name(key.0, key.1));
            assert_eq!(self.io.links[LINK], target);
            for (&owner, p) in &self.state.published {
                assert_eq!(p.aliases().any(|alias| alias == LINK), owner == key);
            }
        }
    }

    #[test]
    fn event_hints_use_current_state() {
        let mut t = Test::default();
        // A vanished add and a remove for a key never seen both find nothing.
        for key in [KEY, OTHER] {
            t.reconcile().check(key, true);
        }
        assert!(t.io.operations.is_empty());

        t.io.host.insert(KEY, snapshot());
        t.reconcile().check(KEY, true);
        assert_eq!(
            t.io.operations,
            ["attach 9", "link input/event6", "link input/by-id/pad", "add"]
        );
        t.io.operations.clear();

        // A late remove, another add, and a scan all find the same live node.
        t.reconcile().check(KEY, true);
        t.reconcile().check(KEY, true);
        t.reconcile().scan();
        assert!(t.io.operations.is_empty());
        assert_eq!(t.io.notifications.len(), 1);
    }

    #[test]
    fn same_node_refreshes_changed_links_and_cached_metadata() {
        let mut t = Test::default();
        t.io.host.insert(KEY, snapshot());
        t.reconcile().scan();
        t.io.operations.clear();

        let d = &mut t.io.host.get_mut(&KEY).unwrap().device;
        d.devpath = "/devices/input/other/event6".into();
        d.initialized = Some(456);
        d.links = vec!["input/by-path/pad".into()];
        d.props.insert("WL_SEAT".into(), "game".into());
        let updated = d.clone();
        t.reconcile().check(KEY, true);
        assert_eq!(t.io.operations, ["link input/by-path/pad", "unlink input/by-id/pad"]);
        assert_eq!(t.io.notifications.len(), 1);

        t.io.host.clear();
        t.io.operations.clear();
        t.reconcile().scan();
        assert_eq!(
            t.io.operations,
            ["unlink input/event6", "unlink input/by-path/pad", "detach", "remove"]
        );
        assert_eq!(t.io.notifications[1], ("remove".into(), updated));
    }

    #[test]
    fn scans_publish_and_restore_the_host_alias_winner() {
        for key in [KEY, OTHER] {
            let mut t = Test::shared_alias(key);
            t.reconcile().scan();
            t.assert_alias_owner(key);
            assert_eq!(t.io.notifications.len(), 2);
            for (_, device) in &t.io.notifications {
                assert_eq!(device.links == [LINK], (device.major, device.minor) == key);
            }

            t.io.links.remove(LINK);
            t.reconcile().scan();
            t.assert_alias_owner(key);

            // A reused device number alone does not identify the mounted node.
            let (_, mut node) = t.io.alias_targets[LINK].unwrap();
            node.inode += 20;
            t.io.alias_targets.insert(LINK.into(), Some((key, node)));
            t.reconcile().scan();
            assert!(!t.io.links.contains_key(LINK));

            t.set_alias_target(key);
            t.reconcile().scan();
            t.assert_alias_owner(key);
            assert_eq!(t.io.notifications.len(), 2);
        }
    }

    #[test]
    fn shared_alias_handoffs_on_change_and_remove_keep_the_current_winner() {
        let mut t = Test::shared_alias(KEY);
        t.reconcile().scan();
        t.assert_alias_owner(KEY);

        // The old winner's event is enough to refresh the new winner.
        t.io.host.get_mut(&KEY).unwrap().device.links.clear();
        t.set_alias_target(OTHER);
        t.reconcile().check(KEY, true);
        t.assert_alias_owner(OTHER);
        t.io.host.get_mut(&KEY).unwrap().device.links.push(LINK.into());
        t.reconcile().check(KEY, true);
        t.assert_alias_owner(OTHER);
        assert_eq!(t.io.notifications.len(), 2);

        t.io.host.remove(&OTHER);
        t.set_alias_target(KEY);
        t.reconcile().check(OTHER, true);
        t.assert_alias_owner(KEY);
        t.reconcile().check(OTHER, true);
        t.assert_alias_owner(KEY);
        assert_eq!(t.io.notifications.len(), 3);
        let (action, device) = &t.io.notifications[2];
        assert_eq!(action, "remove");
        assert_eq!(device.minor, OTHER.1);
        assert_eq!(device.links, [LINK]);
    }

    #[test]
    fn an_unpublished_device_can_release_a_shared_alias() {
        for removed in [false, true] {
            let mut t = Test::shared_alias(OTHER);
            t.io.host
                .get_mut(&OTHER)
                .unwrap()
                .device
                .props
                .insert("ID_INPUT_KEYBOARD".into(), "1".into());
            t.reconcile().scan();
            assert!(!t.io.links.contains_key(LINK));
            assert_eq!(t.io.notifications.len(), 1);

            if removed {
                t.io.host.remove(&OTHER);
            } else {
                t.io.host.get_mut(&OTHER).unwrap().device.links.clear();
            }
            t.set_alias_target(KEY);
            t.reconcile().check(OTHER, true);
            t.assert_alias_owner(KEY);
            assert_eq!(t.io.notifications.len(), 1);
        }
    }

    #[test]
    fn replacement_removes_before_adding() {
        for change in ["name", "inode", "filesystem"] {
            let mut t = Test::default();
            t.io.host.insert(KEY, snapshot());
            t.reconcile().scan();
            t.io.operations.clear();
            let replacement = t.io.host.get_mut(&KEY).unwrap();
            match change {
                "name" => replacement.device.devname = "input/event7".into(),
                "inode" => replacement.node.inode += 1,
                "filesystem" => replacement.node.filesystem += 1,
                _ => unreachable!(),
            }
            t.reconcile().check(KEY, true);
            assert_eq!(
                t.io.operations
                    .iter()
                    .map(|op| op.split_whitespace().next().unwrap())
                    .collect::<Vec<_>>(),
                ["unlink", "unlink", "detach", "remove", "attach", "link", "link", "add"],
                "{change}"
            );
            assert_eq!(t.io.notifications[1], ("remove".into(), snapshot().device));
        }
    }

    #[test]
    fn failed_hints_and_processing_only_withdraw() {
        let mut t = Test::default();
        t.io.host.insert(KEY, snapshot());
        t.reconcile().check(KEY, false);
        let d = &mut t.io.host.get_mut(&KEY).unwrap().device;
        d.props.insert("ID_PROCESSING".into(), "1".into());
        t.reconcile().scan();
        t.reconcile().check(KEY, true);
        assert!(t.io.operations.is_empty());

        t.io.host.get_mut(&KEY).unwrap().device.props.remove("ID_PROCESSING");
        t.reconcile().check(KEY, true);
        t.io.host
            .get_mut(&KEY)
            .unwrap()
            .device
            .props
            .insert("ID_INPUT_KEYBOARD".into(), "1".into());
        t.reconcile().check(KEY, false);
        assert_eq!(t.io.operations.last().unwrap(), "remove");
        assert!(t.state.published.is_empty());

        t.io.host.insert(KEY, snapshot());
        t.reconcile().scan();
        t.io.fail_read = true;
        t.reconcile().check(KEY, true);
        assert_eq!(t.io.operations.last().unwrap(), "remove");
        assert!(t.io.reports.iter().any(|msg| msg.contains("invalid database")));
    }

    #[test]
    fn failed_new_alias_winner_withdraws_the_previous_link() {
        let mut t = Test::shared_alias(KEY);
        t.reconcile().check(KEY, true);
        t.assert_alias_owner(KEY);
        t.set_alias_target(OTHER);
        t.io.host.remove(&OTHER);
        t.reconcile().check(OTHER, false);
        assert!(!t.io.links.contains_key(LINK));
        assert_eq!(t.io.mounts, HashSet::from([KEY]));
        assert!(t.state.published[&KEY].aliases().next().is_none());
    }

    #[test]
    fn input_classification_rechecks_sibling_hidraw() {
        let mut t = Test::default();
        let mut hid = snapshot();
        hid.device.subsystem = "hidraw".into();
        hid.device.minor = OTHER.1;
        hid.device.devname = "hidraw0".into();
        hid.device.links.clear();
        t.io.host.insert(OTHER, hid);
        t.reconcile().check(OTHER, true);
        assert!(t.state.published.is_empty());
        t.io.hidraw.push(OTHER);
        for fail_link in [None, Some(LINK)] {
            t.io.fail_link = fail_link;
            t.io.host.insert(KEY, snapshot());
            t.io.host.get_mut(&OTHER).unwrap().device.gamepad_hid = true;
            t.reconcile().check(KEY, true);
            assert!(t.state.published.contains_key(&OTHER));
            assert_eq!(t.state.published.contains_key(&KEY), fail_link.is_none());
            t.io.host.remove(&KEY);
            t.io.host.get_mut(&OTHER).unwrap().device.gamepad_hid = false;
            t.reconcile().check(KEY, true);
            assert!(t.state.published.is_empty());
        }
    }

    #[test]
    fn initial_link_failure_rolls_back_without_notification() {
        let mut t = Test::default();
        t.io.host.insert(KEY, snapshot());
        t.io.fail_link = Some(LINK);
        t.reconcile().check(KEY, true);
        assert_eq!(
            t.io.operations,
            [
                "attach 9",
                "link input/event6",
                "link input/by-id/pad",
                "unlink input/event6",
                "detach"
            ]
        );
        assert!(t.state.published.is_empty());
        assert!(t.io.links.is_empty());
        assert!(t.io.mounts.is_empty());
        assert!(t.io.notifications.is_empty());
    }

    #[test]
    fn scan_continues_after_a_device_cleanup_error() {
        let mut t = Test::default();
        t.io.host.insert(KEY, snapshot());
        t.reconcile().scan();

        let mut other = t.io.host.remove(&KEY).unwrap();
        other.device.minor = OTHER.1;
        other.device.devname = "input/event7".into();
        other.device.links.clear();
        t.io.host.insert(OTHER, other);
        t.io.fail_detach = true;
        t.reconcile().scan();

        assert!(t.state.published[&KEY].cleaning_up);
        assert_eq!(t.state.status()[0].node, ".psbox/c13-70");
        assert_eq!(t.io.mounts, HashSet::from([KEY, OTHER]));
        assert_eq!(t.io.operations.last().unwrap(), "add");
    }

    #[test]
    fn cleanup_failure_blocks_replacement_until_later_check() {
        for fail_detach in [true, false] {
            for exposed in [true, false] {
                let mut t = Test::default();
                t.io.host.insert(KEY, snapshot());
                t.io.fail_send = true;
                if !exposed {
                    t.io.fail_link = Some(LINK);
                    t.io.fail_detach = fail_detach;
                    t.io.fail_unlink = !fail_detach;
                }
                t.reconcile().check(KEY, true);
                t.io.fail_detach = fail_detach;
                t.io.fail_unlink = !fail_detach;
                t.io.host.get_mut(&KEY).unwrap().node.inode += 1;

                t.io.operations.clear();
                t.reconcile().scan();
                assert!(t.state.published[&KEY].cleaning_up);
                assert!(
                    t.io.operations
                        .iter()
                        .all(|op| op.starts_with("unlink ") || op == "detach")
                );
                assert_eq!(t.io.notifications.len(), usize::from(exposed));
                assert!(t.io.reports.iter().any(|msg| msg.contains(if fail_detach {
                    "detach failed"
                } else {
                    "unlink failed"
                })));

                t.io.fail_detach = false;
                t.io.fail_unlink = false;
                t.io.fail_link = None;
                t.io.operations.clear();
                t.reconcile().check(KEY, true);
                let attach = t.io.operations.iter().position(|op| op == "attach 10").unwrap();
                if exposed {
                    assert_eq!(t.io.operations[attach - 1], "remove");
                }
                assert_eq!(t.io.notifications.len(), if exposed { 3 } else { 1 });
                assert_eq!(t.io.operations.last().unwrap(), "add");
            }
        }
    }
}

#[cfg(test)]
mod native;
