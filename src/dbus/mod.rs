//! A minimal D-Bus implementation: wire format, client connections, and
//! nothing else. Bodies are decoded only where psbox needs to inspect or
//! rewrite them.

pub mod conn;
pub mod message;
pub mod value;

pub use conn::Conn;
pub use value::Value;
