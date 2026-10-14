//! Wayland virtual pointer and keyboard, plus the libeis poll loop.
//!
//! Niri accepts emulated input through `zwlr_virtual_pointer_v1` and
//! `zwp_virtual_keyboard_v1`. This thread owns both the Wayland connection and
//! the libeis context, and applies portal and libei events on that thread.

mod virtual_keyboard {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocol/virtual-keyboard-unstable-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocol/virtual-keyboard-unstable-v1.xml");
}

use std::io::{self, ErrorKind};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use rustix::event::{PollFd, PollFlags, poll};
use rustix::io::Errno;
use rustix::pipe::{PipeFlags, pipe_with};
use rustix::time::{ClockId, clock_gettime};
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1,
};

use crate::eis::{self, Context as EisContext};
use crate::keymap::{self, Layout};

use virtual_keyboard::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use virtual_keyboard::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

pub enum Boot {
    Ready(Commander),
    Failed(String),
}

pub enum Command {
    Motion {
        dx: f64,
        dy: f64,
    },
    MotionAbsolute {
        x: f64,
        y: f64,
    },
    Button {
        button: u32,
        pressed: bool,
    },
    /// Continuous scroll. Positive Y moves downward, matching `wl_pointer`.
    Scroll {
        dx: f64,
        dy: f64,
    },
    /// Discrete wheel steps. Axis 0 is vertical and axis 1 is horizontal.
    ScrollDiscrete {
        axis: u32,
        steps: i32,
    },
    Key {
        keycode: u32,
        pressed: bool,
    },
    Keysym {
        keysym: u32,
        pressed: bool,
    },
    AddEisClient {
        reply: Sender<io::Result<OwnedFd>>,
    },
}

#[derive(Clone)]
pub struct Commander {
    tx: Sender<Command>,
    wake: Arc<OwnedFd>,
}

impl Commander {
    pub fn send(&self, command: Command) -> io::Result<()> {
        self.tx
            .send(command)
            .map_err(|_| io::Error::other("input thread stopped"))?;
        // A one-byte write wakes the poll. The bytes themselves are discarded.
        match rustix::io::write(&*self.wake, &[1]) {
            Ok(_) | Err(Errno::AGAIN) | Err(Errno::INTR) => Ok(()),
            Err(err) => Err(io::Error::from(err)),
        }
    }
}

struct App {
    // Held so the compositor keeps the globals we bound.
    #[allow(dead_code)]
    registry: Option<wl_registry::WlRegistry>,
    qh: QueueHandle<App>,
    eis: EisContext,
    rx: Receiver<Command>,
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    output: Option<wl_output::WlOutput>,
    xdg_output_manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    xdg_output: Option<zxdg_output_v1::ZxdgOutputV1>,
    pointer_manager: Option<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1>,
    keyboard_manager: Option<ZwpVirtualKeyboardManagerV1>,
    pointer: Option<zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1>,
    virtual_keyboard: Option<ZwpVirtualKeyboardV1>,
    layout: Layout,
    logical_size: (u32, u32),
    eis_device_published: bool,
}

