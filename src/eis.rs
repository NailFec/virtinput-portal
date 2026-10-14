//! A small safe wrapper around the libeis server API.
//!
//! The context is not thread-safe. Every call must happen on the thread that
//! created it.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::io;
use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd, RawFd};

use rustix::io::Errno;

const EIS_FLAG_DEVICE_READY: u32 = 1;
const EIS_LOG_PRIORITY_WARNING: u32 = 30;

pub const EVENT_CLIENT_CONNECT: u32 = 1;
pub const EVENT_CLIENT_DISCONNECT: u32 = 2;
pub const EVENT_SEAT_BIND: u32 = 3;
pub const EVENT_DEVICE_CLOSED: u32 = 4;
pub const EVENT_DEVICE_READY: u32 = 5;
pub const EVENT_FRAME: u32 = 100;
pub const EVENT_DEVICE_START_EMULATING: u32 = 200;
pub const EVENT_DEVICE_STOP_EMULATING: u32 = 201;
pub const EVENT_POINTER_MOTION: u32 = 300;
pub const EVENT_POINTER_MOTION_ABSOLUTE: u32 = 400;
pub const EVENT_BUTTON: u32 = 500;
pub const EVENT_SCROLL_DELTA: u32 = 600;
pub const EVENT_SCROLL_STOP: u32 = 601;
pub const EVENT_SCROLL_CANCEL: u32 = 602;
pub const EVENT_SCROLL_DISCRETE: u32 = 603;
pub const EVENT_KEYBOARD_KEY: u32 = 700;

pub const CAP_POINTER: u32 = 1 << 0;
pub const CAP_POINTER_ABSOLUTE: u32 = 1 << 1;
pub const CAP_KEYBOARD: u32 = 1 << 2;
pub const CAP_SCROLL: u32 = 1 << 4;
pub const CAP_BUTTON: u32 = 1 << 5;

const DEVICE_TYPE_VIRTUAL: u32 = 1;
const KEYMAP_TYPE_XKB: u32 = 1;

