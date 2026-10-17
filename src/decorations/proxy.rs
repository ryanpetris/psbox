//! The pass-through between the bridge and the compositor. Every object the
//! bridge creates is created on the compositor's connection too, and
//! requests and events are forwarded with object ids translated between the
//! two. The proxy keeps what the frames need along the way (surface sizes,
//! the X11 window behind each toplevel), and adjusts what a title bar
//! changes: configured sizes, window geometry, size limits, and the
//! positions of popups. The decoration manager it offers is its own: its
//! objects never reach the compositor.

use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use anyhow::{Context, Result, anyhow, bail};
use smithay_client_toolkit::reexports::csd_frame::FrameAction;
use wayland_backend::client::{Backend, ObjectData, ObjectId};
use wayland_backend::protocol::{AllowNull, Argument, ArgumentType, Interface, Message as Upstream};
use wayland_backend::smallvec::SmallVec;
use wayland_client::Proxy as _;
use wayland_client::protocol::wl_display::WlDisplay;

use super::frame::{Frame, Kit};
use super::interfaces;
use super::wire::{self, Arg, Message, SERVER_ID_START};

/// The bridge's `wl_display`.
const DISPLAY: u32 = 1;

/// The global name of the decoration manager the proxy offers, one the
/// compositor's own names (counted up from 1) do not reach.
const DECORATION_GLOBAL: u32 = u32::MAX;

/// `zxdg_toplevel_decoration_v1` modes.
const CLIENT_SIDE: u32 = 1;
const SERVER_SIDE: u32 = 2;

pub fn lock(proxy: &Mutex<Proxy>) -> MutexGuard<'_, Proxy> {
    proxy.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An object of the bridge's.
struct Obj {
    iface: &'static Interface,
    version: u32,
    kind: Kind,
    role: Role,
}

enum Kind {
    Display,
    /// Forwarded to this object on the compositor's connection.
    Upstream(ObjectId),
    /// The proxy's decoration manager, and its per-toplevel objects.
    DecorationManager,
    Decoration {
        toplevel: u32,
    },
}

/// What the proxy tracks about an object.
#[derive(Default)]
enum Role {
    #[default]
    None,
    Surface(Surface),
    Buffer {
        width: i32,
        height: i32,
    },
    BufferParams {
        size: Option<(i32, i32)>,
    },
    Viewport {
        surface: u32,
    },
    Output {
        scale: i32,
    },
    XdgSurface {
        surface: u32,
        toplevel: Option<u32>,
    },
    Toplevel(Toplevel),
    Popup {
        parent: u32,
    },
    Positioner {
        anchor: Option<[i32; 4]>,
        parent_size: Option<(i32, i32)>,
    },
    /// An object whose input events go to one surface at a time.
    Focus(Focus),
    /// A `wl_registry`, with the globals it advertised to the bridge by
    /// name: interface and version.
    Registry {
        globals: HashMap<u32, (&'static Interface, u32)>,
    },
}

/// A `wl_surface`'s size state, double-buffered as the protocol has it.
#[derive(Default)]
struct Surface {
    attached: Option<u32>,
    pending_attach: Option<Option<u32>>,
    scale: i32,
    pending_scale: Option<i32>,
    destination: Option<(i32, i32)>,
    pending_destination: Option<Option<(i32, i32)>>,
    preferred_scale: Option<i32>,
    outputs: Vec<u32>,
}

#[derive(Default)]
struct Toplevel {
    xdg_surface: u32,
    title: Option<String>,
    min: (i32, i32),
    max: (i32, i32),
}

/// Input focus on a surface the bridge does not know (a title bar): the
/// events that follow are not the bridge's.
#[derive(Default)]
struct Focus {
    hidden: bool,
    /// Something was forwarded since the last `frame` event.
    pending: bool,
    /// Touch points that went down on a hidden surface.
    touches: Vec<i32>,
}

impl Focus {
    /// Whether an event for an object with this focus goes to the bridge.
    /// `hidden` is set when the event names an object the bridge does not
    /// know.
    fn pass(&mut self, iface: &str, event: &str, hidden: bool, args: &[Arg]) -> bool {
        let forward = match (iface, event) {
            (_, "enter" | "proximity_in") => {
                self.hidden = hidden;
                !hidden
            }
            (_, "leave" | "proximity_out") => !std::mem::take(&mut self.hidden),
            (_, "frame") => std::mem::take(&mut self.pending),
            ("wl_touch", "down") => {
                if hidden {
                    self.touches.push(int(args, 3));
                }
                !hidden
            }
            ("wl_touch", "up") => {
                let id = int(args, 2);
                let known = !self.touches.contains(&id);
                self.touches.retain(|t| *t != id);
                known
            }
            ("wl_touch", "motion") => !self.touches.contains(&int(args, 1)),
            ("wl_touch", "shape" | "orientation") => !self.touches.contains(&int(args, 0)),
            ("wl_touch", "cancel") => {
                self.touches.clear();
                true
            }
            ("wl_pointer", _) => !self.hidden,
            ("wl_data_device", "motion" | "drop") => !self.hidden,
            (
                "zwp_tablet_tool_v2",
                "motion" | "pressure" | "distance" | "tilt" | "rotation" | "slider" | "wheel" | "button" | "down"
                | "up",
            ) => !self.hidden,
            _ => !hidden,
        };
        if forward && event != "frame" {
            self.pending = true;
        }
        forward
    }
}

impl Role {
    fn for_interface(name: &str) -> Role {
        match name {
            "wl_surface" => Role::Surface(Surface {
                scale: 1,
                ..Default::default()
            }),
            "wl_output" => Role::Output { scale: 1 },
            "zwp_linux_buffer_params_v1" => Role::BufferParams { size: None },
            "xdg_positioner" => Role::Positioner {
                anchor: None,
                parent_size: None,
            },
            "wl_pointer" | "wl_touch" | "wl_data_device" | "zwp_tablet_tool_v2" => Role::Focus(Focus::default()),
            "wl_registry" => Role::Registry {
                globals: HashMap::new(),
            },
            _ => Role::None,
        }
    }
}

pub struct Proxy {
    backend: Backend,
    pub bridge: wire::Socket,
    objects: HashMap<u32, Obj>,
    next_server_id: u32,
    /// The compositor draws no title bars, so the proxy offers to.
    decorate: bool,
    /// Frames by the bridge's toplevel id.
    pub frames: HashMap<u32, Frame>,
    /// What frames are made with, when the proxy draws them.
    kit: Option<Kit>,
    /// An error from forwarding an event, which ends the proxy.
    pub failed: Option<anyhow::Error>,
    key: String,
    this: Weak<Mutex<Proxy>>,
}

/// The object data of everything forwarded for the bridge: events go back
/// to the bridge's object with this id.
struct Forward {
    proxy: Weak<Mutex<Proxy>>,
    bridge: u32,
}

impl ObjectData for Forward {
    fn event(self: Arc<Self>, _: &Backend, msg: Upstream<ObjectId, OwnedFd>) -> Option<Arc<dyn ObjectData>> {
        match self.proxy.upgrade() {
            Some(proxy) => lock(&proxy).event(self.bridge, msg),
            None => discard(&msg),
        }
    }

