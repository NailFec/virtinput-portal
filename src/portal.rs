//! `org.freedesktop.impl.portal.RemoteDesktop` backend.
//!
//! xdg-desktop-portal calls these methods. KDE Connect 26.08 creates a
//! session, starts it, then delivers pointer and keyboard events through
//! `ConnectToEIS`. The `Notify*` methods cover clients that never open an
//! EIS connection.

use std::collections::HashMap;
use std::sync::Mutex;

use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue, StructureBuilder, Value};

use crate::input::{Command, Commander};

const KEYBOARD: u32 = 1;
const POINTER: u32 = 2;
const AVAILABLE: u32 = KEYBOARD | POINTER;

struct Session {
    started: bool,
    persist_mode: u32,
}

pub struct Portal {
    commander: Commander,
    sessions: Mutex<HashMap<String, Session>>,
}

impl Portal {
    pub fn new(commander: Commander) -> Self {
        Self {
            commander,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, Session>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn require_started(&self, session_handle: &str) -> zbus::fdo::Result<()> {
        match self.sessions().get(session_handle) {
            Some(session) if session.started => Ok(()),
            Some(_) => Err(zbus::fdo::Error::Failed(
                "remote desktop session is not started".into(),
            )),
            None => Err(zbus::fdo::Error::Failed(
                "unknown remote desktop session".into(),
            )),
        }
    }

    fn send(&self, command: Command) -> zbus::fdo::Result<()> {
        self.commander
            .send(command)
            .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))
    }
}