#[repr(C)]
struct Eis {
    _private: [u8; 0],
}
#[repr(C)]
struct EisClient {
    _private: [u8; 0],
}
#[repr(C)]
struct EisSeat {
    _private: [u8; 0],
}
#[repr(C)]
struct EisDevice {
    _private: [u8; 0],
}
#[repr(C)]
struct EisEvent {
    _private: [u8; 0],
}
#[repr(C)]
struct EisRegion {
    _private: [u8; 0],
}
#[repr(C)]
struct EisKeymap {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn eis_new(user_data: *mut c_void) -> *mut Eis;
    fn eis_unref(eis: *mut Eis) -> *mut Eis;
    fn eis_set_flag(eis: *mut Eis, flag: u32) -> c_int;
    fn eis_log_set_priority(eis: *mut Eis, priority: u32);
    fn eis_setup_backend_fd(ctx: *mut Eis) -> c_int;
    fn eis_backend_fd_add_client(ctx: *mut Eis) -> c_int;
    fn eis_get_fd(eis: *mut Eis) -> c_int;
    fn eis_dispatch(eis: *mut Eis);
    fn eis_get_event(eis: *mut Eis) -> *mut EisEvent;
    fn eis_event_unref(event: *mut EisEvent) -> *mut EisEvent;
    fn eis_event_get_type(event: *mut EisEvent) -> u32;
    fn eis_event_get_client(event: *mut EisEvent) -> *mut EisClient;
    fn eis_event_get_seat(event: *mut EisEvent) -> *mut EisSeat;
    fn eis_event_get_device(event: *mut EisEvent) -> *mut EisDevice;
    fn eis_event_seat_has_capability(event: *mut EisEvent, cap: u32) -> bool;
    fn eis_event_pointer_get_dx(event: *mut EisEvent) -> f64;
    fn eis_event_pointer_get_dy(event: *mut EisEvent) -> f64;
    fn eis_event_pointer_get_absolute_x(event: *mut EisEvent) -> f64;
    fn eis_event_pointer_get_absolute_y(event: *mut EisEvent) -> f64;
    fn eis_event_button_get_button(event: *mut EisEvent) -> u32;
    fn eis_event_button_get_is_press(event: *mut EisEvent) -> bool;
    fn eis_event_scroll_get_dx(event: *mut EisEvent) -> f64;
    fn eis_event_scroll_get_dy(event: *mut EisEvent) -> f64;
    fn eis_event_scroll_get_stop_x(event: *mut EisEvent) -> bool;
    fn eis_event_scroll_get_stop_y(event: *mut EisEvent) -> bool;
    fn eis_event_scroll_get_discrete_dx(event: *mut EisEvent) -> i32;
    fn eis_event_scroll_get_discrete_dy(event: *mut EisEvent) -> i32;
    fn eis_event_keyboard_get_key(event: *mut EisEvent) -> u32;
    fn eis_event_keyboard_get_key_is_press(event: *mut EisEvent) -> bool;
    fn eis_client_is_sender(client: *mut EisClient) -> bool;
    fn eis_client_get_name(client: *mut EisClient) -> *const c_char;
    fn eis_client_connect(client: *mut EisClient);
    fn eis_client_disconnect(client: *mut EisClient);
    fn eis_client_new_seat(client: *mut EisClient, name: *const c_char) -> *mut EisSeat;
    fn eis_seat_unref(seat: *mut EisSeat) -> *mut EisSeat;
    fn eis_seat_configure_capability(seat: *mut EisSeat, cap: u32);
    fn eis_seat_add(seat: *mut EisSeat);
    fn eis_seat_new_device(seat: *mut EisSeat) -> *mut EisDevice;
    fn eis_device_configure_type(device: *mut EisDevice, type_: u32);
    fn eis_device_configure_name(device: *mut EisDevice, name: *const c_char);
    fn eis_device_configure_capability(device: *mut EisDevice, cap: u32);
    fn eis_device_new_region(device: *mut EisDevice) -> *mut EisRegion;
    fn eis_region_set_size(region: *mut EisRegion, width: u32, height: u32);
    fn eis_region_set_offset(region: *mut EisRegion, x: u32, y: u32);
    fn eis_region_add(region: *mut EisRegion);
    fn eis_region_unref(region: *mut EisRegion) -> *mut EisRegion;
    fn eis_device_new_keymap(
        device: *mut EisDevice,
        type_: u32,
        fd: RawFd,
        size: usize,
    ) -> *mut EisKeymap;
    fn eis_keymap_add(keymap: *mut EisKeymap);
    fn eis_keymap_unref(keymap: *mut EisKeymap) -> *mut EisKeymap;
    fn eis_device_add(device: *mut EisDevice);
    fn eis_device_unref(device: *mut EisDevice) -> *mut EisDevice;
    fn eis_device_resume(device: *mut EisDevice);
}

fn io_from_errno(err: Errno) -> io::Error {
    io::Error::from(err)
}

pub struct Context {
    raw: *mut Eis,
}

// libeis contexts are confined to the input thread.
unsafe impl Send for Context {}

impl Context {
    pub fn new() -> io::Result<Self> {
        // SAFETY: eis_new returns a new refcounted context or null.
        let raw = unsafe { eis_new(std::ptr::null_mut()) };
        if raw.is_null() {
            return Err(io::Error::other("eis_new failed"));
        }
        // SAFETY: `raw` is a live context and the flag is set before the backend.
        let flag = unsafe { eis_set_flag(raw, EIS_FLAG_DEVICE_READY) };
        if flag < 0 {
            unsafe { eis_unref(raw) };
            return Err(io_from_errno(Errno::from_raw_os_error(-flag)));
        }
        // SAFETY: `raw` is a live context.
        unsafe { eis_log_set_priority(raw, EIS_LOG_PRIORITY_WARNING) };
        // SAFETY: `raw` is a live context and the fd backend is the one used by portals.
        let setup = unsafe { eis_setup_backend_fd(raw) };
        if setup < 0 {
            unsafe { eis_unref(raw) };
            return Err(io_from_errno(Errno::from_raw_os_error(-setup)));
        }
        Ok(Self { raw })
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        // SAFETY: `raw` is a live context. The returned descriptor is owned by libeis.
        let raw_fd = unsafe { eis_get_fd(self.raw) };
        // SAFETY: libeis keeps this descriptor open for the life of the context.
        unsafe { BorrowedFd::borrow_raw(raw_fd) }
    }