    fn destroyed(&self, _: ObjectId) {}
}

/// The object data of objects created by an event that is not forwarded.
struct Discard;

impl ObjectData for Discard {
    fn event(self: Arc<Self>, _: &Backend, msg: Upstream<ObjectId, OwnedFd>) -> Option<Arc<dyn ObjectData>> {
        discard(&msg)
    }

    fn destroyed(&self, _: ObjectId) {}
}

/// The data an event that creates an object must return, when the event is
/// dropped.
fn discard(msg: &Upstream<ObjectId, OwnedFd>) -> Option<Arc<dyn ObjectData>> {
    msg.args
        .iter()
        .any(|a| matches!(a, Argument::NewId(_)))
        .then(|| Arc::new(Discard) as Arc<dyn ObjectData>)
}

fn opcode(messages: &[wayland_backend::protocol::MessageDesc], name: &str) -> u16 {
    messages
        .iter()
        .position(|m| m.name == name)
        .expect("the protocol defines this message") as u16
}

fn int(args: &[Arg], i: usize) -> i32 {
    match args.get(i) {
        Some(Argument::Int(v)) => *v,
        _ => 0,
    }
}

fn uint(args: &[Arg], i: usize) -> u32 {
    match args.get(i) {
        Some(Argument::Uint(v) | Argument::Object(v) | Argument::NewId(v)) => *v,
        _ => 0,
    }
}

impl Proxy {
    pub fn new(
        backend: Backend,
        bridge: wire::Socket,
        decorate: bool,
        kit: Option<Kit>,
        key: &str,
    ) -> Arc<Mutex<Proxy>> {
        Arc::new_cyclic(|this| {
            let objects = HashMap::from([(
                DISPLAY,
                Obj {
                    iface: WlDisplay::interface(),
                    version: 1,
                    kind: Kind::Display,
                    role: Role::None,
                },
            )]);
            Mutex::new(Proxy {
                backend,
                bridge,
                objects,
                next_server_id: SERVER_ID_START,
                decorate,
                frames: HashMap::new(),
                kit,
                failed: None,
                key: key.into(),
                this: this.clone(),
            })
        })
    }

    fn forward_data(&self, bridge: u32) -> Arc<dyn ObjectData> {
        Arc::new(Forward {
            proxy: self.this.clone(),
            bridge,
        })
    }

    /// Reads and forwards the bridge's requests. Returns `false` once it
    /// has closed its connection.
    pub fn read_bridge(&mut self) -> Result<bool> {
        let open = self.bridge.fill().context("read from xwayland-satellite")?;
        loop {
            let objects = &self.objects;
            let next = self.bridge.next(|sender, opcode| {
                let obj = objects.get(&sender)?;
                Some(obj.iface.requests.get(usize::from(opcode))?.signature)
            })?;
            match next {
                Some(msg) => self.request(msg)?,
                None => return Ok(open),
            }
        }
    }

    fn send(&mut self, sender: u32, opcode: u16, args: Vec<Arg>) {
        if let Err(e) = self.bridge.send(Message { sender, opcode, args }) {
            self.failed.get_or_insert(e);
        }
    }

    /// Tells the bridge an id it created is free again.
    fn delete_id(&mut self, id: u32) {
        if id < SERVER_ID_START {
            self.send(
                DISPLAY,
                opcode(WlDisplay::interface().events, "delete_id"),
                vec![Argument::Uint(id)],
            );
        }
    }

    fn check_new_id(&self, id: u32) -> Result<()> {
        if id == 0 || id >= SERVER_ID_START || self.objects.contains_key(&id) {
            bail!("xwayland-satellite created object {id}, which is invalid or in use");
        }
        Ok(())
    }

    fn upstream(&self, id: u32) -> Option<ObjectId> {
        match self.objects.get(&id)?.kind {
            Kind::Upstream(ref up) => Some(up.clone()),
            _ => None,
        }
    }

    /// Sends a request of the proxy's own on a forwarded object.
    fn request_upstream(&self, id: &ObjectId, name: &str, args: &[i32]) {
        let msg = Upstream {
            sender_id: id.clone(),
            opcode: opcode(id.interface().requests, name),
            args: args.iter().map(|v| Argument::Int(*v)).collect(),
        };
        let _ = self.backend.send_request(msg, None, None);
    }

    fn request(&mut self, msg: Message) -> Result<()> {
        let obj = &self.objects[&msg.sender];
        let desc = &obj.iface.requests[usize::from(msg.opcode)];
        if desc.since > obj.version {
            bail!(
                "xwayland-satellite sent {}.{} on version {}",
                obj.iface.name,
                desc.name,
                obj.version
            );
        }
        match obj.kind {
            Kind::Display => self.display_request(msg),
            Kind::DecorationManager => self.manager_request(msg),
            Kind::Decoration { toplevel } => self.decoration_request(msg, toplevel),
            Kind::Upstream(ref up) => {
                let up = up.clone();
                match obj.iface.name {
                    "wl_registry" => self.bind(up, msg),
                    _ => self.forward(up, msg),
                }
            }
        }
    }

    /// `wl_display.sync` and `wl_display.get_registry`, both forwarded, so a
    /// round trip still waits for the compositor.
    fn display_request(&mut self, msg: Message) -> Result<()> {
        let &[Argument::NewId(child)] = msg.args.as_slice() else {
            bail!("malformed wl_display request");
        };
        self.check_new_id(child)?;
        let desc = &WlDisplay::interface().requests[usize::from(msg.opcode)];
        let iface = desc
            .child_interface
            .expect("sync and get_registry name their interface");
        let up = self
            .backend
            .send_request(
                Upstream {
                    sender_id: self.backend.display_id(),
                    opcode: msg.opcode,
                    args: SmallVec::from_elem(Argument::NewId(ObjectId::null()), 1),
                },
                Some(self.forward_data(child)),
                None,
            )
            .map_err(|_| anyhow!("the compositor connection is gone"))?;
        self.objects.insert(
            child,
            Obj {
                iface,
                version: 1,
                kind: Kind::Upstream(up),
                role: Role::for_interface(iface.name),
            },
        );
        if iface.name == "wl_registry" && self.decorate {
            let manager = interfaces::decoration_manager();
            self.send(
                child,
                opcode(iface.events, "global"),
                vec![
                    Argument::Uint(DECORATION_GLOBAL),
                    Argument::Str(Some(Box::new(CString::new(manager.name).expect("no NUL")))),
                    Argument::Uint(1),
                ],
            );
        }
        Ok(())
    }

