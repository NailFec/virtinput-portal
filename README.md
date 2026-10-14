# virtinput-portal

A user-level [xdg-desktop-portal](https://flatpak.github.io/xdg-desktop-portal/) backend for
`org.freedesktop.impl.portal.RemoteDesktop`. It injects pointer and keyboard
events through the Wayland protocols `zwlr_virtual_pointer_v1` and
`zwp_virtual_keyboard_v1`.

KDE Connect delivers remote mouse and keyboard input on Wayland by opening a
RemoteDesktop session and sending events over libei (`ConnectToEIS`). Niri does
not implement Mutter's remote-desktop backend, so the GNOME portal reports no
pointer or keyboard devices and KDE Connect cannot move the cursor. This
program fills that one portal interface. Screenshot, screencast, and the other
portals stay with their existing backends.

The drawing-tablet feature in KDE Connect is separate. It creates a kernel
`uinput` device and does not use this portal.

## Security

There is no permission dialog. Any client on the session bus that is routed to
this backend can inject pointer and keyboard input. Install it only for a
desktop session you trust. The process logs the application id when a session
starts.

## Requirements

- A compositor that exposes `zwlr_virtual_pointer_manager_v1` and
  `zwp_virtual_keyboard_manager_v1` (Niri 26.04 does)
- `xdg-desktop-portal`
- `libei` 1.6 (`libeis-1.0`)
- `libxkbcommon`
- Rust 1.85 or newer

## Build

```sh
cargo build --release
```

`cargo test` checks keysym translation and the portal restore-data signature.
`cargo run --release --bin smoke` asks the running backend to move the pointer
and waits until a layer-shell surface receives that motion. The smoke client
covers the output with a transparent surface for a couple of seconds.

## Install on Niri

Copy the release binary to `~/.local/bin/virtinput-portal`, then install:

- `data/virtinput.portal` to
  `~/.local/share/xdg-desktop-portal/portals/virtinput.portal`
- `data/virtinput-portal.service` to
  `~/.config/systemd/user/virtinput-portal.service`
- `data/org.freedesktop.impl.portal.desktop.virtinput.service` to
  `~/.local/share/dbus-1/services/`, with `@BIN@` replaced by the binary path
- `data/niri-portals.conf` to
  `~/.config/xdg-desktop-portal/niri-portals.conf`

That portal config keeps the GNOME and GTK defaults and sends only
RemoteDesktop to this backend. Import `WAYLAND_DISPLAY` and
`XDG_CURRENT_DESKTOP` into the systemd user manager, then start the service
and restart `xdg-desktop-portal`. Restart `kdeconnectd` so it drops any
RemoteDesktop session that failed before this backend existed.

`AvailableDeviceTypes` on `org.freedesktop.portal.Desktop` should then be `3`
(keyboard and pointer). Touch is not advertised.

## What the backend implements

Session setup follows portal interface version 2. `Start` grants keyboard and
pointer. When `persist_mode` is non-zero, `Start` returns a `(suv)`
`restore_data` record so xdg-desktop-portal can issue the `restore_token` KDE
Connect stores.

`ConnectToEIS` is the path KDE Connect 26.08 uses after `Start`. The backend
accepts sender clients, offers one seat, and on seat bind publishes a virtual
device with the capabilities the client bound. The absolute pointer region uses
the focused output's logical size. The keyboard keymap is the one received from
`wl_keyboard`, or a compiled fallback until that keymap arrives.

`NotifyPointer*` and `NotifyKeyboard*` remain available for clients that never
call `ConnectToEIS`. Vertical scroll on the Notify path is inverted once,
because KDE Connect already converts to the libei sign before `ConnectToEIS`
but sends the original sign on the Notify fallback. Positive Y matches
`wl_pointer` (downward) after that conversion.