pub fn run(boot: Sender<Boot>) -> io::Result<()> {
    let connection = connect_wayland()?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let registry = connection.display().get_registry(&qh, ());
    let (read_wake, write_wake) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)?;
    let (tx, rx) = mpsc::channel();
    let commander = Commander {
        tx,
        wake: Arc::new(write_wake),
    };

    let mut app = App {
        registry: Some(registry),
        qh,
        eis: EisContext::new()?,
        rx,
        seat: None,
        keyboard: None,
        output: None,
        xdg_output_manager: None,
        xdg_output: None,
        pointer_manager: None,
        keyboard_manager: None,
        pointer: None,
        virtual_keyboard: None,
        layout: Layout::fallback()?,
        logical_size: (0, 0),
        eis_device_published: false,
    };

    // Globals arrive on the first roundtrip. Seat capabilities, the keymap,
    // and the logical output size arrive once those objects exist, so pump
    // until the virtual devices can be created.
    for _ in 0..8 {
        roundtrip(&mut queue, &mut app)?;
        app.attach_xdg_output();
        if app.pointer.is_some() && app.virtual_keyboard.is_some() && app.logical_size.0 > 0 {
            break;
        }
        if app.seat.is_some() && app.pointer_manager.is_some() && app.keyboard_manager.is_some() {
            app.create_virtual_devices()?;
        }
    }
    app.create_virtual_devices()?;
    roundtrip(&mut queue, &mut app)?;

    if app.pointer.is_none() || app.virtual_keyboard.is_none() {
        let message = "compositor is missing zwlr_virtual_pointer_manager_v1 or zwp_virtual_keyboard_manager_v1";
        let _ = boot.send(Boot::Failed(message.to_string()));
        return Err(io::Error::other(message));
    }

    let (width, height) = app.extent();
    log::info!("virtual input ready on a {width}x{height} output");
    let _ = boot.send(Boot::Ready(commander));

    loop {
        queue.flush().map_err(wayland_io)?;
        dispatch_pending(&mut queue, &mut app)?;

        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let wayland_fd = guard.connection_fd();
        let eis_fd = app.eis.fd();
        let mut polls = [
            PollFd::new(&wayland_fd, PollFlags::IN | PollFlags::ERR | PollFlags::HUP),
            PollFd::new(&eis_fd, PollFlags::IN | PollFlags::ERR | PollFlags::HUP),
            PollFd::new(&read_wake, PollFlags::IN),
        ];
        let polled = match poll(&mut polls, None) {
            Ok(count) => count,
            Err(Errno::INTR) => {
                drop(guard);
                continue;
            }
            Err(err) => return Err(io::Error::from(err)),
        };
        if polled == 0 {
            drop(guard);
            continue;
        }
        let wayland_ready = polls[0]
            .revents()
            .intersects(PollFlags::IN | PollFlags::ERR | PollFlags::HUP);
        let eis_ready = polls[1]
            .revents()
            .intersects(PollFlags::IN | PollFlags::ERR | PollFlags::HUP);
        let wake_ready = polls[2].revents().intersects(PollFlags::IN);

        if wayland_ready {
            if let Err(err) = guard.read() {
                if !is_would_block(&err) {
                    return Err(wayland_io(err));
                }
            }
        } else {
            drop(guard);
        }
        dispatch_pending(&mut queue, &mut app)?;
        if eis_ready {
            app.drain_eis()?;
            queue.flush().map_err(wayland_io)?;
        }
        if wake_ready {
            drain_wake(&read_wake)?;
            app.drain_commands()?;
            queue.flush().map_err(wayland_io)?;
        }
    }
}

fn connect_wayland() -> io::Result<Connection> {
    if let Ok(connection) = Connection::connect_to_env() {
        return Ok(connection);
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").map_err(|_| {
        io::Error::new(
            ErrorKind::NotFound,
            "WAYLAND_DISPLAY and XDG_RUNTIME_DIR are unset",
        )
    })?;
    let mut sockets = Vec::new();
    for entry in std::fs::read_dir(&runtime)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("wayland-") && !name.contains('.') {
            sockets.push(name.into_owned());
        }
    }
    sockets.sort();
    sockets.dedup();
    for name in sockets.iter().rev() {
        // SAFETY: this runs on the input thread before any other thread reads
        // the environment. The process is single-threaded until `main` continues.
        unsafe { std::env::set_var("WAYLAND_DISPLAY", name) };
        if let Ok(connection) = Connection::connect_to_env() {
            log::info!("using Wayland display {name}");
            return Ok(connection);
        }
    }
    Err(io::Error::other("could not connect to a Wayland display"))
}

fn roundtrip(queue: &mut EventQueue<App>, app: &mut App) -> io::Result<()> {
    queue.roundtrip(app).map(|_| ()).map_err(wayland_io)
}

fn dispatch_pending(queue: &mut EventQueue<App>, app: &mut App) -> io::Result<()> {
    queue.dispatch_pending(app).map(|_| ()).map_err(wayland_io)
}

fn wayland_io(err: impl std::fmt::Display) -> io::Error {
    io::Error::other(err.to_string())
}

fn is_would_block(err: &impl std::fmt::Display) -> bool {
    err.to_string().contains("WouldBlock")
        || err.to_string().contains("Resource temporarily unavailable")
}

