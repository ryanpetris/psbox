//! Fuzzes psbox::fuzz::protocol.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| psbox::fuzz::protocol(data));
