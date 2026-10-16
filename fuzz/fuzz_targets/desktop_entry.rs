//! Fuzzes psbox::fuzz::desktop_entry.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| psbox::fuzz::desktop_entry(data));