#[interface(name = "org.freedesktop.impl.portal.RemoteDesktop")]
impl Portal {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }

    #[zbus(property)]
    fn available_device_types(&self) -> u32 {
        AVAILABLE
    }

    #[zbus(name = "CreateSession")]
    fn create_session(
        &self,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        log::info!("remote desktop session requested by {app_id}");
        self.sessions().insert(
            session_handle.to_string(),
            Session {
                started: false,
                persist_mode: 0,
            },
        );
        (0, HashMap::new())
    }

    #[zbus(name = "SelectDevices")]
    fn select_devices(
        &self,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        _app_id: &str,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        if let Some(session) = self.sessions().get_mut(session_handle.as_str()) {
            session.persist_mode = option_u32(&options, "persist_mode").unwrap_or(0);
        }
        (0, HashMap::new())
    }

    #[zbus(name = "Start")]
    fn start(
        &self,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: &str,
        _parent_window: &str,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let mut sessions = self.sessions();
        let Some(session) = sessions.get_mut(session_handle.as_str()) else {
            log::warn!("start for unknown session {session_handle}");
            return (2, HashMap::new());
        };
        let persist = option_u32(&options, "persist_mode").unwrap_or(session.persist_mode);
        session.persist_mode = persist;
        session.started = true;
        log::info!("remote desktop session started for {app_id}");

        let mut results = HashMap::new();
        results.insert("devices".to_string(), OwnedValue::from(AVAILABLE));
        results.insert("clipboard_enabled".to_string(), OwnedValue::from(false));
        if persist > 0 {
            results.insert("restore_data".to_string(), restore_data());
        }
        (0, results)
    }

    #[zbus(name = "NotifyPointerMotion")]
    fn notify_pointer_motion(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        dx: f64,
        dy: f64,
    ) -> zbus::fdo::Result<()> {
        self.require_started(session_handle.as_str())?;
        self.send(Command::Motion { dx, dy })
    }

    #[zbus(name = "NotifyPointerMotionAbsolute")]
    fn notify_pointer_motion_absolute(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        _stream: u32,
        x: f64,
        y: f64,
    ) -> zbus::fdo::Result<()> {
        self.require_started(session_handle.as_str())?;
        self.send(Command::MotionAbsolute { x, y })
    }

    #[zbus(name = "NotifyPointerButton")]
    fn notify_pointer_button(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        button: i32,
        state: u32,
    ) -> zbus::fdo::Result<()> {
        self.require_started(session_handle.as_str())?;
        self.send(Command::Button {
            button: button as u32,
            pressed: state != 0,
        })
    }

    #[zbus(name = "NotifyPointerAxis")]
    fn notify_pointer_axis(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        dx: f64,
        dy: f64,
    ) -> zbus::fdo::Result<()> {
        self.require_started(session_handle.as_str())?;
        // KDE Connect's portal fallback sends the phone's scroll delta, which
        // uses the opposite vertical sign from wl_pointer. The libei path is
        // already converted by the client, so only this method is inverted.
        self.send(Command::Scroll { dx, dy: -dy })
    }

    #[zbus(name = "NotifyPointerAxisDiscrete")]
    fn notify_pointer_axis_discrete(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        axis: u32,
        steps: i32,
    ) -> zbus::fdo::Result<()> {
        self.require_started(session_handle.as_str())?;
        let steps = if axis == 0 { -steps } else { steps };
        self.send(Command::ScrollDiscrete { axis, steps })
    }

    #[zbus(name = "NotifyKeyboardKeycode")]
    fn notify_keyboard_keycode(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        keycode: i32,
        state: u32,
    ) -> zbus::fdo::Result<()> {
        self.require_started(session_handle.as_str())?;
        self.send(Command::Key {
            keycode: keycode as u32,
            pressed: state != 0,
        })
    }

    #[zbus(name = "NotifyKeyboardKeysym")]
    fn notify_keyboard_keysym(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        keysym: i32,
        state: u32,
    ) -> zbus::fdo::Result<()> {
        self.require_started(session_handle.as_str())?;
        self.send(Command::Keysym {
            keysym: keysym as u32,
            pressed: state != 0,
        })
    }

    #[zbus(name = "NotifyTouchDown")]
    fn notify_touch_down(
        &self,
        _session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        _stream: u32,
        _slot: u32,
        _x: f64,
        _y: f64,
    ) {
    }

    #[zbus(name = "NotifyTouchMotion")]
    fn notify_touch_motion(
        &self,
        _session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        _stream: u32,
        _slot: u32,
        _x: f64,
        _y: f64,
    ) {
    }

    #[zbus(name = "NotifyTouchUp")]
    fn notify_touch_up(
        &self,
        _session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
        _slot: u32,
    ) {
    }

    #[zbus(name = "ConnectToEIS")]
    fn connect_to_eis(
        &self,
        session_handle: ObjectPath<'_>,
        app_id: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
        self.require_started(session_handle.as_str())?;
        log::info!("ConnectToEIS for {app_id}");
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        self.send(Command::AddEisClient { reply: reply_tx })?;
        match reply_rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(Ok(fd)) => Ok(fd.into()),
            Ok(Err(err)) => Err(zbus::fdo::Error::Failed(err.to_string())),
            Err(_) => Err(zbus::fdo::Error::Failed(
                "timed out creating an EIS client".into(),
            )),
        }
    }
}

fn option_u32(options: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    options.get(key).and_then(|value| u32::try_from(value).ok())
}

/// `(suv)` restore record. xdg-desktop-portal turns this into the
/// `restore_token` string KDE Connect stores.
fn restore_data() -> OwnedValue {
    let structure = StructureBuilder::new()
        .add_field("virtinput-portal")
        .add_field(1u32)
        .add_field(Value::from("granted"))
        .build()
        .expect("restore_data structure");
    Value::Structure(structure)
        .try_to_owned()
        .expect("restore_data can be owned")
}

#[cfg(test)]
mod tests {
    use super::restore_data;
    use zbus::zvariant::{Structure, Value};

    #[test]
    fn restore_data_signature_is_suv() {
        let owned = restore_data();
        let value = Value::from(owned);
        let structure = Structure::try_from(value).expect("structure");
        assert_eq!(structure.signature().to_string(), "(suv)");
    }
}
