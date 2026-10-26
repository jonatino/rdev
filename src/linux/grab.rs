use crate::linux::common::Display;
use crate::linux::keyboard::Keyboard;
use crate::rdev::{Button, Event, EventType, GrabError, Key, KeyboardState};
use epoll::ControlOptions::{EPOLL_CTL_ADD, EPOLL_CTL_DEL};
use evdev_rs::{
    enums::{EventCode, EV_KEY, EV_REL, EV_SYN},
    Device, InputEvent, TimeVal, UInputDevice,
};
use inotify::{Inotify, WatchMask};
use std::collections::HashSet;
use std::fs::{read_dir, File};
use std::io;
use std::os::unix::{
    fs::FileTypeExt,
    io::{AsRawFd, RawFd},
};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

// TODO The x, y coordinates are currently wrong !! Is there mouse acceleration
// to take into account ??

macro_rules! convert_keys {
    ($($ev_key:ident, $rdev_key:ident),*) => {
        //TODO: make const when rust lang issue #49146 is fixed
        #[allow(unreachable_patterns)]
        fn evdev_key_to_rdev_key(key: &EV_KEY) -> Option<Key> {
            match key {
                $(
                    EV_KEY::$ev_key => Some(Key::$rdev_key),
                )*
                _ => None,
            }
        }

        // //TODO: make const when rust lang issue #49146 is fixed
        // fn rdev_key_to_evdev_key(key: &Key) -> Option<EV_KEY> {
        //     match key {
        //         $(
        //             Key::$rdev_key => Some(EV_KEY::$ev_key),
        //         )*
        //         _ => None
        //     }
        // }
    };
}

macro_rules! convert_buttons {
    ($($ev_key:ident, $rdev_key:ident),*) => {
        //TODO: make const when rust lang issue #49146 is fixed
        fn evdev_key_to_rdev_button(key: &EV_KEY) -> Option<Button> {
            match key {
                $(
                    EV_KEY::$ev_key => Some(Button::$rdev_key),
                )*
                _ => None,
            }
        }

        // //TODO: make const when rust lang issue #49146 is fixed
        // fn rdev_button_to_evdev_key(event: &Button) -> Option<EV_KEY> {
        //     match event {
        //         $(
        //             Button::$rdev_key => Some(EV_KEY::$ev_key),
        //         )*
        //         _ => None
        //     }
        // }
    };
}

#[rustfmt::skip]
convert_buttons!(
    BTN_LEFT, Left,
    BTN_RIGHT, Right,
    BTN_MIDDLE, Middle
);