    /// `wl_registry.bind`, for a global the bridge was told about.
    fn bind(&mut self, registry: ObjectId, msg: Message) -> Result<()> {
        let [
            Argument::Uint(name),
            Argument::Str(Some(iface)),
            Argument::Uint(version),
            Argument::NewId(child),
        ] = msg.args.as_slice()
        else {
            bail!("malformed wl_registry.bind");
        };
        let (name, version, child) = (*name, *version, *child);
        self.check_new_id(child)?;
        let iface_name = iface.to_str().unwrap_or_default();

        if self.decorate && name == DECORATION_GLOBAL {
            let manager = interfaces::decoration_manager();
            if iface_name != manager.name || version != 1 {
                bail!("xwayland-satellite bound global {name} as {iface_name} version {version}");
            }
            self.objects.insert(
                child,
                Obj {
                    iface: manager,
                    version,
                    kind: Kind::DecorationManager,
                    role: Role::None,
                },
            );
            return Ok(());
        }

        let advertised = match self.objects.get(&msg.sender).map(|o| &o.role) {
            Some(Role::Registry { globals }) => globals.get(&name).copied(),
            _ => None,
        };
        let Some((global, most)) = advertised else {
            bail!("xwayland-satellite bound global {name}, which it was not offered");
        };
        if iface_name != global.name || version == 0 || version > most {
            bail!("xwayland-satellite bound global {name} as {iface_name} version {version}");
        }
        let args = [
            Argument::Uint(name),
            Argument::Str(Some(iface.clone())),
            Argument::Uint(version),
            Argument::NewId(ObjectId::null()),
        ];
        let up = self
            .backend
            .send_request(
                Upstream {
                    sender_id: registry,
                    opcode: msg.opcode,
                    args: args.into_iter().collect(),
                },
                Some(self.forward_data(child)),
                Some((global, version)),
            )
            .map_err(|_| anyhow!("the compositor connection is gone"))?;
        self.objects.insert(
            child,
            Obj {
                iface: global,
                version,
                kind: Kind::Upstream(up),
                role: Role::for_interface(global.name),
            },
        );
        Ok(())
    }

    /// Forwards a request, translating object ids.
    fn forward(&mut self, up: ObjectId, mut msg: Message) -> Result<()> {
        let sender = msg.sender;
        let (iface, version) = (self.objects[&sender].iface, self.objects[&sender].version);
        let desc = &iface.requests[usize::from(msg.opcode)];
        if !self.before_request(sender, iface.name, desc.name, &mut msg.args)? {
            return Ok(());
        }
        let after = self.after_request(sender, iface.name, desc.name, &msg.args);

        let mut child = None;
        // An object the compositor has destroyed already.
        let mut dead = false;
        let mut fds = Vec::new();
        let mut objects = desc.arg_interfaces.iter();
        let mut args = SmallVec::new();
        for (arg, ty) in msg.args.into_iter().zip(desc.signature) {
            args.push(match arg {
                Argument::Object(0) => {
                    objects.next();
                    if *ty != ArgumentType::Object(AllowNull::Yes) {
                        bail!("{}.{} passes a null object", iface.name, desc.name);
                    }
                    Argument::Object(ObjectId::null())
                }
                Argument::Object(id) => {
                    let expected = objects.next().map_or("", |i| i.name);
                    let obj = self
                        .objects
                        .get(&id)
                        .with_context(|| format!("{}.{} passes unknown object {id}", iface.name, desc.name))?;
                    let Kind::Upstream(ref target) = obj.kind else {
                        bail!("{}.{} passes an object of the proxy's", iface.name, desc.name);
                    };
                    dead |= target.is_null();
                    if !expected.is_empty() && expected != "<anonymous>" && expected != obj.iface.name {
                        bail!(
                            "{}.{} passes a {} for a {expected}",
                            iface.name,
                            desc.name,
                            obj.iface.name
                        );
                    }
                    Argument::Object(target.clone())
                }
                Argument::NewId(id) => {
                    self.check_new_id(id)?;
                    let child_iface = desc.child_interface.with_context(|| {
                        format!("{}.{} creates an object of no fixed interface", iface.name, desc.name)
                    })?;
                    child = Some((id, child_iface));
                    Argument::NewId(ObjectId::null())
                }
                Argument::Fd(fd) => {
                    let raw = fd.as_raw_fd();
                    fds.push(fd);
                    Argument::Fd(raw)
                }
                Argument::Int(v) => Argument::Int(v),
                Argument::Uint(v) => Argument::Uint(v),
                Argument::Fixed(v) => Argument::Fixed(v),
                Argument::Str(s) => Argument::Str(s),
                Argument::Array(a) => Argument::Array(a),
            });
        }

        let data = child.map(|(id, _)| self.forward_data(id));
        let msg_out = Upstream {
            sender_id: up,
            opcode: msg.opcode,
            args,
        };
        let sent = match dead {
            true => Err(()),
            false => self.backend.send_request(msg_out, data, None).map_err(drop),
        };
        drop(fds);
        match sent {
            Ok(new) => {
                if let Some((id, child_iface)) = child {
                    self.objects.insert(
                        id,
                        Obj {
                            iface: child_iface,
                            version,
                            kind: Kind::Upstream(new),
                            role: Role::for_interface(child_iface.name),
                        },
                    );
                }
            }
            // The request is on, or names, an object the compositor has
            // destroyed: dropped, as a compositor ignores requests on such
            // an object. One it would have created stays dead too.
            Err(()) => {
                if let Some((id, child_iface)) = child {
                    self.objects.insert(
                        id,
                        Obj {
                            iface: child_iface,
                            version,
                            kind: Kind::Upstream(ObjectId::null()),
                            role: Role::None,
                        },
                    );
                }
            }
        }
        self.apply(sender, child.map(|(id, _)| id), after);

        if desc.is_destructor {
            self.objects.remove(&sender);
            self.delete_id(sender);
        }
        Ok(())
    }

