//! The per-sandbox supervisor: owns the namespaces, the cgroup subtree, the
//! mount table, and every process in the sandbox.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use crate::bus;
use crate::devices::{self, Change, HostPaths};
use crate::forker::Forker;
use crate::gateway::{self, Gateway};
use crate::inject;
use crate::plan::{HostEnv, Identity, Mount, Plan, client_env_allowed};
use crate::portal::Policy;
use crate::protocol::{self, LiveMount, Process, PublishedDevice, Request, Response, Status};
use crate::root;
use crate::spawn::{self, Exit, SpawnJob};
use crate::spec::{DeviceClass, MountMode, Network, Spec, Tokens};
use crate::sys::cgroup::Cgroups;
use crate::sys::fd::{Seqpacket, poll_readable};
use crate::sys::mount::{MOUNT_ATTR_NODEV, MOUNT_ATTR_NOSUID, MOUNT_ATTR_RDONLY, PointKind};
use crate::sys::{cvt_int, landlock, mount, ns, path_cstr, process, seccomp};
use crate::systemd::{self, Systemd};
use crate::x11;

/// How long an unnamed sandbox or task waits for its first command once its
/// client has gone.
const UNSERVED_TIMEOUT: Duration = Duration::from_secs(10);

/// A detached mount tree ready to attach at `dst`.
struct Prepared {
    tree: OwnedFd,
    dst: PathBuf,
    kind: PointKind,
}

struct Published {
    names: Vec<String>,
    hidden: String,
    classes: Vec<String>,
}

#[derive(Default)]
struct State {
    clients: usize,
    mounts: Vec<LiveMount>,
    devices: BTreeMap<(u32, u32), Published>,
    /// Processes started for a command (host pid -> command name).
    spawned: HashMap<i32, String>,
    /// Counter for `$XDG_RUNTIME_DIR/doc/<n>` file dialog mounts.
    docs: u32,
    /// Whether any client has started a process yet.
    served: bool,
    /// Config changes that need a restart; the sandbox stops when idle.
    pending_restart: Vec<String>,
    /// Mounts a reload failed to apply. The running mounts no longer match
    /// any plan, so they stay a restart reason until the sandbox stops.
    unapplied: Vec<String>,
    /// Forwarded host sockets currently attached (sandbox paths).
    sockets: HashSet<PathBuf>,
}

struct Supervisor {
    forker: Forker,
    /// The current plan; replaced when the config changes live.
    plan: RwLock<Arc<Plan>>,
    id: Identity,
    host: HostEnv,
    cg: Cgroups,
    init_pid: i32,
    init_pidfd: OwnedFd,
    root_rw: OwnedFd,
    dev_rw: OwnedFd,
    seccomp: Vec<libc::sock_filter>,
    state: Mutex<State>,
    started: std::time::SystemTime,
    /// The X display number, when the sandbox has Xwayland.
    display: Option<u32>,
}

/// Runs the supervisor for `id` until the sandbox stops.
///
/// Must be called before any other thread exists: creating a user namespace
/// requires a single-threaded process.
pub fn run(id: &Identity) -> Result<()> {
    let listener = Seqpacket::from_systemd()?;
    let r = start(id, &listener);
    if let Err(e) = &r {
        refuse_waiting(&listener, &format!("{e:#}"));
    }
    r
}

/// Answers clients already waiting on the socket with the start failure, so
/// they report it instead of a reset connection.
fn refuse_waiting(listener: &Seqpacket, message: &str) {
    while poll_readable(&[listener.as_fd()], 0).is_ok_and(|r| r[0]) {
        let Ok(conn) = listener.accept() else { break };
        // Closing with the request unread would reset the connection.
        if poll_readable(&[conn.as_fd()], 1000).is_ok_and(|r| r[0]) {
            let _ = protocol::recv::<Request>(&conn);
        }
        let _ = protocol::send(
            &conn,
            &Response::Error {
                message: message.into(),
            },
            &[],
        );
    }
}

fn start(id: &Identity, listener: &Seqpacket) -> Result<()> {
    id.validate()?;
    let host = HostEnv::from_process()?;
    let spec = match id {
        Identity::Unnamed { .. } => {
            let path = host.settings_path(id);
            let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            let _ = fs::remove_file(&path);
            Spec::parse(&text).with_context(|| format!("{}", path.display()))?
        }
        Identity::Instance { .. } | Identity::Task { .. } => Spec::load(id.sandbox(), &host.config_dir)?,
    };
    let plan = Plan::build(id, &spec, &host)?;
    let name = plan.key.clone();
    for dir in plan.create_dirs(false)? {
        eprintln!("psbox[{name}]: created {}", dir.display());
    }
    plan.preflight(id, false)?;
    if plan.network_fallback {
        eprintln!("psbox[{name}]: pasta is not installed; using the host network instead of isolated networking");
    }
    let display = match plan.xwayland {
        true => Some(x11::free_display().context("choose an X display")?),
        false => None,
    };

    if let Some(dir) = &plan.host_home_dir {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let sock_dir = host.socket_dir();
    // Each init mounts its root here in its own mount namespace, so every
    // sandbox shares this directory. It is never removed: removing a mount
    // point detaches the mounts on it in every namespace.
    let new_root = sock_dir.join(".root");
    fs::create_dir_all(&new_root)?;

    let cg = Cgroups::setup()?;

    ns::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS).context("unshare")?;
    ns::write_self_maps(host.uid, host.gid).context("write id maps")?;
    ns::set_propagation(Path::new("/"), libc::MS_SLAVE).context("make / a slave")?;
    // Every process in the sandbox, and the supervisor itself, can only reach
    // abstract sockets and send signals within this domain.
    landlock::restrict_scopes(landlock::SCOPE_ABSTRACT_UNIX_SOCKET | landlock::SCOPE_SIGNAL)
        .context("Landlock scopes")?;

    let (sup_end, init_end) = Seqpacket::pair()?;
    let infra = fs::File::open(&cg.infra).with_context(|| format!("open {}", cg.infra.display()))?;
    // SAFETY: still single-threaded, so the child may run normal code.
    let init = match unsafe { process::clone3(libc::CLONE_NEWPID as u64, infra.as_raw_fd()) }.context("start init")? {
        Some(c) => c,
        None => {
            drop(sup_end);
            // SAFETY: the init never returns to code that uses the listener.
            unsafe { libc::close(listener.as_raw_fd()) };
            root::run_init(&plan, &new_root, init_end);
        }
    };
    drop(init_end);
    let (pid, init_pidfd) = (init.pid, init.pidfd);

    if !poll_readable(&[sup_end.as_fd()], 15_000)?[0] {
        bail!("sandbox init did not report within 15s");
    }
    let mut buf = [0u8; 4096];
    let (n, mut fds) = sup_end
        .recv(&mut buf)?
        .ok_or_else(|| anyhow!("sandbox init exited during setup"))?;
    let msg = String::from_utf8_lossy(&buf[..n]).into_owned();
    if msg != "ready" || fds.len() != 2 {
        bail!("sandbox init failed: {}", msg.strip_prefix("error: ").unwrap_or(&msg));
    }
    let dev_rw = fds.pop().expect("two fds");
    let root_rw = fds.pop().expect("two fds");

    let forker = Forker::start(init_pidfd.as_raw_fd()).context("start forker thread")?;
    let sup = Arc::new(Supervisor {
        forker,
        plan: RwLock::new(Arc::new(plan)),
        id: id.clone(),
        host,
        cg,
        init_pid: pid,
        init_pidfd,
        root_rw,
        dev_rw,
        seccomp: seccomp::program(),
        state: Mutex::new(State::default()),
        started: std::time::SystemTime::now(),
        display,
    });
    eprintln!("psbox[{name}]: sandbox ready (init pid {pid})");

    {
        let s = sup.clone();
        std::thread::spawn(move || s.watch_init());
    }
    {
        let s = sup.clone();
        std::thread::spawn(move || s.watch_idle());
    }
    {
        let s = sup.clone();
        std::thread::spawn(move || s.watch_devices());
    }
    if sup.plan().bus.is_some() {
        sup.start_bus().context("start session bus")?;
    }
    if let Some(display) = sup.display {
        sup.start_xwayland(display).context("prepare Xwayland")?;
    }
    if sup.plan().network == Network::Isolated {
        sup.start_pasta().context("start pasta")?;
    }
    sup.init_sockets();
    {
        let s = sup.clone();
        std::thread::spawn(move || s.watch_sockets());
    }
    if matches!(id, Identity::Instance { .. }) {
        let s = sup.clone();
        std::thread::spawn(move || s.watch_config());
    }
    loop {
        let conn = match listener.accept() {
            Ok(c) => c,
            Err(e) if e.raw_os_error() == Some(libc::EINTR) => continue,
            Err(e) => return Err(e).context("accept"),
        };
        let s = sup.clone();
        std::thread::spawn(move || {
            if let Err(e) = s.serve(&conn) {
                let _ = protocol::send(
                    &conn,
                    &Response::Error {
                        message: format!("{e:#}"),
                    },
                    &[],
                );
            }
        });
    }
}

