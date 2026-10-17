//! Fuzzes psbox::fuzz::wayland_message.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| psbox::fuzz::wayland_message(data));