    /// Bookkeeping and adjustments before a request goes out. Returns
    /// whether to forward it.
    fn before_request(&mut self, sender: u32, iface: &str, request: &str, args: &mut [Arg]) -> Result<bool> {
        match (iface, request) {
            ("wl_surface", "attach") => {
                let buffer = uint(args, 0);
                if let Some(Role::Surface(s)) = self.role_mut(sender) {
                    s.pending_attach = Some((buffer != 0).then_some(buffer));
                }
            }
            ("wl_surface", "set_buffer_scale") => {
                let scale = int(args, 0);
                if let Some(Role::Surface(s)) = self.role_mut(sender) {
                    s.pending_scale = Some(scale.max(1));
                }
            }
            ("wl_surface", "commit") => {
                let size = self.commit_surface(sender);
                if let Some(frame) = self.frames.values_mut().find(|f| f.surface == sender) {
                    frame.commit(size);
                }
            }
            ("wp_viewport", "set_destination") => {
                let (w, h) = (int(args, 0), int(args, 1));
                let destination = (w > 0 && h > 0).then_some((w, h));
                self.set_destination(sender, destination);
            }
            ("wp_viewport", "destroy") => self.set_destination(sender, None),
            ("zwp_linux_buffer_params_v1", "create") => {
                let size = (int(args, 0), int(args, 1));
                if let Some(Role::BufferParams { size: s }) = self.role_mut(sender) {
                    *s = Some(size);
                }
            }
            ("xdg_surface", "set_window_geometry") => {
                let geometry = (int(args, 0), int(args, 1), int(args, 2), int(args, 3));
                if let Some(frame) = self.frame_of_xdg_surface(sender) {
                    frame.bridge_geometry = Some(geometry);
                    return Ok(false);
                }
            }
            ("xdg_surface", "get_popup") => {
                let (parent, positioner) = (uint(args, 1), uint(args, 2));
                self.offset_positioner(positioner, self.header(parent));
            }
            ("xdg_popup", "reposition") => {
                let positioner = uint(args, 0);
                self.offset_positioner(positioner, self.header(self.popup_parent(sender)));
            }
            ("xdg_positioner", "set_anchor_rect") => {
                let rect = [int(args, 0), int(args, 1), int(args, 2), int(args, 3)];
                if let Some(Role::Positioner { anchor, .. }) = self.role_mut(sender) {
                    *anchor = Some(rect);
                }
            }
            ("xdg_positioner", "set_parent_size") => {
                let size = (int(args, 0), int(args, 1));
                if let Some(Role::Positioner { parent_size, .. }) = self.role_mut(sender) {
                    *parent_size = Some(size);
                }
            }
            ("xdg_toplevel", "set_title") => {
                let title = match args.first() {
                    Some(Argument::Str(Some(s))) => s.to_string_lossy().into_owned(),
                    _ => String::new(),
                };
                if let Some(frame) = self.frames.get_mut(&sender) {
                    frame.set_title(&title);
                    frame.redraw();
                }
                if let Some(Role::Toplevel(t)) = self.role_mut(sender) {
                    t.title = Some(title);
                }
            }
            ("xdg_toplevel", "set_min_size" | "set_max_size") => {
                let size = (int(args, 0), int(args, 1));
                if let Some(Role::Toplevel(t)) = self.role_mut(sender) {
                    match request {
                        "set_min_size" => t.min = size,
                        _ => t.max = size,
                    }
                }
                let offset = self.frames.get(&sender).map_or(0, Frame::offset);
                if offset > 0 && size.1 > 0 {
                    args[1] = Argument::Int(size.1.saturating_add(offset));
                }
            }
            // The frame's subsurfaces go with it; the window geometry goes
            // with the toplevel.
            ("xdg_toplevel", "destroy") => drop(self.frames.remove(&sender)),
            _ => {}
        }
        Ok(true)
    }

    /// What to record about the objects a request creates, read from its
    /// arguments before they go out.
    fn after_request(&self, sender: u32, iface: &str, request: &str, args: &[Arg]) -> After {
        match (iface, request) {
            ("wl_shm_pool", "create_buffer") => After::Buffer(int(args, 2), int(args, 3)),
            ("zwp_linux_buffer_params_v1", "create_immed") => After::Buffer(int(args, 1), int(args, 2)),
            ("wp_viewporter", "get_viewport") => After::Viewport(uint(args, 1)),
            ("xdg_wm_base", "get_xdg_surface") => After::XdgSurface(uint(args, 1)),
            ("xdg_surface", "get_toplevel") => After::Toplevel,
            ("xdg_surface", "get_popup") => After::Popup {
                parent: uint(args, 1),
                positioner: uint(args, 2),
                offset: self.header(uint(args, 1)),
            },
            ("xdg_popup", "reposition") => After::Reposition {
                positioner: uint(args, 0),
                offset: self.header(self.popup_parent(sender)),
            },
            _ => After::Nothing,
        }
    }

    fn apply(&mut self, sender: u32, child: Option<u32>, after: After) {
        let role = match after {
            After::Nothing => return,
            After::Reposition { positioner, offset } => {
                self.restore_positioner(positioner, offset);
                return;
            }
            After::Buffer(width, height) => Role::Buffer { width, height },
            After::Viewport(surface) => Role::Viewport { surface },
            After::XdgSurface(surface) => Role::XdgSurface {
                surface,
                toplevel: None,
            },
            After::Toplevel => {
                if let (Some(Role::XdgSurface { toplevel, .. }), Some(child)) = (self.role_mut(sender), child) {
                    *toplevel = Some(child);
                }
                Role::Toplevel(Toplevel {
                    xdg_surface: sender,
                    ..Default::default()
                })
            }
            After::Popup {
                parent,
                positioner,
                offset,
            } => {
                self.restore_positioner(positioner, offset);
                Role::Popup { parent }
            }
        };
        if let Some(obj) = child.and_then(|c| self.objects.get_mut(&c)) {
            obj.role = role;
        }
    }

    fn role_mut(&mut self, id: u32) -> Option<&mut Role> {
        self.objects.get_mut(&id).map(|o| &mut o.role)
    }

    fn set_destination(&mut self, viewport: u32, destination: Option<(i32, i32)>) {
        let Some(&Role::Viewport { surface }) = self.objects.get(&viewport).map(|o| &o.role) else {
            return;
        };
        if let Some(Role::Surface(s)) = self.role_mut(surface) {
            s.pending_destination = Some(destination);
        }
    }

    /// Applies a surface's pending state and returns its size in surface
    /// coordinates, when it has content.
    fn commit_surface(&mut self, surface: u32) -> Option<(i32, i32)> {
        let Some(Role::Surface(s)) = self.role_mut(surface) else {
            return None;
        };
        if let Some(a) = s.pending_attach.take() {
            s.attached = a;
        }
        if let Some(scale) = s.pending_scale.take() {
            s.scale = scale;
        }
        if let Some(d) = s.pending_destination.take() {
            s.destination = d;
        }
        if let Some(d) = s.destination {
            return Some(d);
        }
        let (buffer, scale) = (s.attached?, s.scale);
        match self.objects.get(&buffer)?.role {
            Role::Buffer { width, height } => Some((width / scale, height / scale)),
            _ => None,
        }
    }

