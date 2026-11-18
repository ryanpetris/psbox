//! Fuzzes processed udev packets and database records.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| psbox::fuzz::udev(data));
