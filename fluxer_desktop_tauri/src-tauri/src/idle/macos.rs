// SPDX-License-Identifier: AGPL-3.0-or-later

//! `HIDIdleTime` of the IOHIDSystem service, in nanoseconds since the last input event. Written
//! against Apple's IOKit and CoreFoundation C headers; not yet built or run on macOS.

use std::ffi::{c_char, c_void};

type CFTypeRef = *const c_void;
type IoObject = u32;

const IO_MAIN_PORT_DEFAULT: u32 = 0;
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const CF_NUMBER_SINT64_TYPE: isize = 4;
const NANOS_PER_MILLI: u64 = 1_000_000;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> IoObject;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: CFTypeRef,
        allocator: CFTypeRef,
        options: u32,
    ) -> CFTypeRef;
    fn IOObjectRelease(object: IoObject) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(
        allocator: CFTypeRef,
        c_str: *const c_char,
        encoding: u32,
    ) -> CFTypeRef;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(number: CFTypeRef, number_type: isize, value: *mut c_void) -> u8;
    fn CFRelease(cf: CFTypeRef);
}

#[derive(Default)]
pub struct SystemIdle;

impl SystemIdle {
    pub fn idle_time_ms(&self) -> Result<u64, String> {
        // SAFETY: each Create/Copy result is released once below; IOServiceGetMatchingService
        // consumes the matching dictionary.
        unsafe {
            let service = IOServiceGetMatchingService(
                IO_MAIN_PORT_DEFAULT,
                IOServiceMatching(c"IOHIDSystem".as_ptr()),
            );
            if service == 0 {
                return Err("IOHIDSystem service not found".to_owned());
            }
            let key = CFStringCreateWithCString(
                std::ptr::null(),
                c"HIDIdleTime".as_ptr(),
                CF_STRING_ENCODING_UTF8,
            );
            let property = IORegistryEntryCreateCFProperty(service, key, std::ptr::null(), 0);
            CFRelease(key);
            IOObjectRelease(service);
            if property.is_null() {
                return Err("IOHIDSystem has no HIDIdleTime".to_owned());
            }
            let mut nanos = 0i64;
            let read = CFGetTypeID(property) == CFNumberGetTypeID()
                && CFNumberGetValue(property, CF_NUMBER_SINT64_TYPE, (&raw mut nanos).cast()) != 0;
            CFRelease(property);
            if !read || nanos < 0 {
                return Err("HIDIdleTime is not a non-negative 64-bit number".to_owned());
            }
            Ok(nanos as u64 / NANOS_PER_MILLI)
        }
    }
}