    /// The height of the title bar above the toplevel an `xdg_surface` is.
    fn header(&self, xdg_surface: u32) -> i32 {
        match self.objects.get(&xdg_surface).map(|o| &o.role) {
            Some(Role::XdgSurface { toplevel: Some(t), .. }) => self.frames.get(t).map_or(0, Frame::offset),
            _ => 0,
        }
    }

    fn frame_of_xdg_surface(&mut self, xdg_surface: u32) -> Option<&mut Frame> {
        let Some(Role::XdgSurface { toplevel: Some(t), .. }) = self.objects.get(&xdg_surface).map(|o| &o.role) else {
            return None;
        };
        let t = *t;
        self.frames.get_mut(&t)
    }

    fn popup_parent(&self, popup: u32) -> u32 {
        match self.objects.get(&popup).map(|o| &o.role) {
            Some(Role::Popup { parent }) => *parent,
            _ => 0,
        }
    }

    /// Moves a positioner's anchor `offset` down, and grows the parent size
    /// it was given by as much. A popup's position is relative to its
    /// parent's window geometry, which a title bar extends upwards, while
    /// the bridge places popups relative to the X11 window below it.
    fn offset_positioner(&self, positioner: u32, offset: i32) {
        if offset != 0 {
            self.set_positioner(positioner, offset);
        }
    }

    /// Puts back what [`Self::offset_positioner`] changed, so the positioner
    /// stays the bridge's for its next popup.
    fn restore_positioner(&self, positioner: u32, offset: i32) {
        if offset != 0 {
            self.set_positioner(positioner, 0);
        }
    }

    fn set_positioner(&self, positioner: u32, offset: i32) {
        let Some(obj) = self.objects.get(&positioner) else {
            return;
        };
        let (Kind::Upstream(up), Role::Positioner { anchor, parent_size }) = (&obj.kind, &obj.role) else {
            return;
        };
        if let Some([x, y, w, h]) = *anchor {
            self.request_upstream(up, "set_anchor_rect", &[x, y.saturating_add(offset), w, h]);
        }
        if let Some((w, h)) = *parent_size
            && obj.version >= 3
        {
            self.request_upstream(up, "set_parent_size", &[w, h.saturating_add(offset)]);
        }
    }

    /// Forwards an event to the bridge, returning the data of the object it
    /// creates, if any.
    pub fn event(&mut self, bridge: u32, msg: Upstream<ObjectId, OwnedFd>) -> Option<Arc<dyn ObjectData>> {
        let Some(obj) = self.objects.get(&bridge) else {
            return discard(&msg);
        };
        if !matches!(obj.kind, Kind::Upstream(ref up) if *up == msg.sender_id) {
            return discard(&msg);
        }
        let (iface, version) = (obj.iface, obj.version);
        let Some(desc) = iface.events.get(usize::from(msg.opcode)) else {
            return discard(&msg);
        };

        let mut hidden = false;
        let mut child = None;
        let mut args = Vec::with_capacity(msg.args.len());
        for arg in msg.args {
            args.push(match arg {
                Argument::Object(id) if id.is_null() => Argument::Object(0),
                Argument::Object(id) => match self.bridge_id(&id) {
                    Some(b) => Argument::Object(b),
                    None => {
                        hidden = true;
                        Argument::Object(0)
                    }
                },
                Argument::NewId(id) => {
                    let Some(child_iface) = desc.child_interface else {
                        return Some(Arc::new(Discard));
                    };
                    let b = self.next_server_id;
                    self.next_server_id = match b.checked_add(1) {
                        Some(next) => next,
                        None => {
                            self.failed.get_or_insert(anyhow!("ran out of object ids"));
                            return Some(Arc::new(Discard));
                        }
                    };
                    self.objects.insert(
                        b,
                        Obj {
                            iface: child_iface,
                            version,
                            kind: Kind::Upstream(id),
                            role: Role::for_interface(child_iface.name),
                        },
                    );
                    child = Some(self.forward_data(b));
                    Argument::NewId(b)
                }
                Argument::Int(v) => Argument::Int(v),
                Argument::Uint(v) => Argument::Uint(v),
                Argument::Fixed(v) => Argument::Fixed(v),
                Argument::Str(s) => Argument::Str(s),
                Argument::Array(a) => Argument::Array(a),
                Argument::Fd(fd) => Argument::Fd(fd),
            });
        }

        if self.before_event(bridge, iface.name, desc.name, hidden, &mut args) {
            self.send(bridge, msg.opcode, args);
        }
        if desc.is_destructor {
            self.objects.remove(&bridge);
            self.delete_id(bridge);
        }
        child
    }

    /// The bridge's id for a forwarded object; `None` for the proxy's own.
    fn bridge_id(&self, id: &ObjectId) -> Option<u32> {
        let data = self.backend.get_data(id.clone()).ok()?;
        let bridge = data.data_as_any().downcast_ref::<Forward>()?.bridge;
        matches!(self.objects.get(&bridge)?.kind, Kind::Upstream(ref up) if up == id).then_some(bridge)
    }