//TODO: IntlBackslash, kpDelete
#[rustfmt::skip]
convert_keys!(
    KEY_ESC, Escape,
    KEY_1, Num1,
    KEY_2, Num2,
    KEY_3, Num3,
    KEY_4, Num4,
    KEY_5, Num5,
    KEY_6, Num6,
    KEY_7, Num7,
    KEY_8, Num8,
    KEY_9, Num9,
    KEY_0, Num0,
    KEY_MINUS, Minus,
    KEY_EQUAL, Equal,
    KEY_BACKSPACE, Backspace,
    KEY_TAB, Tab,
    KEY_Q, KeyQ,
    KEY_W, KeyW,
    KEY_E, KeyE,
    KEY_R, KeyR,
    KEY_T, KeyT,
    KEY_Y, KeyY,
    KEY_U, KeyU,
    KEY_I, KeyI,
    KEY_O, KeyO,
    KEY_P, KeyP,
    KEY_LEFTBRACE, LeftBracket,
    KEY_RIGHTBRACE, RightBracket,
    KEY_ENTER, Return,
    KEY_LEFTCTRL, ControlLeft,
    KEY_A, KeyA,
    KEY_S, KeyS,
    KEY_D, KeyD,
    KEY_F, KeyF,
    KEY_G, KeyG,
    KEY_H, KeyH,
    KEY_J, KeyJ,
    KEY_K, KeyK,
    KEY_L, KeyL,
    KEY_SEMICOLON, SemiColon,
    KEY_APOSTROPHE, Quote,
    KEY_GRAVE, BackQuote,
    KEY_LEFTSHIFT, ShiftLeft,
    KEY_BACKSLASH, BackSlash,
    KEY_Z, KeyZ,
    KEY_X, KeyX,
    KEY_C, KeyC,
    KEY_V, KeyV,
    KEY_B, KeyB,
    KEY_N, KeyN,
    KEY_M, KeyM,
    KEY_COMMA, Comma,
    KEY_DOT, Dot,
    KEY_SLASH, Slash,
    KEY_RIGHTSHIFT, ShiftRight,
    KEY_KPASTERISK , KpMultiply,
    KEY_LEFTALT, Alt,
    KEY_SPACE, Space,
    KEY_CAPSLOCK, CapsLock,
    KEY_F1, F1,
    KEY_F2, F2,
    KEY_F3, F3,
    KEY_F4, F4,
    KEY_F5, F5,
    KEY_F6, F6,
    KEY_F7, F7,
    KEY_F8, F8,
    KEY_F9, F9,
    KEY_F10, F10,
    KEY_NUMLOCK, NumLock,
    KEY_SCROLLLOCK, ScrollLock,
    KEY_KP7, Kp7,
    KEY_KP8, Kp8,
    KEY_KP9, Kp9,
    KEY_KPMINUS, KpMinus,
    KEY_KP4, Kp4,
    KEY_KP5, Kp5,
    KEY_KP6, Kp6,
    KEY_KPPLUS, KpPlus,
    KEY_KP1, Kp1,
    KEY_KP2, Kp2,
    KEY_KP3, Kp3,
    KEY_KP0, Kp0,
    KEY_F11, F11,
    KEY_F12, F12,
    KEY_KPENTER, KpReturn,
    KEY_RIGHTCTRL, ControlRight,
    KEY_KPSLASH, KpDivide,
    KEY_RIGHTALT, AltGr,
    KEY_HOME , Home,
    KEY_UP, UpArrow,
    KEY_PAGEUP, PageUp,
    KEY_LEFT, LeftArrow,
    KEY_RIGHT, RightArrow,
    KEY_END, End,
    KEY_DOWN, DownArrow,
    KEY_PAGEDOWN, PageDown,
    KEY_INSERT, Insert,
    KEY_DELETE, Delete,
    KEY_PAUSE, Pause,
    KEY_LEFTMETA, MetaLeft,
    KEY_RIGHTMETA, MetaRight,
    KEY_PRINT, PrintScreen,
    // KpDelete behaves like normal Delete most of the time
    KEY_DELETE, KpDelete,
    // Linux doesn't have an IntlBackslash key
    KEY_BACKSLASH, IntlBackslash,
    KEY_MUTE, VolumeMute,
    KEY_VOLUMEDOWN, VolumeDown,
    KEY_VOLUMEUP, VolumeUp
);

fn evdev_event_to_rdev_event(
    event: &InputEvent,
    x: &mut f64,
    y: &mut f64,
    w: f64,
    h: f64,
) -> Option<EventType> {
    match &event.event_code {
        EventCode::EV_KEY(key) => {
            if let Some(button) = evdev_key_to_rdev_button(key) {
                // first check if pressed key is a mouse button
                match event.value {
                    0 => Some(EventType::ButtonRelease(button)),
                    _ => Some(EventType::ButtonPress(button)),
                }
            } else if let Some(key) = evdev_key_to_rdev_key(key) {
                // check if pressed key is a keyboard key
                match event.value {
                    0 => Some(EventType::KeyRelease(key)),
                    _ => Some(EventType::KeyPress(key)),
                }
            } else {
                // if neither mouse button nor keyboard key, return none
                None
            }
        }
        EventCode::EV_REL(mouse) => match mouse {
            EV_REL::REL_X => {
                let dx = event.value as f64;
                *x += dx;
                if *x < 0.0 {
                    *x = 0.0;
                }
                if *x > w {
                    *x = w;
                }
                Some(EventType::MouseMove { x: *x, y: *y })
            }
            EV_REL::REL_Y => {
                let dy = event.value as f64;
                *y += dy;
                if *y < 0.0 {
                    *y = 0.0;
                }
                if *y > h {
                    *y = h;
                }
                Some(EventType::MouseMove { x: *x, y: *y })
            }
            EV_REL::REL_HWHEEL => Some(EventType::Wheel {
                delta_x: event.value.into(),
                delta_y: 0,
            }),
            EV_REL::REL_WHEEL => Some(EventType::Wheel {
                delta_x: 0,
                delta_y: event.value.into(),
            }),
            // Other EV_REL events cannot be represented by rdev
            _ => None,
        },
        // Other event_codes cannot be represented by rdev,
        // and some never will e.g. EV_SYN
        _ => None,
    }
}