    /// Hand a connected client socket to the portal caller.
    pub fn add_client(&self) -> io::Result<OwnedFd> {
        // SAFETY: `raw` was set up with the fd backend.
        let fd = unsafe { eis_backend_fd_add_client(self.raw) };
        if fd < 0 {
            return Err(io_from_errno(Errno::from_raw_os_error(-fd)));
        }
        // SAFETY: libeis transferred this descriptor to the caller.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    pub fn dispatch(&self) {
        // SAFETY: `raw` is a live context and this runs on the input thread.
        unsafe { eis_dispatch(self.raw) };
    }

    pub fn next_event(&self) -> Option<Event> {
        // SAFETY: `raw` is a live context. A non-null event is owned by the caller.
        let raw = unsafe { eis_get_event(self.raw) };
        if raw.is_null() {
            None
        } else {
            Some(Event { raw })
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: `raw` is a live context and this is the last reference we hold.
        unsafe { eis_unref(self.raw) };
    }
}

pub struct Event {
    raw: *mut EisEvent,
}

impl Event {
    pub fn kind(&self) -> u32 {
        // SAFETY: `raw` is a live event.
        unsafe { eis_event_get_type(self.raw) }
    }

    pub fn client_name(&self) -> String {
        // SAFETY: the client pointer is valid for the duration of this event.
        let client = unsafe { eis_event_get_client(self.raw) };
        if client.is_null() {
            return String::new();
        }
        // SAFETY: libeis returns a NUL-terminated name or null.
        let name = unsafe { eis_client_get_name(client) };
        if name.is_null() {
            return String::new();
        }
        // SAFETY: `name` is a C string owned by libeis and remains valid during this call.
        unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned()
    }

    pub fn client_is_sender(&self) -> bool {
        // SAFETY: the client pointer is valid for the duration of this event.
        let client = unsafe { eis_event_get_client(self.raw) };
        !client.is_null() && unsafe { eis_client_is_sender(client) }
    }

    pub fn accept_sender(&self) {
        // SAFETY: the client pointer is valid for the duration of this event.
        let client = unsafe { eis_event_get_client(self.raw) };
        if !client.is_null() {
            unsafe { eis_client_connect(client) };
        }
    }

    pub fn reject_client(&self) {
        // SAFETY: the client pointer is valid for the duration of this event.
        let client = unsafe { eis_event_get_client(self.raw) };
        if !client.is_null() {
            unsafe { eis_client_disconnect(client) };
        }
    }

    /// Advertise one seat that can host the devices KDE Connect binds.
    pub fn add_input_seat(&self) {
        // SAFETY: the client pointer is valid for the duration of this event.
        let client = unsafe { eis_event_get_client(self.raw) };
        if client.is_null() {
            return;
        }
        // SAFETY: `client` was just accepted. The seat reference is released below.
        let seat = unsafe { eis_client_new_seat(client, c"default".as_ptr()) };
        if seat.is_null() {
            return;
        }
        for cap in [
            CAP_POINTER,
            CAP_POINTER_ABSOLUTE,
            CAP_BUTTON,
            CAP_SCROLL,
            CAP_KEYBOARD,
        ] {
            unsafe { eis_seat_configure_capability(seat, cap) };
        }
        unsafe {
            eis_seat_add(seat);
            eis_seat_unref(seat);
        }
    }

    pub fn has_capability(&self, cap: u32) -> bool {
        // SAFETY: capability queries are valid for seat-bind events.
        unsafe { eis_event_seat_has_capability(self.raw, cap) }
    }