fn drain_wake(wake: &OwnedFd) -> io::Result<()> {
    let mut buf = [0u8; 64];
    loop {
        match rustix::io::read(wake, &mut buf) {
            Ok(0) => return Ok(()),
            Ok(_) => continue,
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN) => return Ok(()),
            Err(err) => return Err(io::Error::from(err)),
        }
    }
}

fn now_ms() -> u32 {
    let time = clock_gettime(ClockId::Monotonic);
    (time.tv_sec as u64)
        .saturating_mul(1000)
        .saturating_add((time.tv_nsec as u64) / 1_000_000) as u32
}

impl App {
    fn attach_xdg_output(&mut self) {
        if self.xdg_output.is_some() {
            return;
        }
        let (Some(manager), Some(output)) = (&self.xdg_output_manager, &self.output) else {
            return;
        };
        self.xdg_output = Some(manager.get_xdg_output(output, &self.qh, ()));
    }

    fn create_virtual_devices(&mut self) -> io::Result<()> {
        let (Some(seat), Some(pointer_manager), Some(keyboard_manager)) =
            (&self.seat, &self.pointer_manager, &self.keyboard_manager)
        else {
            return Ok(());
        };
        if self.pointer.is_none() {
            self.pointer = Some(pointer_manager.create_virtual_pointer(Some(seat), &self.qh, ()));
        }
        if self.virtual_keyboard.is_none() {
            let keyboard = keyboard_manager.create_virtual_keyboard(seat, &self.qh, ());
            self.apply_keymap(&keyboard)?;
            self.virtual_keyboard = Some(keyboard);
        }
        Ok(())
    }

    fn apply_keymap(&self, keyboard: &ZwpVirtualKeyboardV1) -> io::Result<()> {
        let (fd, size) = keymap::sealed_keymap(&self.layout.text)?;
        // wl_keyboard.keymap_format.xkb_v1 == 1. The descriptor must stay open for this call.
        keyboard.keymap(1, fd.as_fd(), size as u32);
        Ok(())
    }

    fn extent(&self) -> (u32, u32) {
        if self.logical_size.0 == 0 || self.logical_size.1 == 0 {
            (1920, 1080)
        } else {
            self.logical_size
        }
    }

    fn adopt_layout(&mut self, layout: Layout) {
        self.layout = layout;
        if let Some(keyboard) = &self.virtual_keyboard {
            if let Err(err) = self.apply_keymap(keyboard) {
                log::warn!("failed to update the virtual keyboard keymap: {err}");
            }
        }
    }