// fn rdev_event_to_evdev_event(event: &EventType, time: &TimeVal) -> Option<InputEvent> {
//     match event {
//         EventType::KeyPress(key) => {
//             let key = rdev_key_to_evdev_key(&key)?;
//             Some(InputEvent::new(&time, &EventCode::EV_KEY(key), 1))
//         }
//         EventType::KeyRelease(key) => {
//             let key = rdev_key_to_evdev_key(&key)?;
//             Some(InputEvent::new(&time, &EventCode::EV_KEY(key), 0))
//         }
//         EventType::ButtonPress(button) => {
//             let button = rdev_button_to_evdev_key(&button)?;
//             Some(InputEvent::new(&time, &EventCode::EV_KEY(button), 1))
//         }
//         EventType::ButtonRelease(button) => {
//             let button = rdev_button_to_evdev_key(&button)?;
//             Some(InputEvent::new(&time, &EventCode::EV_KEY(button), 0))
//         }
//         EventType::MouseMove { x, y } => {
//             let (x, y) = (*x as i32, *y as i32);
//             //TODO allow both x and y movements simultaneously
//             if x != 0 {
//                 Some(InputEvent::new(&time, &EventCode::EV_REL(EV_REL::REL_X), x))
//             } else {
//                 Some(InputEvent::new(&time, &EventCode::EV_REL(EV_REL::REL_Y), y))
//             }
//         }
//         EventType::Wheel { delta_x, delta_y } => {
//             let (x, y) = (*delta_x as i32, *delta_y as i32);
//             //TODO allow both x and y movements simultaneously
//             if x != 0 {
//                 Some(InputEvent::new(
//                     &time,
//                     &EventCode::EV_REL(EV_REL::REL_HWHEEL),
//                     x,
//                 ))
//             } else {
//                 Some(InputEvent::new(
//                     &time,
//                     &EventCode::EV_REL(EV_REL::REL_WHEEL),
//                     y,
//                 ))
//             }
//         }
//     }
// }

/// Filter to detect physical keyboards and media keyboards.
/// Excludes virtual devices (uinput), mice/pointing devices, audio jacks, and non-keyboard peripherals.
fn is_keyboard(device: &Device) -> bool {
    // 1. Must have a physical hardware address (not a virtual uinput device)
    let phys = match device.phys() {
        Some(p) if !p.is_empty() => p,
        _ => return false,
    };
    if phys.starts_with("ALSA") {
        return false;
    }

    // 2. Exclude mice / pointing devices:
    // Pointers have horizontal and vertical relative motion (REL_X and REL_Y)
    if device.has_event_code(&EventCode::EV_REL(EV_REL::REL_X))
        && device.has_event_code(&EventCode::EV_REL(EV_REL::REL_Y))
    {
        return false;
    }

    // Exclude mouse click buttons (BTN_LEFT, BTN_RIGHT, BTN_MIDDLE)
    if device.has_event_code(&EventCode::EV_KEY(EV_KEY::BTN_LEFT))
        || device.has_event_code(&EventCode::EV_KEY(EV_KEY::BTN_RIGHT))
        || device.has_event_code(&EventCode::EV_KEY(EV_KEY::BTN_MIDDLE))
    {
        return false;
    }

    // Exclude devices with "mouse", "touchpad", or non-keyboard peripherals in name
    let name = device.name().unwrap_or_default().to_lowercase();
    if name.contains("mouse")
        || name.contains("touchpad")
        || name.contains("trackball")
        || name.contains("model d")
        || name.contains("model o")
        || name.contains("model i")
        || name.contains("power button")
        || name.contains("sleep button")
        || name.contains("video bus")
        || name.contains("led controller")
        || name.contains("pc speaker")
    {
        return false;
    }

    // 3. Must support EV_KEY
    if !device.has_event_type(&evdev_rs::enums::EventType::EV_KEY) {
        return false;
    }

    // 4. Must support real keyboard keys or media volume keys
    device.has_event_code(&EventCode::EV_KEY(EV_KEY::KEY_A))
        || device.has_event_code(&EventCode::EV_KEY(EV_KEY::KEY_SPACE))
        || device.has_event_code(&EventCode::EV_KEY(EV_KEY::KEY_ENTER))
        || device.has_event_code(&EventCode::EV_KEY(EV_KEY::KEY_VOLUMEUP))
        || device.has_event_code(&EventCode::EV_KEY(EV_KEY::KEY_VOLUMEDOWN))
        || device.has_event_code(&EventCode::EV_KEY(EV_KEY::KEY_MUTE))
}