    /// Publish a virtual device for the capabilities the client bound.
    ///
    /// `keymap_fd` is the XKB keymap used when the keyboard capability is bound.
    /// Its size includes the trailing NUL. libeis copies what it needs before
    /// this function returns.
    pub fn publish_device(&self, width: u32, height: u32, keymap_fd: RawFd, keymap_size: usize) {
        // SAFETY: the seat pointer is valid for the duration of a seat-bind event.
        let seat = unsafe { eis_event_get_seat(self.raw) };
        if seat.is_null() {
            return;
        }
        // SAFETY: `seat` belongs to an accepted client. The device ref is released after add.
        let device = unsafe { eis_seat_new_device(seat) };
        if device.is_null() {
            return;
        }
        unsafe {
            eis_device_configure_type(device, DEVICE_TYPE_VIRTUAL);
            eis_device_configure_name(device, c"virtinput-portal".as_ptr());
        }
        let caps = [
            CAP_POINTER,
            CAP_POINTER_ABSOLUTE,
            CAP_BUTTON,
            CAP_SCROLL,
            CAP_KEYBOARD,
        ];
        for cap in caps {
            if self.has_capability(cap) {
                unsafe { eis_device_configure_capability(device, cap) };
            }
        }
        if self.has_capability(CAP_POINTER_ABSOLUTE) {
            // SAFETY: `device` is a virtual device that has not been added yet.
            let region = unsafe { eis_device_new_region(device) };
            if !region.is_null() {
                unsafe {
                    eis_region_set_offset(region, 0, 0);
                    eis_region_set_size(region, width.max(1), height.max(1));
                    eis_region_add(region);
                    eis_region_unref(region);
                }
            }
        }
        if self.has_capability(CAP_KEYBOARD) && keymap_size > 0 {
            // SAFETY: `keymap_fd` is a sealed memfd of `keymap_size` bytes. libeis
            // maps it privately and does not keep the descriptor.
            let keymap =
                unsafe { eis_device_new_keymap(device, KEYMAP_TYPE_XKB, keymap_fd, keymap_size) };
            if !keymap.is_null() {
                unsafe {
                    eis_keymap_add(keymap);
                    eis_keymap_unref(keymap);
                }
            }
        }
        unsafe {
            eis_device_add(device);
            eis_device_unref(device);
        }
    }

    pub fn resume_device(&self) {
        // SAFETY: the device pointer is valid for a device-ready event.
        let device = unsafe { eis_event_get_device(self.raw) };
        if !device.is_null() {
            unsafe { eis_device_resume(device) };
        }
    }

    pub fn pointer_delta(&self) -> (f64, f64) {
        // SAFETY: valid for pointer-motion events.
        unsafe {
            (
                eis_event_pointer_get_dx(self.raw),
                eis_event_pointer_get_dy(self.raw),
            )
        }
    }

    pub fn pointer_absolute(&self) -> (f64, f64) {
        // SAFETY: valid for absolute pointer-motion events.
        unsafe {
            (
                eis_event_pointer_get_absolute_x(self.raw),
                eis_event_pointer_get_absolute_y(self.raw),
            )
        }
    }

    pub fn button(&self) -> (u32, bool) {
        // SAFETY: valid for button events.
        unsafe {
            (
                eis_event_button_get_button(self.raw),
                eis_event_button_get_is_press(self.raw),
            )
        }
    }

    pub fn scroll_delta(&self) -> (f64, f64) {
        // SAFETY: valid for scroll-delta events.
        unsafe {
            (
                eis_event_scroll_get_dx(self.raw),
                eis_event_scroll_get_dy(self.raw),
            )
        }
    }

    pub fn scroll_stop(&self) -> (bool, bool) {
        // SAFETY: valid for scroll-stop and scroll-cancel events.
        unsafe {
            (
                eis_event_scroll_get_stop_x(self.raw),
                eis_event_scroll_get_stop_y(self.raw),
            )
        }
    }

    pub fn scroll_discrete(&self) -> (i32, i32) {
        // SAFETY: valid for discrete scroll events. Values are in multiples of 120.
        unsafe {
            (
                eis_event_scroll_get_discrete_dx(self.raw),
                eis_event_scroll_get_discrete_dy(self.raw),
            )
        }
    }

    pub fn key(&self) -> (u32, bool) {
        // SAFETY: valid for keyboard events. The key is an evdev code.
        unsafe {
            (
                eis_event_keyboard_get_key(self.raw),
                eis_event_keyboard_get_key_is_press(self.raw),
            )
        }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: `raw` is a live event owned by this wrapper.
        unsafe { eis_event_unref(self.raw) };
    }
}
