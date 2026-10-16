//! Entry points for the fuzz targets in `fuzz/`. Each one feeds arbitrary
//! bytes to a parser that reads input psbox does not control, and must
//! never panic, whatever the input.

use crate::dbus::message::Message;
use crate::plan::{HostEnv, Identity, Plan};
use crate::protocol::{Request, Response};
use crate::spec::Spec;

/// A sandbox config, and when it parses, the plan built from it.
pub fn config(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(spec) = Spec::parse(text) else {
        return;
    };
    let host = HostEnv {
        uid: 1000,
        gid: 1000,
        user: "u".into(),
        home: "/home/u".into(),
        runtime_dir: "/run/user/1000".into(),
        wayland_socket: Some("/run/user/1000/wayland-0".into()),
        session_bus: Some("/run/user/1000/bus".into()),
        config_dir: "/home/u/.config/psbox".into(),
        root: "/home/u/Sandbox".into(),
        pasta: true,
    };
    let id = Identity::Instance {
        sandbox: "app".into(),
        instance: None,
    };
    let _ = Plan::build(&id, &spec, &host);
    for task in spec.task.keys() {
        let id = Identity::Task {
            sandbox: "app".into(),
            task: task.clone(),
        };
        let _ = Plan::build(&id, &spec, &host);
    }
}

/// A request or response on a supervisor socket.
pub fn protocol(data: &[u8]) {
    let _ = serde_json::from_slice::<Request>(data);
    let _ = serde_json::from_slice::<Response>(data);
}

/// A D-Bus message from a sandbox client, as the gateway sees it: parsed,
/// its arguments decoded, checked against the portal policy, and encoded
/// again for the host.
pub fn dbus_message(data: &[u8]) {
    let Ok(msg) = Message::parse(data) else {
        return;
    };
    let _ = msg.args();
    let _ = crate::portal::check(&crate::portal::Policy::default(), &msg);
    let encoded = msg.encode(1);
    let _ = Message::parse(&encoded);
}

/// A host desktop entry, rewritten the way `psbox install` does.
pub fn desktop_entry(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(entry) = crate::desktop::Entry::parse(text) else {
        return;
    };
    let _ = entry.render("app");
    for section in entry.exec_sections() {
        if let Some(exec) = entry.get(&section, "Exec") {
            let _ = crate::desktop::launch_for(exec, &[]);
        }
    }
}