/// Filter for general input grabbing (keyboards or pointers), excluding virtual devices and non-inputs.
fn is_keyboard_or_pointer(device: &Device) -> bool {
    let phys = match device.phys() {
        Some(p) if !p.is_empty() => p,
        _ => return false,
    };
    if phys.starts_with("ALSA") {
        return false;
    }
    let name = device.name().unwrap_or_default().to_lowercase();
    if name.contains("power button")
        || name.contains("sleep button")
        || name.contains("video bus")
        || name.contains("led controller")
        || name.contains("pc speaker")
    {
        return false;
    }
    device.has_event_type(&evdev_rs::enums::EventType::EV_KEY)
        || device.has_event_type(&evdev_rs::enums::EventType::EV_REL)
}

pub struct ManagedDevice {
    pub device: Device,
    pub output: UInputDevice,
    pub fd: RawFd,
    pub grabbed: bool,
    pub held_keys: HashSet<EV_KEY>,
}

impl ManagedDevice {
    #[allow(dead_code)]
    pub fn new(file: File) -> io::Result<Self> {
        let fd = file.as_raw_fd();
        let device = Device::new_from_fd(file)?;
        let output = UInputDevice::create_from_device(&device)?;
        Ok(Self {
            device,
            output,
            fd,
            grabbed: false,
            held_keys: HashSet::new(),
        })
    }

    pub fn grab(&mut self) -> io::Result<()> {
        if !self.grabbed {
            self.device.grab(evdev_rs::GrabMode::Grab)?;
            self.grabbed = true;
        }
        Ok(())
    }

    pub fn ungrab(&mut self) {
        if self.grabbed {
            let _ = self.device.grab(evdev_rs::GrabMode::Ungrab);
            self.grabbed = false;
        }
    }

    pub fn record_and_write(&mut self, event: &InputEvent) -> io::Result<()> {
        if let EventCode::EV_KEY(ref k) = event.event_code {
            if event.value == 1 {
                self.held_keys.insert(k.clone());
            } else if event.value == 0 {
                self.held_keys.remove(k);
            }
        }
        self.output.write_event(event)
    }

    pub fn flush_held_keys(&mut self) {
        let time = TimeVal::new(0, 0);
        for k in self.held_keys.drain() {
            let rel_event = InputEvent::new(&time, &EventCode::EV_KEY(k), 0);
            let syn_event =
                InputEvent::new(&time, &EventCode::EV_SYN(EV_SYN::SYN_REPORT), 0);
            let _ = self.output.write_event(&rel_event);
            let _ = self.output.write_event(&syn_event);
        }
    }
}

impl Drop for ManagedDevice {
    fn drop(&mut self) {
        self.flush_held_keys();
        self.ungrab();
    }
}

/// Grab physical keyboard events without an X11 display or text translation.
/// Non-key events are forwarded unchanged and Event::name is always None.
pub fn grab_keys<T>(callback: T) -> Result<(), GrabError>
where
    T: Fn(Event) -> Option<Event> + 'static,
{
    filter_map_events(is_keyboard, |event| {
        let key = match event.event_code {
            EventCode::EV_KEY(ref key) => match evdev_key_to_rdev_key(key) {
                Some(key) => key,
                None => return (Some(event), GrabStatus::Continue),
            },
            _ => return (Some(event), GrabStatus::Continue),
        };
        let event_type = match event.value {
            0 => EventType::KeyRelease(key),
            1 | 2 => EventType::KeyPress(key),
            _ => return (Some(event), GrabStatus::Continue),
        };
        let translated = Event {
            time: SystemTime::now(),
            name: None,
            event_type,
        };
        if callback(translated).is_some() {
            (Some(event), GrabStatus::Continue)
        } else {
            (None, GrabStatus::Continue)
        }
    })?;
    Ok(())
}