    fn drain_commands(&mut self) -> io::Result<()> {
        loop {
            match self.rx.try_recv() {
                Ok(command) => self.handle_command(command)?,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::other("portal dropped the input channel"));
                }
            }
        }
    }

    fn handle_command(&mut self, command: Command) -> io::Result<()> {
        match command {
            Command::Motion { dx, dy } => self.motion(dx, dy),
            Command::MotionAbsolute { x, y } => self.motion_absolute(x, y),
            Command::Button { button, pressed } => self.button(button, pressed),
            Command::Scroll { dx, dy } => self.scroll(dx, dy),
            Command::ScrollDiscrete { axis, steps } => self.scroll_discrete(axis, steps),
            Command::Key { keycode, pressed } => self.key(keycode, pressed),
            Command::Keysym { keysym, pressed } => match self.layout.evdev_from_keysym(keysym) {
                Some(keycode) => self.key(keycode, pressed),
                None => log::warn!("no evdev keycode for keysym {keysym:#x}"),
            },
            Command::AddEisClient { reply } => {
                let _ = reply.send(self.eis.add_client());
            }
        }
        Ok(())
    }

    fn motion(&mut self, dx: f64, dy: f64) {
        let Some(pointer) = &self.pointer else { return };
        pointer.motion(now_ms(), dx, dy);
        pointer.frame();
        log::debug!("pointer motion {dx},{dy}");
    }

    fn motion_absolute(&mut self, x: f64, y: f64) {
        let Some(pointer) = &self.pointer else { return };
        let (width, height) = self.extent();
        let x = x.clamp(0.0, width.saturating_sub(1) as f64) as u32;
        let y = y.clamp(0.0, height.saturating_sub(1) as f64) as u32;
        pointer.motion_absolute(now_ms(), x, y, width, height);
        pointer.frame();
        log::debug!("pointer absolute {x},{y} on {width}x{height}");
    }

    fn button(&mut self, button: u32, pressed: bool) {
        let Some(pointer) = &self.pointer else { return };
        let state = if pressed {
            wl_pointer::ButtonState::Pressed
        } else {
            wl_pointer::ButtonState::Released
        };
        pointer.button(now_ms(), button, state.into());
        pointer.frame();
    }

    fn scroll(&mut self, dx: f64, dy: f64) {
        let Some(pointer) = &self.pointer else { return };
        let time = now_ms();
        pointer.axis_source(wl_pointer::AxisSource::Finger.into());
        if dx != 0.0 {
            pointer.axis(time, wl_pointer::Axis::HorizontalScroll.into(), dx);
        }
        if dy != 0.0 {
            pointer.axis(time, wl_pointer::Axis::VerticalScroll.into(), dy);
        }
        pointer.frame();
    }

    fn scroll_discrete(&mut self, axis: u32, steps: i32) {
        let Some(pointer) = &self.pointer else { return };
        let wl_axis = if axis == 1 {
            wl_pointer::Axis::HorizontalScroll
        } else {
            wl_pointer::Axis::VerticalScroll
        };
        // Fifteen pixels is libinput's usual click distance. The discrete count
        // tells the client how many wheel detents this represents.
        let pixels = f64::from(steps) * 15.0;
        let time = now_ms();
        pointer.axis_source(wl_pointer::AxisSource::Wheel.into());
        pointer.axis_discrete(time, wl_axis.into(), pixels, steps);
        pointer.frame();
    }

    fn key(&mut self, keycode: u32, pressed: bool) {
        let Some(keyboard) = &self.virtual_keyboard else {
            return;
        };
        let mods = self.layout.update_evdev(keycode, pressed);
        keyboard.modifiers(mods.depressed, mods.latched, mods.locked, mods.group);
        let state = if pressed { 1 } else { 0 };
        keyboard.key(now_ms(), keycode, state);
        log::debug!("key {keycode} pressed={pressed} mods={mods:?}");
    }

    fn scroll_stop(&mut self, x: bool, y: bool) {
        let Some(pointer) = &self.pointer else { return };
        let time = now_ms();
        if x {
            pointer.axis_stop(time, wl_pointer::Axis::HorizontalScroll.into());
        }
        if y {
            pointer.axis_stop(time, wl_pointer::Axis::VerticalScroll.into());
        }
        pointer.frame();
    }

    fn drain_eis(&mut self) -> io::Result<()> {
        self.eis.dispatch();
        while let Some(event) = self.eis.next_event() {
            self.handle_eis(&event)?;
        }
        Ok(())
    }

    fn handle_eis(&mut self, event: &eis::Event) -> io::Result<()> {
        match event.kind() {
            eis::EVENT_CLIENT_CONNECT => {
                let name = event.client_name();
                if event.client_is_sender() {
                    log::info!("libei sender connected: {name}");
                    event.accept_sender();
                    event.add_input_seat();
                } else {
                    log::warn!("rejecting libei receiver client {name}");
                    event.reject_client();
                }
            }
            eis::EVENT_CLIENT_DISCONNECT => {
                log::info!("libei client disconnected");
                self.eis_device_published = false;
            }
            eis::EVENT_SEAT_BIND if !self.eis_device_published => {
                let (width, height) = self.extent();
                let (fd, size) = keymap::sealed_keymap(&self.layout.text)?;
                event.publish_device(width, height, fd.as_raw_fd(), size);
                self.eis_device_published = true;
                log::info!("published libei device for a {width}x{height} region");
            }
            eis::EVENT_DEVICE_READY => event.resume_device(),
            eis::EVENT_POINTER_MOTION => {
                let (dx, dy) = event.pointer_delta();
                self.motion(dx, dy);
            }
            eis::EVENT_POINTER_MOTION_ABSOLUTE => {
                let (x, y) = event.pointer_absolute();
                self.motion_absolute(x, y);
            }
            eis::EVENT_BUTTON => {
                let (button, pressed) = event.button();
                self.button(button, pressed);
            }
            eis::EVENT_SCROLL_DELTA => {
                let (dx, dy) = event.scroll_delta();
                self.scroll(dx, dy);
            }
            eis::EVENT_SCROLL_DISCRETE => {
                let (dx, dy) = event.scroll_discrete();
                self.scroll_v120(dx, dy);
            }
            eis::EVENT_SCROLL_STOP | eis::EVENT_SCROLL_CANCEL => {
                let (x, y) = event.scroll_stop();
                self.scroll_stop(x, y);
            }
            eis::EVENT_KEYBOARD_KEY => {
                let (keycode, pressed) = event.key();
                self.key(keycode, pressed);
            }
            eis::EVENT_FRAME
            | eis::EVENT_DEVICE_CLOSED
            | eis::EVENT_DEVICE_START_EMULATING
            | eis::EVENT_DEVICE_STOP_EMULATING => {}
            other => log::debug!("ignoring libei event {other}"),
        }
        Ok(())
    }

    /// libei discrete scroll is measured in fractions of 120 per detent.
    fn scroll_v120(&mut self, dx: i32, dy: i32) {
        if dy != 0 {
            let steps = div_detents(dy);
            if steps != 0 {
                self.scroll_discrete(0, steps);
            } else {
                self.scroll(0.0, f64::from(dy) / 120.0 * 15.0);
            }
        }
        if dx != 0 {
            let steps = div_detents(dx);
            if steps != 0 {
                self.scroll_discrete(1, steps);
            } else {
                self.scroll(f64::from(dx) / 120.0 * 15.0, 0.0);
            }
        }
    }
}

