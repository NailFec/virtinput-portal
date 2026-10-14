//! Confirm the backend can move the pointer.
//!
//! A transparent layer-shell surface covers the output and listens for pointer
//! events. The backend is then asked to move the pointer onto that surface.

use std::collections::HashMap;
use std::io;
use std::os::fd::AsFd;
use std::ptr::null_mut;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, poll};
use rustix::fs::{MemfdFlags, memfd_create};
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};
use rustix::time::Timespec;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};
use zbus::zvariant::{ObjectPath, OwnedValue};

struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    output: Option<wl_output::WlOutput>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    surface: Option<wl_surface::WlSurface>,
    layer: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    pointer: Option<wl_pointer::WlPointer>,
    configured: Option<(i32, i32)>,
    mapped: bool,
    saw_pointer: bool,
    map: Option<(*mut std::ffi::c_void, usize)>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
    let connection =
        Connection::connect_to_env().map_err(|err| io::Error::other(err.to_string()))?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let _registry = connection.display().get_registry(&qh, ());
    let mut state = State {
        compositor: None,
        shm: None,
        seat: None,
        output: None,
        layer_shell: None,
        surface: None,
        layer: None,
        pointer: None,
        configured: None,
        mapped: false,
        saw_pointer: false,
        map: None,
    };
    queue.roundtrip(&mut state).map_err(wayland_io)?;

    let compositor = state
        .compositor
        .take()
        .ok_or_else(|| io::Error::other("missing wl_compositor"))?;
    let shm = state
        .shm
        .take()
        .ok_or_else(|| io::Error::other("missing wl_shm"))?;
    let shell = state
        .layer_shell
        .take()
        .ok_or_else(|| io::Error::other("missing zwlr_layer_shell_v1"))?;
    let seat = state
        .seat
        .as_ref()
        .ok_or_else(|| io::Error::other("missing wl_seat"))?;
    state.pointer = Some(seat.get_pointer(&qh, ()));
    let surface = compositor.create_surface(&qh, ());
    let output = state.output.as_ref();
    let layer = shell.get_layer_surface(
        &surface,
        output,
        zwlr_layer_shell_v1::Layer::Overlay,
        "virtinput-smoke".to_string(),
        &qh,
        (),
    );
    let anchor = zwlr_layer_surface_v1::Anchor::Top
        | zwlr_layer_surface_v1::Anchor::Bottom
        | zwlr_layer_surface_v1::Anchor::Left
        | zwlr_layer_surface_v1::Anchor::Right;
    layer.set_anchor(anchor);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
    surface.commit();
    state.surface = Some(surface);
    state.layer = Some(layer);
    state.compositor = Some(compositor);
    state.shm = Some(shm);
    state.layer_shell = Some(shell);

    let deadline = Instant::now() + Duration::from_secs(2);
    while state.configured.is_none() && Instant::now() < deadline {
        dispatch_for(&mut queue, &mut state, Duration::from_millis(100))?;
    }
    let (width, height) = state
        .configured
        .ok_or_else(|| io::Error::other("layer surface was not configured"))?;
    let width = if width > 0 { width } else { 64 };
    let height = if height > 0 { height } else { 64 };
    let shm = state.shm.as_ref().unwrap();
    let surface = state.surface.as_ref().unwrap();
    let (buffer, map) = create_buffer(shm, &qh, width, height)?;
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, width, height);
    surface.commit();
    state.map = Some(map);
    state.mapped = true;
    dispatch_for(&mut queue, &mut state, Duration::from_millis(150))?;

    move_pointer().await?;

    let deadline = Instant::now() + Duration::from_secs(2);
    while !state.saw_pointer && Instant::now() < deadline {
        dispatch_for(&mut queue, &mut state, Duration::from_millis(100))?;
    }

    if let Some((ptr, len)) = state.map.take() {
        // SAFETY: this mapping was created by `create_buffer` and is no longer used.
        unsafe { munmap(ptr, len) }.ok();
    }
    if !state.saw_pointer {
        return Err(io::Error::other("pointer did not enter the test surface"));
    }
    println!("pointer event received");
    Ok(())
}