pub fn grab<T>(callback: T) -> Result<(), GrabError>
where
    T: Fn(Event) -> Option<Event> + 'static,
{
    let mut kb = Keyboard::new().ok_or(GrabError::KeyboardError)?;
    let display = Display::new().ok_or(GrabError::MissingDisplayError)?;
    let (width, height) = display.get_size().ok_or(GrabError::MissingDisplayError)?;
    let (current_x, current_y) = display
        .get_mouse_pos()
        .ok_or(GrabError::MissingDisplayError)?;
    let mut x = current_x as f64;
    let mut y = current_y as f64;
    let w = width as f64;
    let h = height as f64;
    filter_map_events(is_keyboard_or_pointer, |event| {
        let event_type = match evdev_event_to_rdev_event(&event, &mut x, &mut y, w, h) {
            Some(rdev_event) => rdev_event,
            // If we can't convert event, simulate it
            None => return (Some(event), GrabStatus::Continue),
        };
        let name = kb.add(&event_type);
        let rdev_event = Event {
            time: SystemTime::now(),
            name,
            event_type,
        };
        if callback(rdev_event).is_some() {
            (Some(event), GrabStatus::Continue)
        } else {
            // callback returns None, swallow the event
            (None, GrabStatus::Continue)
        }
    })?;
    Ok(())
}

pub fn filter_map_events<P, F>(predicate: P, mut func: F) -> io::Result<()>
where
    P: Fn(&Device) -> bool,
    F: FnMut(InputEvent) -> (Option<InputEvent>, GrabStatus),
{
    let (epoll_fd, mut devices) = setup_devices(&predicate)?;
    let mut inotify = setup_inotify(epoll_fd, devices.len())?;

    let mut epoll_buffer = [epoll::Event::new(epoll::Events::empty(), 0); 16];
    let mut inotify_buffer = vec![0_u8; 4096];

    'event_loop: loop {
        let num_events = match epoll::wait(epoll_fd, -1, &mut epoll_buffer) {
            Ok(n) => n,
            Err(ref e) if e.raw_os_error() == Some(libc::EINTR) => continue,
            Err(e) => return Err(e),
        };

        'events: for event in &epoll_buffer[0..num_events] {
            if event.data == INOTIFY_DATA {
                if let Ok(inotify_events) = inotify.read_events(&mut inotify_buffer) {
                    for in_ev in inotify_events {
                        if in_ev.mask.contains(inotify::EventMask::CREATE) {
                            if let Some(name) = in_ev.name {
                                let name_str = name.to_string_lossy();
                                if !name_str.starts_with("event") {
                                    continue;
                                }

                                let mut device_path = PathBuf::from(DEV_PATH);
                                device_path.push(name);

                                // Retry up to 5 times (125ms total) for udev permissions
                                let mut file_opt = None;
                                for _ in 0..5 {
                                    match File::open(&device_path) {
                                        Ok(f) => {
                                            file_opt = Some(f);
                                            break;
                                        }
                                        Err(ref e)
                                            if e.kind() == io::ErrorKind::PermissionDenied =>
                                        {
                                            std::thread::sleep(Duration::from_millis(25));
                                        }
                                        Err(_) => break,
                                    }
                                }

                                let file = match file_opt {
                                    Some(f) => f,
                                    None => continue,
                                };
                                let fd = file.as_raw_fd();
                                let dev = match Device::new_from_fd(file) {
                                    Ok(d) => d,
                                    Err(_) => continue,
                                };
                                if !predicate(&dev) {
                                    continue;
                                }
                                let out = match UInputDevice::create_from_device(&dev) {
                                    Ok(o) => o,
                                    Err(_) => continue,
                                };
                                let mut managed = ManagedDevice {
                                    device: dev,
                                    output: out,
                                    fd,
                                    grabbed: false,
                                    held_keys: HashSet::new(),
                                };
                                if managed.grab().is_err() {
                                    continue;
                                }
                                let new_idx = devices.len();
                                if new_idx as u64 >= INOTIFY_DATA {
                                    managed.ungrab();
                                    continue;
                                }
                                let ep_event = epoll::Event::new(EPOLLIN, new_idx as u64);
                                if epoll::ctl(epoll_fd, EPOLL_CTL_ADD, fd, ep_event).is_err() {
                                    managed.ungrab();
                                    continue;
                                }
                                devices.push(managed);
                            }
                        }
                    }
                }
            } else {
                let device_idx = event.data as usize;
                let Some(item) = devices.get_mut(device_idx) else {
                    continue;
                };

                while item.device.has_event_pending() {
                    let (_, ev) = match item.device.next_event(evdev_rs::ReadFlag::NORMAL) {
                        Ok(ev) => ev,
                        Err(err) => {
                            let raw_err = err.raw_os_error().unwrap_or(0);
                            if raw_err == libc::EAGAIN
                                || raw_err == libc::EWOULDBLOCK
                                || raw_err == libc::EINTR
                            {
                                break;
                            }
                            // Fatal device error (device disconnected, etc.)
                            item.ungrab();
                            let empty_event = epoll::Event::new(epoll::Events::empty(), 0);
                            let _ = epoll::ctl(epoll_fd, EPOLL_CTL_DEL, item.fd, empty_event);
                            continue 'events;
                        }
                    };

                    let (out_ev, grab_status) = func(ev);

                    if let Some(out_ev) = out_ev {
                        let _ = item.record_and_write(&out_ev);
                    }

                    if grab_status == GrabStatus::Stop {
                        break 'event_loop;
                    }
                }
            }
        }
    }

    // Explicit drop calls drop on every ManagedDevice (flushing held keys and ungrabbing)
    drop(devices);
    let _ = epoll::close(epoll_fd);

    Ok(())
}

