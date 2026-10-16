//! Fuzzes psbox::fuzz::config.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| psbox::fuzz::config(data));