impl Supervisor {
    /// The current plan.
    fn plan(&self) -> Arc<Plan> {
        self.plan.read().unwrap().clone()
    }

    /// The supervisor key, for logs.
    fn key(&self) -> String {
        self.plan().key.clone()
    }

    fn tokens(&self) -> Tokens<'_> {
        Tokens {
            home: &self.host.home,
            root: &self.host.root,
            name: self.id.sandbox(),
        }
    }

    fn serve(&self, conn: &Seqpacket) -> Result<()> {
        if conn.peer_uid()? != self.host.uid {
            bail!("permission denied");
        }
        let Some((req, fds)) = protocol::recv::<Request>(conn)? else {
            return Ok(());
        };
        match req {
            Request::Spawn(spawn) => {
                {
                    let mut st = self.state.lock().unwrap();
                    st.clients += 1;
                    st.served = true;
                }
                let r = self.serve_spawn(conn, spawn, fds);
                self.state.lock().unwrap().clients -= 1;
                r
            }
            Request::Mount { host, at, mode } => {
                self.mount(&host, &at, mode)?;
                protocol::send(conn, &Response::Done, &[])?;
                Ok(())
            }
            Request::Umount { at } => {
                self.umount(&at)?;
                protocol::send(conn, &Response::Done, &[])?;
                Ok(())
            }
            Request::Status => {
                protocol::send(conn, &Response::Status(self.status()), &[])?;
                Ok(())
            }
            Request::Stop => {
                protocol::send(conn, &Response::Done, &[])?;
                self.shutdown(0)
            }
            Request::Signal { .. } => bail!("no process on this connection"),
        }
    }

    fn serve_spawn(&self, conn: &Seqpacket, spawn: protocol::Spawn, fds: Vec<OwnedFd>) -> Result<()> {
        let protocol::Spawn {
            command,
            args,
            env: client_env,
            cwd: client_cwd,
            exec,
        } = spawn;
        let [stdin, stdout, stderr]: [OwnedFd; 3] = fds
            .try_into()
            .map_err(|_| anyhow!("spawn needs exactly three descriptors"))?;
        let plan = self.plan();
        let name = match command {
            Some(c) => c,
            None => plan
                .default_command()
                .ok_or_else(|| anyhow!("sandbox has several commands; choose one with --command"))?
                .to_string(),
        };
        let cmd = plan
            .commands
            .get(&name)
            .ok_or_else(|| anyhow!("unknown command {name:?}"))?;
        let mut argv: Vec<String> = if exec { Vec::new() } else { cmd.exec.clone() };
        argv.extend(args);
        if argv.is_empty() {
            bail!("nothing to run");
        }
        let argv: Vec<String> = argv
            .into_iter()
            .map(|a| match a.strip_prefix("~/") {
                Some(rest) => self.plan().home.join(rest).to_string_lossy().into_owned(),
                None => a,
            })
            .collect();

        let mut env: BTreeMap<String, String> = client_env.into_iter().filter(|(k, _)| client_env_allowed(k)).collect();
        env.extend(self.env());
        env.extend(cmd.env.clone());

        let plan = self.plan();
        let has_home = plan.mounts.iter().any(|m| m.dst() == plan.home);
        let cwd = if self.has_dir(&client_cwd) {
            client_cwd
        } else if has_home {
            plan.home.clone()
        } else {
            PathBuf::from("/")
        };
        let child = self.spawn(
            argv.clone(),
            env,
            cwd,
            [stdin, stdout, stderr],
            Vec::new(),
            &self.cg.workload,
        )?;
        eprintln!("psbox[{}]: started {:?} as pid {}", self.key(), argv, child.pid);
        self.state.lock().unwrap().spawned.insert(child.pid, name.clone());
        let restart_pending = self.state.lock().unwrap().pending_restart.clone();
        protocol::send(
            conn,
            &Response::Spawned {
                pid: child.pid,
                restart_pending,
                network_fallback: self.plan().network_fallback,
            },
            &[],
        )?;

        let mut attached = true;
        loop {
            let ready = if attached {
                poll_readable(&[child.pidfd.as_fd(), conn.as_fd()], -1)?
            } else {
                poll_readable(&[child.pidfd.as_fd()], -1)?
            };
            if ready[0] {
                break;
            }
            match protocol::recv::<Request>(conn) {
                Ok(Some((Request::Signal { signal }, _))) => {
                    let _ = ns::pidfd_send_signal(child.pidfd.as_fd(), signal);
                }
                Ok(Some(_)) => {
                    let _ = protocol::send(
                        conn,
                        &Response::Error {
                            message: "unexpected request".into(),
                        },
                        &[],
                    );
                }
                Ok(None) | Err(_) => attached = false,
            }
        }
        let exit = spawn::wait(child.pidfd.as_fd());
        self.state.lock().unwrap().spawned.remove(&child.pid);
        let (code, signal) = match exit? {
            Exit::Code(c) => (Some(c), None),
            Exit::Signal(s) => (None, Some(s)),
        };
        if attached {
            let _ = protocol::send(conn, &Response::Exited { code, signal }, &[]);
        }
        Ok(())
    }

    /// Starts a process in the sandbox, in the cgroup `cgroup`.
    fn spawn(
        &self,
        argv: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: PathBuf,
        stdio: [OwnedFd; 3],
        extra_fds: Vec<OwnedFd>,
        cgroup: &Path,
    ) -> Result<spawn::Child> {
        let job = SpawnJob {
            argv: argv.clone(),
            env,
            cwd,
            stdio,
            extra_fds,
            init_pidfd: self.init_pidfd.as_raw_fd(),
            join_net: self.plan().network != Network::Host,
            cgroup: fs::File::open(cgroup)?.into(),
            seccomp: self.seccomp.clone(),
        };
        self.forker
            .run(move || spawn::spawn(job))?
            .with_context(|| format!("start {}", argv[0]))
    }

    /// A descriptor for the sandbox root. The init is our unreaped child,
    /// so its pid cannot be reused while the supervisor runs.
    fn root(&self) -> io::Result<OwnedFd> {
        mount::open_dir(&PathBuf::from(format!("/proc/{}/root", self.init_pid)))
    }

    /// The plan's environment, with `DISPLAY` when the sandbox has Xwayland.
    fn env(&self) -> BTreeMap<String, String> {
        let mut env = self.plan().env.clone();
        if let Some(display) = self.display {
            env.insert("DISPLAY".into(), format!(":{display}"));
        }
        env
    }

    /// Starts the private session bus in the `infra` cgroup and the gateway
    /// to the host bus. The sandbox stops if the bus exits.
    fn start_bus(self: &Arc<Self>) -> Result<()> {
        let paths = self.plan().bus.clone().expect("bus configured");
        let root = self.root()?;
        bus::prepare(root.as_fd(), &paths, &self.plan().home)?;
        let argv = vec![
            "dbus-daemon".to_string(),
            format!("--config-file={}", paths.config.display()),
            "--nofork".into(),
            "--nopidfile".into(),
            "--nosyslog".into(),
        ];
        let stdio = [
            std::io::stdin().as_fd().try_clone_to_owned()?,
            std::io::stdout().as_fd().try_clone_to_owned()?,
            std::io::stderr().as_fd().try_clone_to_owned()?,
        ];
        let daemon = self.spawn(argv, self.env(), PathBuf::from("/"), stdio, Vec::new(), &self.cg.infra)?;
        // The socket's directory, opened without following symlinks, named
        // through its descriptor.
        let socket_dir = mount::openat2(
            root.as_fd(),
            bus::in_root(paths.socket.parent().expect("socket has a parent")),
            libc::O_PATH | libc::O_DIRECTORY,
            mount::RESOLVE_SAFE_IN_ROOT,
        )?;
        let socket = mount::fd_path(socket_dir.as_fd()).join(paths.socket.file_name().expect("socket has a name"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while crate::dbus::Conn::connect_path(&socket).is_err() {
            if Instant::now() > deadline || poll_readable(&[daemon.pidfd.as_fd()], 0)?[0] {
                bail!("dbus-daemon did not start");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let policy = Policy {
            camera: self.plan().node_classes.contains(&DeviceClass::Camera),
            ..Policy::default()
        };
        let host: Arc<dyn gateway::Host> = self.clone();
        Gateway::start(&socket, gateway::host_bus_address()?, policy, host)?;
        eprintln!(
            "psbox[{}]: session bus ready (dbus-daemon pid {})",
            self.key(),
            daemon.pid
        );
        let s = self.clone();
        std::thread::spawn(move || {
            let _ = spawn::wait(daemon.pidfd.as_fd());
            eprintln!("psbox[{}]: dbus-daemon exited", s.key());
            let _ = s.shutdown(1);
        });
        Ok(())
    }

    /// Listens on the sandbox's X socket and starts `xwayland-satellite`
    /// (in `infra`) whenever a client connects while it is not running.
    fn start_xwayland(self: &Arc<Self>, display: u32) -> Result<()> {
        let listener = x11::listen(self.root()?.as_fd(), display)?;
        let s = self.clone();
        std::thread::Builder::new().name("xwayland".into()).spawn(move || {
            loop {
                if poll_readable(&[listener.as_fd()], -1).is_err() {
                    return;
                }
                let started = (|| -> Result<spawn::Child> {
                    let stdio = [
                        std::io::stdin().as_fd().try_clone_to_owned()?,
                        std::io::stdout().as_fd().try_clone_to_owned()?,
                        std::io::stderr().as_fd().try_clone_to_owned()?,
                    ];
                    let fd = listener.try_clone()?;
                    s.spawn(
                        x11::satellite_argv(display),
                        s.plan().env.clone(),
                        PathBuf::from("/"),
                        stdio,
                        vec![fd],
                        &s.cg.infra,
                    )
                })();
                match started {
                    Ok(child) => {
                        eprintln!("psbox[{}]: started xwayland-satellite (pid {})", s.key(), child.pid);
                        let exit = spawn::wait(child.pidfd.as_fd());
                        eprintln!("psbox[{}]: xwayland-satellite exited: {exit:?}", s.key());
                    }
                    Err(e) => {
                        eprintln!("psbox[{}]: cannot start xwayland-satellite: {e:#}", s.key());
                        // Do not spin on a pending connection nobody will serve.
                        std::thread::sleep(Duration::from_secs(5));
                    }
                }
            }
        })?;
        Ok(())
    }

    /// Starts `pasta` attached to the sandbox's network namespace: outbound
    /// connections only (no port forwarding either way, no route to the
    /// host's loopback), and DNS relayed to the host's resolver. pasta stays
    /// on the host side, in the supervisor's cgroup, and gets just the
    /// capabilities it needs in the supervisor's user namespace (which owns
    /// the sandbox's network namespace and nothing on the host).
    fn start_pasta(self: &Arc<Self>) -> Result<()> {
        use std::os::unix::process::CommandExt;
        let netns = format!("/proc/{}/ns/net", self.init_pid);
        let mut cmd = std::process::Command::new("pasta");
        cmd.args([
            "--quiet",
            "--foreground",
            "--config-net",
            "--no-map-gw",
            "--dns-forward",
            crate::plan::PASTA_DNS,
        ])
        .args(["-t", "none", "-u", "none", "-T", "none", "-U", "none"])
        .args(["--netns-only", "--netns", &netns]);
        // SAFETY: the closure only issues raw syscalls.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
                raise_ambient(&[CAP_SYS_ADMIN, CAP_NET_ADMIN, CAP_NET_RAW, CAP_NET_BIND_SERVICE])
            });
        }
        let mut child = cmd.spawn().context("run pasta")?;
        eprintln!("psbox[{}]: pasta started (pid {})", self.key(), child.id());
        let s = self.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            eprintln!("psbox[{}]: pasta exited: {status:?}", s.key());
        });
        Ok(())
    }

    /// The host pid of a process given its pid inside the sandbox.
    fn host_pid(&self, sandbox_pid: u32) -> Option<i32> {
        let ns = fs::read_link(format!("/proc/{}/ns/pid", self.init_pid)).ok()?;
        for entry in fs::read_dir("/proc").ok()?.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else {
                continue;
            };
            if fs::read_link(format!("/proc/{pid}/ns/pid")).ok().as_ref() != Some(&ns) {
                continue;
            }
            let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
            let inner = status
                .lines()
                .find_map(|l| l.strip_prefix("NSpid:"))
                .and_then(|v| v.split_whitespace().last())
                .and_then(|v| v.parse::<u32>().ok());
            if inner == Some(sandbox_pid) {
                return Some(pid);
            }
        }
        None
    }

    /// Attaches one plan mount (a bind or tmpfs) in the running sandbox.
    fn attach_plan_mount(&self, m: &Mount) -> Result<()> {
        match self.prepare_plan_mount(&self.plan(), m)? {
            Some(prepared) => self.place(prepared),
            None => Ok(()),
        }
    }

    /// The detached mount tree for a mount of `plan`, or `None` for an
    /// optional bind whose source does not exist.
    fn prepare_plan_mount(&self, plan: &Plan, m: &Mount) -> Result<Option<Prepared>> {
        let prepared = match m {
            Mount::Bind { src, dst, ro, optional } => {
                let source = match mount::open_source(src, plan.source_guard(src).as_deref()) {
                    Ok(fd) => fd,
                    Err(e) if e.kind() == io::ErrorKind::NotFound && *optional => return Ok(None),
                    Err(e) => return Err(e).with_context(|| format!("source {}", src.display())),
                };
                let meta = fs::File::from(source.try_clone()?).metadata()?;
                let tree = mount::clone_tree_fd(source.as_fd(), true)?;
                let mut attrs = MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV;
                if *ro {
                    attrs |= MOUNT_ATTR_RDONLY;
                }
                mount::set_attr_fd(tree.as_fd(), true, attrs, 0)?;
                let kind = if meta.is_dir() { PointKind::Dir } else { PointKind::File };
                Prepared {
                    tree,
                    dst: dst.clone(),
                    kind,
                }
            }
            Mount::Tmpfs { dst, mode } => Prepared {
                tree: mount::tmpfs(*mode, MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV)?,
                dst: dst.clone(),
                kind: PointKind::Dir,
            },
            other => bail!("cannot attach {} live", other.dst().display()),
        };
        Ok(Some(prepared))
    }

    fn place(&self, p: Prepared) -> Result<()> {
        self.ensure_point(&p.dst, p.kind)?;
        inject::attach(self.init_pidfd.as_fd(), p.tree.as_fd(), &p.dst)
            .with_context(|| format!("attach {}", p.dst.display()))
    }

    /// Reloads the config and applies what can change live. Changes that
    /// need a restart are recorded instead, and the sandbox stops when idle.
    /// New mounts are prepared before anything is detached, so a missing
    /// source leaves the sandbox as it was; a mount that still fails to
    /// apply marks the sandbox for a restart.
    fn reload(&self) -> Result<()> {
        let spec = Spec::load(self.id.sandbox(), &self.host.config_dir)?;
        let new = Plan::build(&self.id, &spec, &self.host)?;
        for dir in new.create_dirs(false)? {
            eprintln!("psbox[{}]: created {}", self.key(), dir.display());
        }
        new.preflight(&self.id, false)?;
        let old = self.plan();
        let change = crate::plan::diff(&old, &new);
        if !change.restart.is_empty() {
            eprintln!(
                "psbox[{}]: config change needs a restart: {}",
                self.key(),
                change.restart.join(", ")
            );
            let mut st = self.state.lock().unwrap();
            st.pending_restart = change.restart.into_iter().chain(st.unapplied.clone()).collect();
            return Ok(());
        }
        let prepared = change
            .attach
            .iter()
            .map(|m| Ok((m, self.prepare_plan_mount(&new, m)?)))
            .collect::<Result<Vec<_>>>()?;

        let mut failed = Vec::new();
        for m in &change.detach {
            if let Err(e) = inject::detach(self.init_pidfd.as_fd(), m.dst()) {
                // An optional bind whose source was missing was never attached.
                let never_attached = matches!(m, Mount::Bind { optional: true, .. })
                    && matches!(e.raw_os_error(), Some(libc::ENOENT | libc::EINVAL));
                if !never_attached {
                    eprintln!("psbox[{}]: detach {}: {e}", self.key(), m.dst().display());
                    failed.push(format!("mount at {}", m.dst().display()));
                    continue;
                }
            }
            self.state.lock().unwrap().sockets.remove(m.dst());
        }
        for (m, p) in prepared {
            let Some(p) = p else { continue };
            match self.place(p) {
                Ok(()) => {
                    if self.forwarded_socket(m).is_some() {
                        self.state.lock().unwrap().sockets.insert(m.dst().to_path_buf());
                    }
                }
                Err(e) => {
                    eprintln!("psbox[{}]: {e:#}", self.key());
                    failed.push(format!("mount at {}", m.dst().display()));
                }
            }
        }
        let classes_changed = old.node_classes != new.node_classes;
        *self.plan.write().unwrap() = Arc::new(new);

        // Live mounts (`psbox mount`, the file chooser) beneath a detached
        // mount went with it, and those beneath a newly attached one are
        // covered by it; mount them again on top of what is there now, with
        // the new plan in place.
        let lost: Vec<LiveMount> = {
            let mut st = self.state.lock().unwrap();
            let (lost, kept) = std::mem::take(&mut st.mounts).into_iter().partition(|lm| {
                change
                    .detach
                    .iter()
                    .chain(&change.attach)
                    .any(|m| Path::new(&lm.at).starts_with(m.dst()))
            });
            st.mounts = kept;
            lost
        };
        for lm in lost {
            if let Err(e) = self.mount(&lm.host, &lm.at, lm.mode) {
                eprintln!("psbox[{}]: re-attach {}: {e:#}", self.key(), lm.at);
            }
        }
        if !failed.is_empty() {
            eprintln!(
                "psbox[{}]: config only partly applied; restart needed for: {}",
                self.key(),
                failed.join(", ")
            );
        }
        {
            let mut st = self.state.lock().unwrap();
            st.unapplied.extend(failed);
            st.pending_restart = st.unapplied.clone();
        }
        if classes_changed {
            self.reconcile_all(&HostPaths::system());
        }
        if change.is_empty() {
            eprintln!("psbox[{}]: config reloaded", self.key());
        } else {
            eprintln!(
                "psbox[{}]: config reloaded ({} attached, {} detached)",
                self.key(),
                change.attach.len(),
                change.detach.len()
            );
        }
        Ok(())
    }

    /// Watches the sandbox config and the profiles directory.
    fn watch_config(&self) {
        // Only this sandbox's file in `sandboxes/`, and any profile.
        let file = format!("{}.toml", self.id.sandbox());
        let watch = || -> io::Result<(crate::sys::inotify::Inotify, i32, Option<i32>)> {
            let ino = crate::sys::inotify::Inotify::new()?;
            let mask =
                libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM;
            let dir = self.host.config_dir.join("sandboxes");
            let _ = fs::create_dir_all(&dir);
            let sandboxes = ino.add(&dir, mask)?;
            let profiles = self.host.config_dir.join("profiles");
            let profiles = match profiles.is_dir() {
                true => Some(ino.add(&profiles, mask)?),
                false => None,
            };
            Ok((ino, sandboxes, profiles))
        };
        let (ino, sandboxes, profiles) = match watch() {
            Ok(w) => w,
            Err(e) => {
                eprintln!("psbox[{}]: cannot watch the config: {e}", self.key());
                return;
            }
        };
        loop {
            let Ok(events) = ino.read() else { return };
            let relevant = events.iter().any(|e| {
                (e.wd == sandboxes && e.name == file) || (Some(e.wd) == profiles && e.name.ends_with(".toml"))
            });
            if !relevant {
                continue;
            }
            // Editors write in several steps; let them finish.
            std::thread::sleep(Duration::from_millis(200));
            if let Err(e) = self.reload() {
                eprintln!("psbox[{}]: config not applied: {e:#}", self.key());
            }
        }
    }

    /// The host source of a plan mount that forwards a host runtime socket.
    fn forwarded_socket<'a>(&self, m: &'a Mount) -> Option<&'a Path> {
        match m {
            Mount::Bind { src, .. } if src.starts_with(&self.host.runtime_dir) => Some(src),
            _ => None,
        }
    }

    /// Records which forwarded sockets were present when the root was built.
    fn init_sockets(&self) {
        let plan = self.plan();
        let mut st = self.state.lock().unwrap();
        for m in &plan.mounts {
            if self.forwarded_socket(m).is_some_and(Path::exists) {
                st.sockets.insert(m.dst().to_path_buf());
            }
        }
    }

    /// Re-attaches forwarded host sockets (Wayland, PipeWire, Pulse) when
    /// their server creates them again, so a restarted compositor or audio
    /// server reaches running apps without a sandbox restart.
    fn watch_sockets(&self) {
        let Ok(ino) = crate::sys::inotify::Inotify::new() else {
            return;
        };
        let mut dirs: HashMap<i32, PathBuf> = HashMap::new();
        for m in &self.plan().mounts {
            if let Some(dir) = self.forwarded_socket(m).and_then(Path::parent)
                && !dirs.values().any(|d| d == dir)
                && let Ok(wd) = ino.add(dir, libc::IN_CREATE | libc::IN_MOVED_TO)
            {
                dirs.insert(wd, dir.to_path_buf());
            }
        }
        if dirs.is_empty() {
            return;
        }
        loop {
            let Ok(events) = ino.read() else { return };
            for e in events {
                let Some(dir) = dirs.get(&e.wd) else { continue };
                let host_path = dir.join(&e.name);
                let plan = self.plan();
                let Some(m) = plan
                    .mounts
                    .iter()
                    .find(|m| self.forwarded_socket(m) == Some(host_path.as_path()))
                else {
                    continue;
                };
                if let Err(err) = self.reattach_socket(m) {
                    eprintln!("psbox[{}]: re-attach {}: {err:#}", self.key(), m.dst().display());
                }
            }
        }
    }

    fn reattach_socket(&self, m: &Mount) -> Result<()> {
        let Mount::Bind { dst, .. } = m else { return Ok(()) };
        let attached = self.state.lock().unwrap().sockets.contains(dst);
        if attached {
            let Some(p) = self.prepare_plan_mount(&self.plan(), m)? else {
                return Ok(());
            };
            inject::replace(self.init_pidfd.as_fd(), p.tree.as_fd(), dst)?;
        } else {
            self.attach_plan_mount(m)?;
            self.state.lock().unwrap().sockets.insert(dst.clone());
        }
        eprintln!("psbox[{}]: re-attached {}", self.key(), dst.display());
        Ok(())
    }

    /// Where a host path already appears in the sandbox through a writable
    /// bind, if anywhere.
    fn visible_at(&self, host_path: &Path) -> Option<PathBuf> {
        let live = self.state.lock().unwrap().mounts.clone();
        let plan = self.plan();
        let mounts: Vec<(Option<PathBuf>, PathBuf)> = plan
            .mounts
            .iter()
            .map(|m| match m {
                Mount::Bind {
                    src, dst, ro: false, ..
                } => (Some(src.clone()), dst.clone()),
                other => (None, other.dst().to_path_buf()),
            })
            .chain(live.iter().map(|m| {
                let src = (m.mode == MountMode::Rw).then(|| PathBuf::from(&m.host));
                (src, PathBuf::from(&m.at))
            }))
            .collect();
        visible_through(&mounts, host_path)
    }

    /// Creates the mount point `at` inside the sandbox. Paths on the
    /// read-only root or `/dev` are created through the writable views;
    /// everything else through the sandbox's own view of its root, so a
    /// path beneath a read-only mount fails as it does at start.
    fn ensure_point(&self, at: &Path, kind: PointKind) -> Result<()> {
        if let Ok(rel) = at.strip_prefix("/dev") {
            mount::make_point(self.dev_rw.as_fd(), rel, kind)?;
            return Ok(());
        }
        let rel = at.strip_prefix("/").unwrap_or(at);
        let root = mount::open_dir(&PathBuf::from(format!("/proc/{}/root", self.init_pid)))?;
        match mount::make_point(root.as_fd(), rel, kind) {
            Ok(_) => Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::EROFS) && self.on_root_tmpfs(root.as_fd(), rel) => {
                mount::make_point(self.root_rw.as_fd(), rel, kind)?;
                Ok(())
            }
            Err(e) => Err(e).with_context(|| format!("mount point {}", at.display())),
        }
    }

    /// Whether `path` is a directory inside the sandbox.
    fn has_dir(&self, path: &Path) -> bool {
        let Ok(root) = mount::open_dir(&PathBuf::from(format!("/proc/{}/root", self.init_pid))) else {
            return false;
        };
        let rel = path.strip_prefix("/").unwrap_or(path);
        path.is_absolute()
            && mount::openat2(
                root.as_fd(),
                rel,
                libc::O_PATH | libc::O_DIRECTORY,
                mount::RESOLVE_IN_ROOT,
            )
            .is_ok()
    }

    /// Whether the deepest existing ancestor of `rel` is on the root tmpfs
    /// itself rather than on a mount above it.
    fn on_root_tmpfs(&self, root: BorrowedFd<'_>, rel: &Path) -> bool {
        let dev = |fd: BorrowedFd<'_>| {
            let file = fs::File::from(fd.try_clone_to_owned().ok()?);
            file.metadata().ok().map(|m| m.dev())
        };
        let mut dir = rel.parent();
        while let Some(d) = dir {
            let path = if d.as_os_str().is_empty() { Path::new(".") } else { d };
            if let Ok(fd) = mount::openat2(root, path, libc::O_PATH | libc::O_DIRECTORY, mount::RESOLVE_IN_ROOT) {
                return dev(fd.as_fd()).is_some_and(|d| Some(d) == dev(self.root_rw.as_fd()));
            }
            dir = d.parent();
        }
        false
    }

    fn mount(&self, host_path: &str, at: &str, mode: MountMode) -> Result<()> {
        if !matches!(mode, MountMode::Ro | MountMode::Rw) {
            bail!("live mounts support ro and rw");
        }
        let tokens = self.tokens();
        let src = tokens.expand(host_path)?;
        let dst = tokens.expand(at)?;
        if dst == Path::new("/") || dst.starts_with("/proc") || dst.starts_with("/dev") {
            bail!("cannot mount at {}", dst.display());
        }
        let source = match mount::open_source(&src, self.plan().source_guard(&src).as_deref()) {
            Ok(fd) => fd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => bail!("it doesn't exist"),
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                bail!("it goes through a symlink in a directory the sandbox can write to")
            }
            Err(e) => return Err(e).with_context(|| format!("source {}", src.display())),
        };
        let meta = fs::File::from(source.try_clone()?).metadata()?;
        let tree = mount::clone_tree_fd(source.as_fd(), true)?;
        let mut attrs = MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV;
        if mode == MountMode::Ro {
            attrs |= MOUNT_ATTR_RDONLY;
        }
        mount::set_attr_fd(tree.as_fd(), true, attrs, 0)?;
        self.ensure_point(&dst, if meta.is_dir() { PointKind::Dir } else { PointKind::File })?;
        inject::attach(self.init_pidfd.as_fd(), tree.as_fd(), &dst).context("attach")?;
        eprintln!(
            "psbox[{}]: mounted {} at {} ({mode:?})",
            self.key(),
            src.display(),
            dst.display()
        );
        self.state.lock().unwrap().mounts.push(LiveMount {
            host: src.to_string_lossy().into_owned(),
            at: dst.to_string_lossy().into_owned(),
            mode,
        });
        Ok(())
    }

    fn umount(&self, at: &str) -> Result<()> {
        let dst = self.tokens().expand(at)?.to_string_lossy().into_owned();
        let mut state = self.state.lock().unwrap();
        let idx = state
            .mounts
            .iter()
            .position(|m| m.at == dst)
            .ok_or_else(|| anyhow!("{dst} is not a live mount"))?;
        inject::detach(self.init_pidfd.as_fd(), Path::new(&dst)).context("detach")?;
        state.mounts.remove(idx);
        eprintln!("psbox[{}]: unmounted {dst}", self.key());
        Ok(())
    }

    fn status(&self) -> Status {
        let state = self.state.lock().unwrap();
        let procs = Cgroups::procs(&self.cg.workload);
        let mut processes: Vec<Process> = state
            .spawned
            .iter()
            .filter(|(pid, _)| procs.contains(pid))
            .map(|(&pid, command)| Process {
                pid,
                command: command.clone(),
                argv: fs::read(format!("/proc/{pid}/cmdline"))
                    .map(|b| {
                        b.split(|&c| c == 0)
                            .filter(|a| !a.is_empty())
                            .map(|a| String::from_utf8_lossy(a).into_owned())
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect();
        processes.sort_by_key(|p| p.pid);
        Status {
            name: self.key(),
            started: self
                .started
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            restart_pending: state.pending_restart.clone(),
            init_pid: self.init_pid,
            network: self.plan().network,
            network_fallback: self.plan().network_fallback,
            display: self.display,
            processes,
            process_count: procs.len(),
            clients: state.clients,
            mounts: state.mounts.clone(),
            devices: state
                .devices
                .values()
                .map(|p| PublishedDevice {
                    node: p.names[0].clone(),
                    links: p.names[1..].to_vec(),
                    classes: p.classes.clone(),
                })
                .collect(),
        }
    }

    fn watch_init(&self) {
        let _ = poll_readable(&[self.init_pidfd.as_fd()], -1);
        eprintln!("psbox[{}]: sandbox init exited", self.key());
        let _ = self.shutdown(1);
    }

    fn watch_idle(&self) {
        let started = Instant::now();
        let mut idle_since: Option<Instant> = None;
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let (clients, served) = {
                let st = self.state.lock().unwrap();
                (st.clients, st.served)
            };
            // A zero timeout (tasks, unnamed sandboxes) means "stop once the
            // work is done", so it only applies after the first process was
            // started, or when the client that started the sandbox left
            // without asking for one.
            if self.plan().idle_timeout.is_zero() && !served {
                if clients == 0 && started.elapsed() >= UNSERVED_TIMEOUT {
                    eprintln!("psbox[{}]: no command requested, stopping", self.key());
                    let _ = self.shutdown(0);
                }
                continue;
            }
            let busy = clients > 0 || Cgroups::populated(&self.cg.workload);
            if busy {
                idle_since = None;
                continue;
            }
            let pending = self.state.lock().unwrap().pending_restart.clone();
            if !pending.is_empty() {
                eprintln!(
                    "psbox[{}]: idle with a config change that needs a restart ({}), stopping",
                    self.key(),
                    pending.join(", ")
                );
                let _ = self.shutdown(0);
            }
            let since = *idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= self.plan().idle_timeout {
                eprintln!(
                    "psbox[{}]: idle for {:?}, stopping",
                    self.key(),
                    self.plan().idle_timeout
                );
                let _ = self.shutdown(0);
            }
        }
    }

    /// Stops everything and exits the process.
    fn shutdown(&self, code: i32) -> Result<()> {
        let _ = Cgroups::kill(&self.cg.workload);
        let _ = ns::pidfd_send_signal(self.init_pidfd.as_fd(), libc::SIGKILL);
        let _ = Cgroups::kill(&self.cg.infra);
        // An unnamed sandbox is never started again, so its socket unit goes
        // too, even when the client that started it is gone.
        if matches!(self.id, Identity::Unnamed { .. }) {
            let _ = Systemd::connect().and_then(|s| s.stop(&systemd::socket_unit(&self.key())));
        }
        std::process::exit(code)
    }

    fn watch_devices(&self) {
        let host = HostPaths::system();
        let watcher = match devices::Watcher::new(&host) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("psbox[{}]: cannot watch {}: {e}", self.key(), host.udev_data.display());
                return;
            }
        };
        self.reconcile_all(&host);
        loop {
            let changes = match watcher.next() {
                Ok(c) => c,
                Err(e) if e.raw_os_error() == Some(libc::EINTR) => continue,
                Err(e) => {
                    eprintln!("psbox[{}]: device watch failed: {e}", self.key());
                    return;
                }
            };
            for change in changes {
                match change {
                    Change::Updated(maj, min) | Change::Removed(maj, min) => self.reconcile(&host, maj, min),
                    Change::Rescan => self.reconcile_all(&host),
                }
            }
        }
    }

    fn reconcile_all(&self, host: &HostPaths) {
        let mut keys = devices::scan(host);
        keys.extend(self.state.lock().unwrap().devices.keys().copied());
        keys.sort();
        keys.dedup();
        for (maj, min) in keys {
            self.reconcile(host, maj, min);
        }
    }

    /// Publishes or unpublishes one device so `/dev` matches the host and
    /// the configured classes.
    fn reconcile(&self, host: &HostPaths, maj: u32, min: u32) {
        let dev = devices::read_device(host, maj, min);
        let classes = dev
            .as_ref()
            .map(|d| devices::classify(d, &self.plan().node_classes))
            .unwrap_or_default();
        let published = self.state.lock().unwrap().devices.contains_key(&(maj, min));
        let r = match (dev, classes.is_empty(), published) {
            (Some(d), false, false) => self.publish(&d, &classes),
            (_, true, true) | (None, _, true) => self.unpublish(maj, min),
            _ => Ok(()),
        };
        if let Err(e) = r {
            eprintln!("psbox[{}]: device {maj}:{min}: {e:#}", self.key());
        }
    }

    fn publish(&self, dev: &devices::Device, classes: &[crate::spec::DeviceClass]) -> Result<()> {
        let node = devices::host_node(dev);
        let tree = mount::clone_tree(&node, false).with_context(|| format!("clone {}", node.display()))?;
        // The node must be the device udev described, not a replacement.
        // SAFETY: fstat on a valid fd into a local buffer.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        cvt_int(unsafe { libc::fstat(tree.as_raw_fd(), &mut st) })?;
        if st.st_mode & libc::S_IFMT != libc::S_IFCHR || st.st_rdev != libc::makedev(dev.major, dev.minor) {
            bail!("{} is not character device {}:{}", node.display(), dev.major, dev.minor);
        }
        mount::set_attr_fd(tree.as_fd(), false, MOUNT_ATTR_NOSUID, 0)?;

        let hidden = devices::hidden_name(dev.major, dev.minor);
        let hidden_rel = Path::new(".psbox").join(&hidden);
        mount::make_point(self.dev_rw.as_fd(), &hidden_rel, PointKind::File)?;
        inject::attach(
            self.init_pidfd.as_fd(),
            tree.as_fd(),
            &Path::new("/dev").join(&hidden_rel),
        )
        .context("attach node")?;

        let mut names = vec![dev.devname.clone()];
        names.extend(dev.links.iter().cloned());
        for name in &names {
            publish_link(self.dev_rw.as_fd(), name, &devices::link_target(name, &hidden))
                .with_context(|| format!("link /dev/{name}"))?;
        }
        let class_names: Vec<String> = classes.iter().map(|c| c.to_string()).collect();
        eprintln!(
            "psbox[{}]: published /dev/{} ({})",
            self.key(),
            dev.devname,
            class_names.join(", ")
        );
        self.state.lock().unwrap().devices.insert(
            (dev.major, dev.minor),
            Published {
                names,
                hidden,
                classes: class_names,
            },
        );
        Ok(())
    }

    fn unpublish(&self, maj: u32, min: u32) -> Result<()> {
        let Some(p) = self.state.lock().unwrap().devices.remove(&(maj, min)) else {
            return Ok(());
        };
        for name in &p.names {
            let _ = unlink_if_ours(self.dev_rw.as_fd(), name, &devices::link_target(name, &p.hidden));
        }
        let hidden_rel = Path::new(".psbox").join(&p.hidden);
        inject::detach(self.init_pidfd.as_fd(), &Path::new("/dev").join(&hidden_rel)).context("detach node")?;
        let c = path_cstr(&hidden_rel)?;
        // SAFETY: valid dirfd and path.
        unsafe { libc::unlinkat(self.dev_rw.as_raw_fd(), c.as_ptr(), 0) };
        eprintln!("psbox[{}]: removed /dev/{}", self.key(), p.names[0]);
        Ok(())
    }
}