    /// Bookkeeping and adjustments before an event goes to the bridge.
    /// Returns whether to forward it.
    fn before_event(&mut self, bridge: u32, iface: &str, event: &str, hidden: bool, args: &mut [Arg]) -> bool {
        match (iface, event) {
            ("wl_registry", "global") => {
                let (name, version) = (uint(args, 0), uint(args, 2));
                let global = match &args[1] {
                    Argument::Str(Some(s)) => s.to_str().ok().and_then(interfaces::global),
                    _ => None,
                };
                let Some(global) =
                    global.filter(|g| !(self.decorate && g.name == interfaces::decoration_manager().name))
                else {
                    return false;
                };
                let version = version.min(global.version);
                args[2] = Argument::Uint(version);
                if let Some(Role::Registry { globals }) = self.role_mut(bridge) {
                    globals.insert(name, (global, version));
                }
                return true;
            }
            ("wl_registry", "global_remove") => {
                let name = uint(args, 0);
                return match self.role_mut(bridge) {
                    Some(Role::Registry { globals }) => globals.remove(&name).is_some(),
                    _ => false,
                };
            }
            ("xdg_toplevel", "configure") => {
                if let Some(frame) = self.frames.get_mut(&bridge) {
                    let states = match &args[2] {
                        Argument::Array(a) => a.to_vec(),
                        _ => Vec::new(),
                    };
                    let (w, h) = frame.configure(&states, int(args, 0), int(args, 1));
                    args[0] = Argument::Int(w);
                    args[1] = Argument::Int(h);
                }
            }
            ("xdg_toplevel", "configure_bounds") => {
                let offset = self.frames.get(&bridge).map_or(0, Frame::offset);
                let h = int(args, 1);
                if offset > 0 && h > 0 {
                    args[1] = Argument::Int((h - offset).max(1));
                }
            }
            ("xdg_toplevel", "wm_capabilities") => {
                if let (Some(frame), Some(Argument::Array(caps))) = (self.frames.get_mut(&bridge), args.first()) {
                    frame.set_capabilities(caps);
                }
            }
            ("xdg_popup", "configure") => {
                let parent = match self.objects.get(&bridge).map(|o| &o.role) {
                    Some(Role::Popup { parent }) => *parent,
                    _ => 0,
                };
                args[1] = Argument::Int(int(args, 1).saturating_sub(self.header(parent)));
            }
            ("wl_surface", "enter" | "leave" | "preferred_buffer_scale") => {
                let value = uint(args, 0);
                if let Some(Role::Surface(s)) = self.role_mut(bridge) {
                    match event {
                        "enter" => s.outputs.push(value),
                        "leave" => s.outputs.retain(|o| *o != value),
                        _ => s.preferred_scale = Some(value as i32),
                    }
                }
                self.rescale(|surface| surface == bridge);
            }
            ("wl_output", "scale") => {
                let scale = int(args, 0);
                if let Some(Role::Output { scale: s }) = self.role_mut(bridge) {
                    *s = scale.max(1);
                }
                let outputs: Vec<u32> = self
                    .objects
                    .iter()
                    .filter_map(|(id, o)| match &o.role {
                        Role::Surface(s) if s.outputs.contains(&bridge) => Some(*id),
                        _ => None,
                    })
                    .collect();
                self.rescale(|surface| outputs.contains(&surface));
            }
            ("zwp_linux_buffer_params_v1", "created") => {
                let (buffer, size) = match self.objects.get(&bridge).map(|o| &o.role) {
                    Some(Role::BufferParams { size: Some(size) }) => (uint(args, 0), *size),
                    _ => return !hidden,
                };
                if let Some(obj) = self.objects.get_mut(&buffer) {
                    obj.role = Role::Buffer {
                        width: size.0,
                        height: size.1,
                    };
                }
            }
            _ => {}
        }
        self.focus(bridge, iface, event, hidden, args)
    }

    /// Keeps input on title bars from the bridge: events on a surface it does
    /// not know are dropped, along with the ones that follow until the focus
    /// leaves it.
    fn focus(&mut self, bridge: u32, iface: &str, event: &str, hidden: bool, args: &[Arg]) -> bool {
        match self.role_mut(bridge) {
            Some(Role::Focus(f)) => f.pass(iface, event, hidden, args),
            _ => !hidden,
        }
    }

    /// Updates the scale of frames whose surfaces `affected` matches.
    fn rescale(&mut self, affected: impl Fn(u32) -> bool) {
        let scales: Vec<(u32, i32)> = self
            .frames
            .iter()
            .filter(|(_, f)| affected(f.surface))
            .map(|(t, f)| (*t, self.surface_scale(f.surface)))
            .collect();
        for (t, scale) in scales {
            if let Some(frame) = self.frames.get_mut(&t) {
                frame.set_scale(scale);
            }
        }
    }

    /// The buffer scale a surface prefers: what the compositor says, or the
    /// largest scale of the outputs it is on.
    fn surface_scale(&self, surface: u32) -> i32 {
        let Some(Role::Surface(s)) = self.objects.get(&surface).map(|o| &o.role) else {
            return 1;
        };
        s.preferred_scale.unwrap_or_else(|| {
            s.outputs
                .iter()
                .filter_map(|o| match self.objects.get(o)?.role {
                    Role::Output { scale } => Some(scale),
                    _ => None,
                })
                .max()
                .unwrap_or(1)
        })
    }

    fn manager_request(&mut self, msg: Message) -> Result<()> {
        let sender = msg.sender;
        match (msg.opcode, msg.args.as_slice()) {
            (0, []) => {
                self.objects.remove(&sender);
                self.delete_id(sender);
            }
            (1, &[Argument::NewId(id), Argument::Object(toplevel)]) => {
                self.check_new_id(id)?;
                if !matches!(self.objects.get(&toplevel).map(|o| &o.role), Some(Role::Toplevel(_))) {
                    bail!("decoration requested for object {toplevel}, which is not a toplevel");
                }
                self.objects.insert(
                    id,
                    Obj {
                        iface: interfaces::toplevel_decoration(),
                        version: 1,
                        kind: Kind::Decoration { toplevel },
                        role: Role::None,
                    },
                );
            }
            _ => bail!("malformed zxdg_decoration_manager_v1 request"),
        }
        Ok(())
    }

    fn decoration_request(&mut self, msg: Message, toplevel: u32) -> Result<()> {
        let sender = msg.sender;
        let mode = match (msg.opcode, msg.args.as_slice()) {
            (0, []) => {
                self.remove_frame(toplevel);
                self.objects.remove(&sender);
                self.delete_id(sender);
                return Ok(());
            }
            (1, &[Argument::Uint(mode)]) if mode == CLIENT_SIDE || mode == SERVER_SIDE => mode,
            (2, []) => SERVER_SIDE,
            _ => bail!("malformed zxdg_toplevel_decoration_v1 request"),
        };
        let mode = match mode {
            SERVER_SIDE => match self.add_frame(toplevel) {
                Ok(()) => SERVER_SIDE,
                Err(e) => {
                    eprintln!("psbox[{}]: cannot draw a title bar: {e:#}", self.key);
                    CLIENT_SIDE
                }
            },
            _ => {
                self.remove_frame(toplevel);
                CLIENT_SIDE
            }
        };
        let configure = opcode(interfaces::toplevel_decoration().events, "configure");
        self.send(sender, configure, vec![Argument::Uint(mode)]);
        Ok(())
    }

    fn add_frame(&mut self, toplevel: u32) -> Result<()> {
        if self.frames.contains_key(&toplevel) {
            return Ok(());
        }
        let Some(Role::Toplevel(t)) = self.objects.get(&toplevel).map(|o| &o.role) else {
            bail!("object {toplevel} is not a toplevel");
        };
        let (xdg_surface, title, min, max) = (t.xdg_surface, t.title.clone(), t.min, t.max);
        let Some(&Role::XdgSurface { surface, .. }) = self.objects.get(&xdg_surface).map(|o| &o.role) else {
            bail!("toplevel {toplevel} has no surface");
        };
        let ids = (
            self.upstream(surface),
            self.upstream(xdg_surface),
            self.upstream(toplevel),
        );
        let (Some(surface_up), Some(xdg_up), Some(toplevel_up)) = ids else {
            bail!("toplevel {toplevel} is not on the compositor's connection");
        };
        let kit = self.kit.as_ref().context("the proxy draws no title bars")?;
        let mut frame = Frame::new(kit, surface, &surface_up, &xdg_up, &toplevel_up)?;
        if let Some(title) = title {
            frame.set_title(&title);
        }
        frame.set_scale(self.surface_scale(surface));
        let offset = frame.offset();
        self.frames.insert(toplevel, frame);
        self.resend_size_limits(&toplevel_up, min, max, offset);
        Ok(())
    }