static DEV_PATH: &str = "/dev/input";
const INOTIFY_DATA: u64 = u64::MAX;
const EPOLLIN: epoll::Events = epoll::Events::EPOLLIN;

/// Whether to continue grabbing events or to stop
/// Used in `filter_map_events` (and others)
#[derive(Debug, Eq, PartialEq, Hash)]
pub enum GrabStatus {
    /// Stop grabbing
    Continue,
    /// ungrab events
    Stop,
}

fn get_device_files<T>(path: T) -> io::Result<Vec<File>>
where
    T: AsRef<Path>,
{
    let mut res = Vec::new();
    let mut entries: Vec<_> = read_dir(path)?.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.path());

    for entry in entries {
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if !file_type.is_char_device() {
            continue;
        }

        let path = entry.path();
        let file_name = match path.file_name() {
            Some(name) => name.to_string_lossy(),
            None => continue,
        };

        if !file_name.starts_with("event") {
            continue;
        }

        if let Ok(file) = File::open(&path) {
            res.push(file);
        }
    }
    Ok(res)
}

fn setup_devices<P>(predicate: &P) -> io::Result<(RawFd, Vec<ManagedDevice>)>
where
    P: Fn(&Device) -> bool,
{
    let epoll_fd = epoll::create(true)?;
    let mut managed_devices = Vec::new();
    let device_files = get_device_files(DEV_PATH)?;

    for file in device_files {
        let fd = file.as_raw_fd();
        let device = match Device::new_from_fd(file) {
            Ok(d) => d,
            Err(_) => continue,
        };

        if !predicate(&device) {
            continue;
        }

        let output = match UInputDevice::create_from_device(&device) {
            Ok(o) => o,
            Err(_) => continue,
        };

        let mut managed = ManagedDevice {
            device,
            output,
            fd,
            grabbed: false,
            held_keys: HashSet::new(),
        };

        if managed.grab().is_err() {
            continue;
        }

        let idx = managed_devices.len();
        let epoll_event = epoll::Event::new(EPOLLIN, idx as u64);
        if epoll::ctl(epoll_fd, EPOLL_CTL_ADD, fd, epoll_event).is_err() {
            managed.ungrab();
            continue;
        }

        managed_devices.push(managed);
    }

    Ok((epoll_fd, managed_devices))
}

fn setup_inotify(epoll_fd: RawFd, devices_len: usize) -> io::Result<Inotify> {
    if devices_len as u64 >= INOTIFY_DATA {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "too many device files!",
        ));
    }
    let mut inotify = Inotify::init()?;
    inotify.add_watch(DEV_PATH, WatchMask::CREATE)?;
    let epoll_event = epoll::Event::new(EPOLLIN, INOTIFY_DATA);
    epoll::ctl(epoll_fd, EPOLL_CTL_ADD, inotify.as_raw_fd(), epoll_event)?;
    Ok(inotify)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_filtering() {
        if let Ok(files) = get_device_files(DEV_PATH) {
            for file in files {
                if let Ok(dev) = Device::new_from_fd(file) {
                    let name = dev.name().unwrap_or_default();
                    let kb = is_keyboard(&dev);
                    println!("Device '{}' is_keyboard = {}", name, kb);
                    if kb {
                        assert!(
                            !name.to_lowercase().contains("mouse"),
                            "Device '{}' was classified as a keyboard but contains 'mouse'",
                            name
                        );
                        assert!(
                            !dev.has_event_code(&EventCode::EV_REL(EV_REL::REL_X))
                                || !dev.has_event_code(&EventCode::EV_REL(EV_REL::REL_Y)),
                            "Device '{}' was classified as a keyboard but has REL_X and REL_Y",
                            name
                        );
                    }
                }
            }
        }
    }
}
