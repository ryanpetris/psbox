//! The systemd user manager, over the session bus.

use anyhow::{Context, Result};

use crate::dbus::message::Message;
use crate::dbus::{Conn, Value};

const DEST: &str = "org.freedesktop.systemd1";
const PATH: &str = "/org/freedesktop/systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";

pub struct Systemd(Conn);

/// The unit name prefix for psbox supervisors (`psbox@<key>`).
pub const UNIT_PREFIX: &str = "psbox@";

pub fn socket_unit(key: &str) -> String {
    format!("{UNIT_PREFIX}{key}.socket")
}

pub fn service_unit(key: &str) -> String {
    format!("{UNIT_PREFIX}{key}.service")
}

impl Systemd {
    /// Connects to the session bus the user manager is on.
    pub fn connect() -> Result<Self> {
        let address = match std::env::var("DBUS_SESSION_BUS_ADDRESS") {
            Ok(a) if !a.is_empty() => a,
            _ => {
                let rt = std::env::var("XDG_RUNTIME_DIR").context("no session bus: XDG_RUNTIME_DIR is not set")?;
                format!("unix:path={rt}/bus")
            }
        };
        Ok(Self(Conn::connect(&address).context("connect to the session bus")?))
    }

    fn call(&self, member: &str, args: &[Value]) -> Result<Message> {
        self.0
            .call(&Message::method_call(DEST, PATH, MANAGER, member, args))
            .with_context(|| format!("systemd {member}"))
    }

    pub fn start(&self, unit: &str) -> Result<()> {
        self.call("StartUnit", &[Value::Str(unit.into()), Value::Str("replace".into())])?;
        Ok(())
    }

    pub fn stop(&self, unit: &str) -> Result<()> {
        self.call("StopUnit", &[Value::Str(unit.into()), Value::Str("replace".into())])?;
        Ok(())
    }

    /// Names of active units matching `pattern`.
    pub fn active_units(&self, pattern: &str) -> Result<Vec<String>> {
        self.units(&["active"], pattern)
    }

    /// Names of units matching `pattern` in one of `states`.
    fn units(&self, states: &[&str], pattern: &str) -> Result<Vec<String>> {
        let reply = self.call(
            "ListUnitsByPatterns",
            &[
                Value::Array("s".into(), states.iter().map(|s| Value::Str(s.to_string())).collect()),
                Value::Array("s".into(), vec![Value::Str(pattern.into())]),
            ],
        )?;
        let args = reply.args()?;
        let Some(Value::Array(_, units)) = args.into_iter().next() else {
            return Ok(Vec::new());
        };
        Ok(units
            .into_iter()
            .filter_map(|u| match u {
                Value::Struct(fields) => fields.first().and_then(Value::as_str).map(str::to_string),
                _ => None,
            })
            .collect())
    }

    /// The state of a unit file (`static`, `enabled`, ...), or an error when
    /// systemd has no such unit file.
    pub fn unit_file_state(&self, unit: &str) -> Result<String> {
        let reply = self.call("GetUnitFileState", &[Value::Str(unit.into())])?;
        Ok(reply
            .args()?
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Whether `unit` is starting, running, or stopping.
    pub fn is_up(&self, unit: &str) -> Result<bool> {
        let states = ["active", "activating", "deactivating", "reloading", "refreshing"];
        Ok(self.units(&states, unit)?.iter().any(|u| u == unit))
    }

    pub fn is_active(&self, unit: &str) -> Result<bool> {
        Ok(self.active_units(unit)?.iter().any(|u| u == unit))
    }
}