    fn remove_frame(&mut self, toplevel: u32) {
        let Some(frame) = self.frames.remove(&toplevel) else {
            return;
        };
        frame.remove();
        if let (Some(Role::Toplevel(t)), Some(up)) =
            (self.objects.get(&toplevel).map(|o| &o.role), self.upstream(toplevel))
        {
            self.resend_size_limits(&up, t.min, t.max, 0);
        }
    }

    /// The bridge's size limits, which leave out the title bar.
    fn resend_size_limits(&self, toplevel: &ObjectId, min: (i32, i32), max: (i32, i32), offset: i32) {
        for (name, (w, h)) in [("set_min_size", min), ("set_max_size", max)] {
            if (w, h) != (0, 0) {
                let h = if h > 0 { h.saturating_add(offset) } else { h };
                self.request_upstream(toplevel, name, &[w, h]);
            }
        }
    }

    /// Carries out what a click on a title bar asks for.
    pub fn frame_action(
        &mut self,
        toplevel: u32,
        action: FrameAction,
        seat: &wayland_client::protocol::wl_seat::WlSeat,
        serial: u32,
    ) {
        match action {
            FrameAction::Close => {
                if let Some(obj) = self.objects.get(&toplevel) {
                    let close = opcode(obj.iface.events, "close");
                    self.send(toplevel, close, Vec::new());
                }
            }
            action => {
                if let Some(frame) = self.frames.get(&toplevel) {
                    frame.act(action, seat, serial);
                }
            }
        }
    }
}

/// What a request creates, to record once it has gone out.
enum After {
    Nothing,
    Buffer(i32, i32),
    Viewport(u32),
    XdgSurface(u32),
    Toplevel,
    Popup { parent: u32, positioner: u32, offset: i32 },
    Reposition { positioner: u32, offset: i32 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use wayland_client::protocol::{wl_callback, wl_compositor, wl_registry, wl_surface};

    /// A proxy between a fake bridge and a fake compositor, both raw sockets.
    struct Harness {
        proxy: Arc<Mutex<Proxy>>,
        backend: Backend,
        bridge: UnixStream,
        compositor: UnixStream,
    }

    fn encode(sender: u32, opcode: u16, args: Vec<Arg>) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write(Message { sender, opcode, args }, &mut out, &mut Vec::new()).unwrap();
        out
    }

    fn string(s: &str) -> Arg {
        Argument::Str(Some(Box::new(CString::new(s).unwrap())))
    }

    impl Harness {
        fn new() -> Harness {
            let (client, compositor) = UnixStream::pair().unwrap();
            let backend = Backend::connect(client).unwrap();
            let (ours, bridge) = UnixStream::pair().unwrap();
            bridge.set_nonblocking(true).unwrap();
            let proxy = Proxy::new(backend.clone(), wire::Socket::new(ours.into()), true, None, "test");
            Harness {
                proxy,
                backend,
                bridge,
                compositor,
            }
        }

        /// A request from the bridge, as the proxy handles it.
        fn request(&mut self, sender: u32, opcode: u16, args: Vec<Arg>) -> Result<()> {
            self.bridge.write_all(&encode(sender, opcode, args)).unwrap();
            lock(&self.proxy).read_bridge().map(drop)
        }

        /// An event from the compositor, dispatched to the proxy.
        fn event(&mut self, sender: u32, opcode: u16, args: Vec<Arg>) {
            self.compositor.write_all(&encode(sender, opcode, args)).unwrap();
            self.backend.prepare_read().unwrap().read().unwrap();
        }

        /// The events the bridge got, read by the interfaces of their
        /// senders.
        fn bridge_events(&mut self, interfaces: &[(u32, &'static Interface)]) -> Vec<Message> {
            lock(&self.proxy).bridge.flush().unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0u8; 4096];
            while let Ok(n) = self.bridge.read(&mut buf) {
                bytes.extend_from_slice(&buf[..n]);
            }
            let mut rest = &bytes[..];
            let mut events = Vec::new();
            let mut fds = VecDeque::new();
            while let Some((msg, size)) = wire::parse(rest, &mut fds, |sender, opcode| {
                let iface = interfaces.iter().find(|(id, _)| *id == sender)?.1;
                Some(iface.events.get(usize::from(opcode))?.signature)
            })
            .unwrap()
            {
                events.push(msg);
                rest = &rest[size..];
            }
            events
        }

        /// A bridge with a registry (2 on both sides) that offered
        /// `wl_compositor` as global 1.
        fn with_registry() -> Harness {
            let mut h = Harness::new();
            h.request(DISPLAY, 1, vec![Argument::NewId(2)]).unwrap();
            h.event(
                2,
                0,
                vec![Argument::Uint(1), string("wl_compositor"), Argument::Uint(6)],
            );
            h
        }
    }

