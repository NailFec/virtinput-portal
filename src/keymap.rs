//! XKB keymap loading and keysym-to-evdev translation.
//!
//! KDE Connect sends either evdev keycodes or keysyms. Virtual keyboards on
//! Wayland consume evdev keycodes, so keysyms are resolved against the keymap
//! the compositor gave us.

use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, OwnedFd};

use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, memfd_create};
use xkbcommon::xkb::{
    self, KEYMAP_COMPILE_NO_FLAGS, KEYMAP_FORMAT_TEXT_V1, Keycode, Keymap, State,
};

pub struct Layout {
    pub text: String,
    pub keymap: Keymap,
    pub state: State,
}

impl Layout {
    pub fn fallback() -> io::Result<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap =
            Keymap::new_from_names(&context, "", "", "", "", None, KEYMAP_COMPILE_NO_FLAGS)
                .ok_or_else(|| io::Error::other("failed to compile the fallback XKB keymap"))?;
        let text = keymap.get_as_string(KEYMAP_FORMAT_TEXT_V1);
        let state = State::new(&keymap);
        Ok(Self {
            text,
            keymap,
            state,
        })
    }

    /// Replace the layout with a keymap file descriptor from `wl_keyboard`.
    ///
    /// # Safety
    /// `fd` must be a readable keymap mapping of `size` bytes, including the
    /// trailing NUL, as provided by the compositor.
    pub unsafe fn from_keymap_fd(fd: OwnedFd, size: usize) -> io::Result<Self> {
        if size < 2 {
            return Err(io::Error::other("keymap file is empty"));
        }
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = unsafe {
            Keymap::new_from_fd(
                &context,
                fd,
                size,
                KEYMAP_FORMAT_TEXT_V1,
                KEYMAP_COMPILE_NO_FLAGS,
            )?
        }
        .ok_or_else(|| io::Error::other("compositor keymap failed to compile"))?;
        let text = keymap.get_as_string(KEYMAP_FORMAT_TEXT_V1);
        let state = State::new(&keymap);
        Ok(Self {
            text,
            keymap,
            state,
        })
    }

    /// Resolve a keysym to an evdev keycode (`xkb keycode - 8`).
    pub fn evdev_from_keysym(&self, keysym: u32) -> Option<u32> {
        let layout = self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE);
        let min = self.keymap.min_keycode().raw();
        let max = self.keymap.max_keycode().raw();
        for code in min..max {
            let key = Keycode::new(code);
            let levels = self.keymap.num_levels_for_key(key, layout);
            for level in 0..levels {
                let syms = self.keymap.key_get_syms_by_level(key, layout, level);
                if syms.iter().any(|sym| sym.raw() == keysym) {
                    return Some(code.saturating_sub(8));
                }
            }
        }
        None
    }

    /// Feed an evdev keycode into the XKB state and return the modifier mask.
    pub fn update_evdev(&mut self, evdev: u32, pressed: bool) -> Modifiers {
        let direction = if pressed {
            xkb::KeyDirection::Down
        } else {
            xkb::KeyDirection::Up
        };
        self.state
            .update_key(Keycode::new(evdev.saturating_add(8)), direction);
        Modifiers {
            depressed: self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
            latched: self.state.serialize_mods(xkb::STATE_MODS_LATCHED),
            locked: self.state.serialize_mods(xkb::STATE_MODS_LOCKED),
            group: self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Modifiers {
    pub depressed: u32,
    pub latched: u32,
    pub locked: u32,
    pub group: u32,
}

/// A sealed memfd containing `text` and a trailing NUL.
///
/// The returned size includes the NUL so both libeis and the virtual keyboard
/// protocol can map the same bytes.
pub fn sealed_keymap(text: &str) -> io::Result<(OwnedFd, usize)> {
    let mut bytes = text.as_bytes().to_vec();
    if !bytes.ends_with(&[0]) {
        bytes.push(0);
    }
    let fd = memfd_create(
        c"virtinput-keymap",
        MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
    )?;
    {
        let mut file = File::from(fd.try_clone()?);
        file.write_all(&bytes)?;
        file.flush()?;
    }
    fcntl_add_seals(
        &fd,
        SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL,
    )?;
    let _ = fd.as_raw_fd();
    Ok((fd, bytes.len()))
}

#[cfg(test)]
mod tests {
    use super::Layout;

    #[test]
    fn letter_a_is_evdev_keycode_30() {
        let layout = Layout::fallback().expect("fallback keymap");
        let keysym_a = 0x0061;
        assert_eq!(layout.evdev_from_keysym(keysym_a), Some(30));
    }
}