async fn move_pointer() -> io::Result<()> {
    let connection = zbus::connection::Builder::session()
        .map_err(io::Error::other)?
        .build()
        .await
        .map_err(io::Error::other)?;
    let session = ObjectPath::from_static_str("/org/freedesktop/portal/session/virtinput_smoke")
        .map_err(|err| io::Error::other(err.to_string()))?;
    let empty = HashMap::<String, OwnedValue>::new();
    call(
        &connection,
        "CreateSession",
        &(
            ObjectPath::from_static_str("/org/freedesktop/portal/request/virtinput_smoke1")
                .map_err(|err| io::Error::other(err.to_string()))?,
            &session,
            "virtinput.smoke",
            &empty,
        ),
    )
    .await?;
    call(
        &connection,
        "SelectDevices",
        &(
            ObjectPath::from_static_str("/org/freedesktop/portal/request/virtinput_smoke2")
                .map_err(|err| io::Error::other(err.to_string()))?,
            &session,
            "virtinput.smoke",
            &empty,
        ),
    )
    .await?;
    call(
        &connection,
        "Start",
        &(
            ObjectPath::from_static_str("/org/freedesktop/portal/request/virtinput_smoke3")
                .map_err(|err| io::Error::other(err.to_string()))?,
            &session,
            "virtinput.smoke",
            "",
            &empty,
        ),
    )
    .await?;
    connection
        .call_method(
            Some("org.freedesktop.impl.portal.desktop.virtinput"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.impl.portal.RemoteDesktop"),
            "NotifyPointerMotionAbsolute",
            &(&session, &empty, 0u32, 120.0_f64, 80.0_f64),
        )
        .await
        .map_err(io::Error::other)?;
    connection
        .call_method(
            Some("org.freedesktop.impl.portal.desktop.virtinput"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.impl.portal.RemoteDesktop"),
            "NotifyPointerMotion",
            &(&session, &empty, 40.0_f64, 20.0_f64),
        )
        .await
        .map_err(io::Error::other)?;
    Ok(())
}

async fn call(
    connection: &zbus::Connection,
    method: &str,
    body: &(impl zbus::export::serde::Serialize + zbus::zvariant::DynamicType),
) -> io::Result<()> {
    let reply = connection
        .call_method(
            Some("org.freedesktop.impl.portal.desktop.virtinput"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.impl.portal.RemoteDesktop"),
            method,
            body,
        )
        .await
        .map_err(io::Error::other)?;
    let (response, _results): (u32, HashMap<String, OwnedValue>) = reply
        .body()
        .deserialize()
        .map_err(|err| io::Error::other(err.to_string()))?;
    if response != 0 {
        return Err(io::Error::other(format!(
            "{method} failed with response {response}"
        )));
    }
    Ok(())
}

fn create_buffer(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<State>,
    width: i32,
    height: i32,
) -> io::Result<(wl_buffer::WlBuffer, (*mut std::ffi::c_void, usize))> {
    let stride = width * 4;
    let size = stride * height;
    if size <= 0 {
        return Err(io::Error::other("invalid buffer size"));
    }
    let len = size as usize;
    let fd = memfd_create(c"virtinput-smoke", MemfdFlags::CLOEXEC)?;
    rustix::fs::ftruncate(&fd, len as u64)?;
    // SAFETY: the memfd is truncated to `len` and is mapped privately for this process.
    let map = unsafe {
        mmap(
            null_mut(),
            len,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::SHARED,
            &fd,
            0,
        )?
    };
    let pool = shm.create_pool(fd.as_fd(), size, qh, ());
    let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, qh, ());
    Ok((buffer, (map, len)))
}

fn dispatch_for(
    queue: &mut EventQueue<State>,
    state: &mut State,
    budget: Duration,
) -> io::Result<()> {
    queue.flush().map_err(wayland_io)?;
    queue.dispatch_pending(state).map_err(wayland_io)?;
    let Some(guard) = queue.prepare_read() else {
        queue.dispatch_pending(state).map_err(wayland_io)?;
        return Ok(());
    };
    let fd = guard.connection_fd();
    let mut polls = [PollFd::new(&fd, PollFlags::IN)];
    let timeout = Timespec {
        tv_sec: budget.as_secs() as i64,
        tv_nsec: budget.subsec_nanos() as i64,
    };
    match poll(&mut polls, Some(&timeout)) {
        Ok(0) => drop(guard),
        Ok(_) => {
            guard.read().ok();
        }
        Err(rustix::io::Errno::INTR) => drop(guard),
        Err(err) => return Err(io::Error::from(err)),
    }
    queue.dispatch_pending(state).map_err(wayland_io)?;
    Ok(())
}

fn wayland_io(err: impl std::fmt::Display) -> io::Error {
    io::Error::other(err.to_string())
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
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
            "wl_compositor" => state.compositor = Some(registry.bind(name, version.min(4), qh, ())),
            "wl_shm" => state.shm = Some(registry.bind(name, version.min(1), qh, ())),
            "wl_seat" => state.seat = Some(registry.bind(name, version.min(5), qh, ())),
            "wl_output" if state.output.is_none() => {
                state.output = Some(registry.bind(name, version.min(3), qh, ()));
            }
            "zwlr_layer_shell_v1" => {
                state.layer_shell = Some(registry.bind(name, version.min(1), qh, ()))
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_compositor::WlCompositor,
        _: wl_compositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_shm::WlShm, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_shm::WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_shm_pool::WlShmPool, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_output::WlOutput, ()> for State {
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
impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        pointer: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { serial, .. } => {
                pointer.set_cursor(serial, None, 0, 0);
                state.saw_pointer = true;
            }
            wl_pointer::Event::Motion { .. } | wl_pointer::Event::Button { .. } => {
                state.saw_pointer = true
            }
            _ => {}
        }
    }
}
impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        _: zwlr_layer_shell_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            layer.ack_configure(serial);
            state.configured = Some((width as i32, height as i32));
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some((ptr, len)) = self.map.take() {
            // SAFETY: the mapping belongs to this state and is dropped once.
            unsafe { munmap(ptr, len) }.ok();
        }
    }
}