const CAP_NET_BIND_SERVICE: u32 = 10;
const CAP_NET_ADMIN: u32 = 12;
const CAP_NET_RAW: u32 = 13;
const CAP_SYS_ADMIN: u32 = 21;

/// Keeps `caps` across `execve` by making them inheritable and ambient.
/// Runs between fork and exec, so raw syscalls only.
fn raise_ambient(caps: &[u32]) -> io::Result<()> {
    #[repr(C)]
    struct Header {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    let mut header = Header {
        version: 0x2008_0522,
        pid: 0,
    };
    let mut data = [Data::default(); 2];
    // SAFETY: capget/capset with a v3 header and two data words; prctl.
    unsafe {
        if libc::syscall(libc::SYS_capget, &mut header as *mut Header, data.as_mut_ptr()) < 0 {
            return Err(io::Error::last_os_error());
        }
        for &cap in caps {
            data[(cap / 32) as usize].inheritable |= 1 << (cap % 32);
        }
        if libc::syscall(libc::SYS_capset, &header as *const Header, data.as_ptr()) < 0 {
            return Err(io::Error::last_os_error());
        }
        for &cap in caps {
            if libc::prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_RAISE,
                cap as libc::c_ulong,
                0,
                0,
            ) < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// Maps a host path through the writable binds among `mounts`, given as
/// `(writable source, sandbox destination)` in the order they were mounted.
/// A bind only counts when it is what the sandbox sees at the mapped path:
/// it is the last mount containing that path, and the only one at the
/// deepest place containing it. The order is only approximate (a plan
/// mount attached on reload or a re-attached socket sits above older live
/// mounts but is listed before them), so requiring all of this errs towards
/// "not visible", which costs an extra mount rather than handing out a path
/// that shows something else. Of the binds that count, the most specific
/// wins.
fn visible_through(mounts: &[(Option<PathBuf>, PathBuf)], host_path: &Path) -> Option<PathBuf> {
    let top = |path: &Path| {
        let containing: Vec<(usize, usize)> = mounts
            .iter()
            .enumerate()
            .filter(|(_, (_, dst))| path.starts_with(dst))
            .map(|(i, (_, dst))| (i, dst.components().count()))
            .collect();
        let &(last, depth) = containing.last()?;
        let at_deepest = containing.iter().filter(|(_, d)| *d == depth).count();
        (containing.iter().all(|(_, d)| *d <= depth) && at_deepest == 1).then_some(last)
    };
    mounts
        .iter()
        .enumerate()
        .filter_map(|(i, (src, dst))| {
            let src = src.as_ref()?;
            let path = dst.join(host_path.strip_prefix(src).ok()?);
            (top(&path) == Some(i)).then_some((src.components().count(), path))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, path)| path)
}

/// Atomically creates or replaces `/dev/<name>` as a symlink to `target`.
/// The device is already attached at the target, so the first event an app
/// sees for `name` refers to a working node.
fn publish_link(dev: BorrowedFd<'_>, name: &str, target: &str) -> io::Result<()> {
    let path = Path::new(name);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        mount::make_point(dev, parent, PointKind::Dir)?;
    }
    let tmp = path_cstr(&Path::new(".psbox").join(format!(".link-{}", name.replace('/', "_"))))?;
    let target = path_cstr(Path::new(target))?;
    let dst = path_cstr(path)?;
    // SAFETY: valid dirfd and C strings.
    unsafe {
        libc::unlinkat(dev.as_raw_fd(), tmp.as_ptr(), 0);
        cvt_int(libc::symlinkat(target.as_ptr(), dev.as_raw_fd(), tmp.as_ptr()))?;
        cvt_int(libc::renameat(
            dev.as_raw_fd(),
            tmp.as_ptr(),
            dev.as_raw_fd(),
            dst.as_ptr(),
        ))?;
    }
    Ok(())
}

fn unlink_if_ours(dev: BorrowedFd<'_>, name: &str, target: &str) -> io::Result<()> {
    let c = path_cstr(Path::new(name))?;
    let mut buf = [0u8; 4096];
    // SAFETY: reading the link into a local buffer.
    let n = unsafe { libc::readlinkat(dev.as_raw_fd(), c.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    if &buf[..n as usize] == target.as_bytes() {
        // SAFETY: valid dirfd and path.
        cvt_int(unsafe { libc::unlinkat(dev.as_raw_fd(), c.as_ptr(), 0) })?;
    }
    Ok(())
}

impl gateway::Host for Supervisor {
    fn app_id_for_pid(&self, sandbox_pid: u32) -> Option<String> {
        let mut pid = self.host_pid(sandbox_pid)?;
        for _ in 0..64 {
            let command = self.state.lock().unwrap().spawned.get(&pid).cloned();
            if let Some(name) = command {
                let own = self.plan().commands.get(&name).and_then(|c| c.app_id.clone());
                return own.or_else(|| self.plan().app_id.clone());
            }
            let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
            let ppid = status
                .lines()
                .find_map(|l| l.strip_prefix("PPid:"))?
                .trim()
                .parse::<i32>()
                .ok()?;
            if ppid <= 1 || ppid == self.init_pid {
                break;
            }
            pid = ppid;
        }
        self.plan().app_id.clone()
    }

    fn expose(&self, host_path: &Path, create: bool) -> Result<PathBuf> {
        if create && !host_path.exists() {
            fs::File::options().write(true).create_new(true).open(host_path)?;
        }
        if let Some(p) = self.visible_at(host_path) {
            return Ok(p);
        }
        let n = {
            let mut st = self.state.lock().unwrap();
            st.docs += 1;
            st.docs
        };
        let file = host_path.file_name().context("path has no file name")?;
        let at = self.plan().runtime_dir.join("doc").join(n.to_string()).join(file);
        self.mount(&host_path.to_string_lossy(), &at.to_string_lossy(), MountMode::Rw)?;
        Ok(at)
    }

    fn log(&self, message: &str) {
        eprintln!("psbox[{}]: {message}", self.key());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_host_paths_through_the_bind_the_sandbox_sees() {
        let rw = |src: &str, dst: &str| (Some(PathBuf::from(src)), PathBuf::from(dst));
        let other = |dst: &str| (None, PathBuf::from(dst));
        let mounts = vec![
            rw("/home/u/Sandbox/app", "/home/u"),
            rw("/home/u/Sandbox/app/Downloads/x", "/mnt/x"),
            rw("/home/u/Downloads", "/home/u/Downloads"),
            other("/home/u/Downloads/private"),
            rw("/home/u/Music", "/home/u/Music"),
            other("/home/u/Music"),
            rw("/home/u/Pictures", "/home/u/Pictures"),
            other("/home/u/Pictures/x"),
            rw("/srv/a", "/srv/a/b"),
            other("/srv"),
            other("/opt/z"),
            rw("/h", "/opt"),
        ];
        let r = |p: &str| visible_through(&mounts, Path::new(p));

        assert_eq!(
            r("/home/u/Sandbox/app/notes.txt"),
            Some(PathBuf::from("/home/u/notes.txt"))
        );
        assert_eq!(r("/home/u/Sandbox/app/Downloads/x/y"), Some(PathBuf::from("/mnt/x/y")));
        assert_eq!(
            r("/home/u/Downloads/a.pdf"),
            Some(PathBuf::from("/home/u/Downloads/a.pdf"))
        );
        assert_eq!(
            r("/home/u/Downloads/privateer/a.pdf"),
            Some(PathBuf::from("/home/u/Downloads/privateer/a.pdf"))
        );
        // Covered by a mount made later beneath, at, or above the bind.
        assert_eq!(r("/home/u/Downloads/private/a.pdf"), None);
        assert_eq!(r("/home/u/Music/a.ogg"), None);
        assert_eq!(r("/home/u/Pictures/x/a.png"), None);
        assert_eq!(
            r("/home/u/Pictures/a.png"),
            Some(PathBuf::from("/home/u/Pictures/a.png"))
        );
        assert_eq!(r("/srv/a/c"), None);
        // A deeper mount listed earlier may have been attached later, and
        // of two mounts at the same place either may be on top.
        assert_eq!(r("/h/z/f"), None);
        assert_eq!(r("/h/f"), Some(PathBuf::from("/opt/f")));
        let same_place = vec![rw("/a", "/x"), rw("/b", "/x")];
        assert_eq!(visible_through(&same_place, Path::new("/b/f")), None);
        assert_eq!(r("/home/u/Sandbox/application/z"), None);
        assert_eq!(r("/etc/passwd"), None);
    }
}