    #[test]
    fn globals_are_filtered_and_binds_checked() {
        let mut h = Harness::new();
        h.request(DISPLAY, 1, vec![Argument::NewId(2)]).unwrap();
        // The compositor offers a newer wl_compositor than the proxy knows,
        // and a global the bridge does not get.
        h.event(
            2,
            0,
            vec![Argument::Uint(1), string("wl_compositor"), Argument::Uint(99)],
        );
        h.event(
            2,
            0,
            vec![Argument::Uint(2), string("wp_presentation"), Argument::Uint(1)],
        );
        let registry = wl_registry::WlRegistry::interface();
        let globals: Vec<(u32, String, u32)> = h
            .bridge_events(&[(2, registry)])
            .into_iter()
            .map(|m| match &m.args[..] {
                [Argument::Uint(n), Argument::Str(Some(i)), Argument::Uint(v)] => {
                    (*n, i.to_string_lossy().into_owned(), *v)
                }
                _ => panic!("not a global: {m:?}"),
            })
            .collect();
        let compositor = wl_compositor::WlCompositor::interface().version;
        assert_eq!(
            globals,
            [
                (DECORATION_GLOBAL, "zxdg_decoration_manager_v1".into(), 1),
                (1, "wl_compositor".into(), compositor),
            ]
        );

        let bind = |name, iface: &str, version, id| {
            vec![
                Argument::Uint(name),
                string(iface),
                Argument::Uint(version),
                Argument::NewId(id),
            ]
        };
        for (name, iface, version) in [
            (2, "wp_presentation", 1),
            (1, "wl_shm", 1),
            (1, "wl_compositor", compositor + 1),
            (1, "wl_compositor", 0),
            (DECORATION_GLOBAL, "zxdg_decoration_manager_v1", 2),
        ] {
            let mut h = Harness::with_registry();
            assert!(
                h.request(2, 0, bind(name, iface, version, 3)).is_err(),
                "{iface} {version}"
            );
        }
        h.request(2, 0, bind(1, "wl_compositor", compositor, 3)).unwrap();
        h.request(2, 0, bind(DECORATION_GLOBAL, "zxdg_decoration_manager_v1", 1, 4))
            .unwrap();
        // An id in use, and a server id.
        assert!(h.request(3, 0, vec![Argument::NewId(4)]).is_err());
        let mut h = Harness::with_registry();
        assert!(h.request(2, 0, bind(1, "wl_compositor", 1, SERVER_ID_START)).is_err());
        // A global is bound through the registry that offered it.
        let mut h = Harness::with_registry();
        h.request(DISPLAY, 1, vec![Argument::NewId(3)]).unwrap();
        assert!(h.request(3, 0, bind(1, "wl_compositor", 1, 4)).is_err());
        h.request(2, 0, bind(1, "wl_compositor", 1, 4)).unwrap();
    }

    #[test]
    fn requests_are_checked_before_they_reach_the_compositor() {
        let surface = |version| {
            let mut h = Harness::with_registry();
            let bind = vec![
                Argument::Uint(1),
                string("wl_compositor"),
                Argument::Uint(version),
                Argument::NewId(3),
            ];
            h.request(2, 0, bind).unwrap();
            h.request(3, 0, vec![Argument::NewId(4)]).unwrap();
            h
        };
        let attach = opcode(wl_surface::WlSurface::interface().requests, "attach");
        let damage_buffer = opcode(wl_surface::WlSurface::interface().requests, "damage_buffer");
        let attach_args = |buffer| vec![Argument::Object(buffer), Argument::Int(0), Argument::Int(0)];

        // A null buffer is allowed; the compositor where a buffer goes, or
        // an object that does not exist, are not.
        surface(6).request(4, attach, attach_args(0)).unwrap();
        assert!(surface(6).request(4, attach, attach_args(3)).is_err());
        assert!(surface(6).request(4, attach, attach_args(77)).is_err());
        // A request newer than the object.
        assert!(
            surface(3)
                .request(4, damage_buffer, (0..4).map(|_| Argument::Int(0)).collect())
                .is_err()
        );
        surface(4)
            .request(4, damage_buffer, (0..4).map(|_| Argument::Int(0)).collect())
            .unwrap();
        // The proxy's own objects never go to the compositor.
        let mut h = surface(6);
        let bind = vec![
            Argument::Uint(DECORATION_GLOBAL),
            string("zxdg_decoration_manager_v1"),
            Argument::Uint(1),
            Argument::NewId(5),
        ];
        h.request(2, 0, bind).unwrap();
        assert!(h.request(4, attach, attach_args(5)).is_err());
        // A decoration for something that is not a toplevel.
        assert!(h.request(5, 1, vec![Argument::NewId(6), Argument::Object(4)]).is_err());
    }

    #[test]
    fn destroyed_objects_free_their_ids() {
        let mut h = Harness::new();
        // wl_display.sync: the callback is 2 upstream too.
        h.request(DISPLAY, 0, vec![Argument::NewId(9)]).unwrap();
        h.event(2, 0, vec![Argument::Uint(5)]);
        let events = h.bridge_events(&[
            (DISPLAY, WlDisplay::interface()),
            (9, wl_callback::WlCallback::interface()),
        ]);
        let summary: Vec<(u32, u16, u32)> = events.iter().map(|m| (m.sender, m.opcode, uint(&m.args, 0))).collect();
        assert_eq!(summary, [(9, 0, 5), (DISPLAY, 1, 9)]);
        // The id is free for the bridge to use again.
        h.request(DISPLAY, 0, vec![Argument::NewId(9)]).unwrap();
    }

    #[test]
    fn input_on_title_bars_stays_away_from_the_bridge() {
        let mut pointer = Focus::default();
        let ev = |f: &mut Focus, event: &str, hidden: bool| f.pass("wl_pointer", event, hidden, &[]);
        // Into the window, then from it onto a title bar in one frame: the
        // bridge gets its leave and the frame that ends it, nothing else.
        assert!(ev(&mut pointer, "enter", false));
        assert!(ev(&mut pointer, "frame", false));
        assert!(ev(&mut pointer, "leave", false));
        assert!(!ev(&mut pointer, "enter", true));
        assert!(ev(&mut pointer, "frame", false));
        for event in ["motion", "button", "axis", "frame"] {
            assert!(!ev(&mut pointer, event, false), "{event}");
        }
        // Off the title bar and back into the window.
        assert!(!ev(&mut pointer, "leave", true));
        assert!(ev(&mut pointer, "enter", false));
        assert!(ev(&mut pointer, "motion", false));
        assert!(ev(&mut pointer, "frame", false));

        let mut touch = Focus::default();
        let down = |id| {
            [
                Argument::Uint(1),
                Argument::Uint(2),
                Argument::Object(0),
                Argument::Int(id),
            ]
        };
        assert!(!touch.pass("wl_touch", "down", true, &down(7)));
        assert!(touch.pass("wl_touch", "down", false, &down(8)));
        assert!(!touch.pass("wl_touch", "motion", false, &[Argument::Uint(3), Argument::Int(7)]));
        assert!(touch.pass("wl_touch", "motion", false, &[Argument::Uint(3), Argument::Int(8)]));
        assert!(!touch.pass(
            "wl_touch",
            "up",
            false,
            &[Argument::Uint(4), Argument::Uint(5), Argument::Int(7)]
        ));
        assert!(touch.pass("wl_touch", "motion", false, &[Argument::Uint(3), Argument::Int(7)]));

        // A drag over a title bar, while the clipboard still reaches the
        // bridge.
        let mut device = Focus::default();
        assert!(!device.pass("wl_data_device", "enter", true, &[]));
        assert!(!device.pass("wl_data_device", "motion", false, &[]));
        assert!(device.pass("wl_data_device", "selection", false, &[]));
        assert!(!device.pass("wl_data_device", "leave", false, &[]));
        assert!(device.pass("wl_data_device", "enter", false, &[]));
    }
}
