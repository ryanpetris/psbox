//! Fuzzes psbox::fuzz::dbus_message.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| psbox::fuzz::dbus_message(data));