fn div_detents(value: i32) -> i32 {
    if value == 0 {
        0
    } else if value.abs() < 120 {
        value.signum()
    } else {
        value / 120
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_seat" => {
                state.seat = Some(registry.bind(name, version.min(8), qh, ()));
            }
            "wl_output" if state.output.is_none() => {
                state.output = Some(registry.bind(name, version.min(4), qh, ()));
            }
            "zxdg_output_manager_v1" => {
                state.xdg_output_manager = Some(registry.bind(name, version.min(3), qh, ()));
            }
            "zwlr_virtual_pointer_manager_v1" => {
                state.pointer_manager = Some(registry.bind(name, version.min(2), qh, ()));
            }
            "zwp_virtual_keyboard_manager_v1" => {
                state.keyboard_manager = Some(registry.bind(name, 1, qh, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            let has_keyboard = match capabilities {
                WEnum::Value(caps) => caps.contains(wl_seat::Capability::Keyboard),
                WEnum::Unknown(bits) => bits & 2 != 0,
            };
            if has_keyboard && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let wl_keyboard::Event::Keymap { format, fd, size } = event else {
            return;
        };
        let is_xkb = match format {
            WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) => true,
            WEnum::Value(_) => false,
            WEnum::Unknown(value) => value == 1,
        };
        if !is_xkb || size == 0 {
            return;
        }
        match unsafe { Layout::from_keymap_fd(fd, size as usize) } {
            Ok(layout) => {
                log::info!("using the compositor XKB keymap");
                state.adopt_layout(layout);
            }
            Err(err) => log::warn!("ignoring compositor keymap: {err}"),
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_output::WlOutput,
        _: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zxdg_output_manager_v1::ZxdgOutputManagerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &zxdg_output_manager_v1::ZxdgOutputManagerV1,
        _: zxdg_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, ()> for App {
    fn event(
        state: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_output_v1::Event::LogicalSize { width, height } = event {
            if width > 0 && height > 0 {
                state.logical_size = (width as u32, height as u32);
                log::info!("output logical size is {width}x{height}");
            }
        }
    }
}

impl Dispatch<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
        _: zwlr_virtual_pointer_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
        _: zwlr_virtual_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardManagerV1,
        event: <ZwpVirtualKeyboardManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let _ = event;
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardV1,
        event: <ZwpVirtualKeyboardV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let _ = event;
    }
}
