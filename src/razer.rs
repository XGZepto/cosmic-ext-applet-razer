//! Razer laptop HID protocol over Linux hidraw.
//!
//! This mirrors exactly what the RazerControl web app (rzrctrl.com) sends via
//! WebHID: 91-byte feature reports (report ID 0 + 90-byte Razer packet) to the
//! laptop's hidraw interface. No daemon or kernel module needed.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const RAZER_VENDOR: u16 = 0x1532;

/// Laptop product IDs accepted by RazerControl (Blade 14/15/16/17, Stealth, Book…).
pub const LAPTOP_PIDS: &[u16] = &[
    0x0224, 0x0233, 0x023b, 0x0240, 0x0246, 0x023a, 0x0245, 0x0255, 0x0253, 0x022d, 0x0232, 0x0239,
    0x024a, 0x0252, 0x0256, 0x0234, 0x022f, 0x0225, 0x0210, 0x020f, 0x026a, 0x026f, 0x0270, 0x0276,
    0x026d, 0x027a, 0x028a, 0x028b, 0x028c, 0x0259, 0x029f, 0x029d,
];

const REPORT_LEN: usize = 91;

// Razer packet status codes (byte 1 of the response)
const STATUS_BUSY: u8 = 0x01;
const STATUS_OK: u8 = 0x02;
const STATUS_FAIL: u8 = 0x03;
const STATUS_UNSUPPORTED: u8 = 0x05;

const CLASS_DEVICE: u8 = 0x00;
const CLASS_LIGHTING: u8 = 0x03;
const CLASS_BATTERY: u8 = 0x07;
const CLASS_PERFORMANCE: u8 = 0x0d;

pub const ZONE_CPU: u8 = 0x01;
pub const ZONE_GPU: u8 = 0x02;

pub const POWER_BALANCED: u8 = 0x00;
pub const POWER_CUSTOM: u8 = 0x04;

pub const FAN_RPM_MIN: u32 = 2000;
pub const FAN_RPM_MAX: u32 = 5400;

// _IOC(_IOC_READ|_IOC_WRITE, 'H', nr, len) from <linux/hidraw.h>
const fn hid_ioc(nr: u64, len: usize) -> u64 {
    (3u64 << 30) | ((len as u64) << 16) | ((b'H' as u64) << 8) | nr
}
const HIDIOCSFEATURE: u64 = hid_ioc(0x06, REPORT_LEN);
const HIDIOCGFEATURE: u64 = hid_ioc(0x07, REPORT_LEN);

/// A Razer command packet, laid out with the report ID at byte 0.
#[derive(Clone, Debug)]
pub struct Packet(pub [u8; REPORT_LEN]);

impl Packet {
    pub fn new(class: u8, id: u8, data_size: u8) -> Self {
        let mut b = [0u8; REPORT_LEN];
        b[2] = 0x1f; // transaction id
        b[6] = data_size;
        b[7] = class;
        b[8] = id;
        Self(b)
    }

    pub fn arg(mut self, i: usize, v: u8) -> Self {
        self.0[9 + i] = v;
        self
    }

    /// XOR of bytes 2..88 of the Razer report (3..89 here, shifted by report ID).
    pub fn finalize(&mut self) {
        let crc = self.0[3..89].iter().fold(0u8, |a, b| a ^ b);
        self.0[89] = crc;
    }
}

#[derive(Clone, Debug)]
pub struct Response(pub [u8; REPORT_LEN]);

impl Response {
    pub fn status(&self) -> u8 {
        self.0[1]
    }
    pub fn arg(&self, i: usize) -> u8 {
        self.0[9 + i]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KbMode {
    Off,
    Wave,
    Reactive,
    Spectrum,
    Static,
    Starlight,
}

impl KbMode {
    pub const ALL: [KbMode; 6] = [
        KbMode::Off,
        KbMode::Static,
        KbMode::Spectrum,
        KbMode::Wave,
        KbMode::Reactive,
        KbMode::Starlight,
    ];

    pub fn code(self) -> u8 {
        match self {
            KbMode::Off => 0x00,
            KbMode::Wave => 0x01,
            KbMode::Reactive => 0x02,
            KbMode::Spectrum => 0x04,
            KbMode::Static => 0x06,
            KbMode::Starlight => 0x19,
        }
    }

    pub fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            0x00 => KbMode::Off,
            0x01 => KbMode::Wave,
            0x02 => KbMode::Reactive,
            0x04 => KbMode::Spectrum,
            0x06 => KbMode::Static,
            0x07 | 0x19 => KbMode::Starlight,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            KbMode::Off => "Off",
            KbMode::Wave => "Wave",
            KbMode::Reactive => "Reactive",
            KbMode::Spectrum => "Spectrum",
            KbMode::Static => "Static",
            KbMode::Starlight => "Starlight",
        }
    }

    pub fn uses_color(self) -> bool {
        matches!(self, KbMode::Static | KbMode::Reactive | KbMode::Starlight)
    }

    pub fn uses_speed(self) -> bool {
        matches!(self, KbMode::Reactive | KbMode::Starlight)
    }

    pub fn uses_direction(self) -> bool {
        matches!(self, KbMode::Wave)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyboard {
    pub mode: KbMode,
    pub rgb: [u8; 3],
    /// 1..=15
    pub speed: u8,
    pub wave_right: bool,
}

impl Default for Keyboard {
    fn default() -> Self {
        Self {
            mode: KbMode::Spectrum,
            rgb: [0x44, 0xd6, 0x2c],
            speed: 2,
            wave_right: false,
        }
    }
}

/// Static facts about the laptop, read once when the device is opened.
#[derive(Clone, Debug, Default)]
pub struct DeviceInfo {
    /// Marketing name from DMI, e.g. "Razer Blade 14".
    pub model: Option<String>,
    /// Razer part number from DMI, e.g. "RZ09-0370BEA3".
    pub sku: Option<String>,
    pub bios: Option<String>,
    /// Embedded controller firmware (major, minor).
    pub firmware: Option<(u8, u8)>,
    pub pid: u16,
    pub path: PathBuf,
}

impl DeviceInfo {
    fn read_dmi(&mut self) {
        let dmi = |f: &str| {
            std::fs::read_to_string(Path::new("/sys/class/dmi/id").join(f))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        if !dmi("sys_vendor").is_some_and(|v| v.starts_with("Razer")) {
            return;
        }
        // product_name looks like "Blade 14 - RZ09-0370"
        if let Some(name) = dmi("product_name") {
            let (model, code) = match name.split_once(" - ") {
                Some((m, c)) => (m.trim().to_string(), Some(c.trim().to_string())),
                None => (name, None),
            };
            self.model = Some(if model.starts_with("Razer") {
                model
            } else {
                format!("Razer {model}")
            });
            self.sku = dmi("product_sku").or(code);
        }
        self.bios = dmi("bios_version");
    }
}

/// Everything the applet shows. `None` = the laptop didn't answer that query.
#[derive(Clone, Debug, Default)]
pub struct State {
    pub info: DeviceInfo,
    pub power_mode: Option<u8>,
    pub fan_manual: bool,
    pub fan_rpm: Option<u32>,
    pub cpu_level: Option<u8>,
    pub gpu_level: Option<u8>,
    pub keyboard: Option<Keyboard>,
    pub logo_on: Option<bool>,
    pub logo_breathing: bool,
    pub battery_limit_on: Option<bool>,
    pub battery_limit: u8,
}

#[derive(Debug)]
pub enum OpenError {
    NotFound,
    PermissionDenied(PathBuf),
    NoResponse,
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::NotFound => write!(f, "No supported Razer laptop found."),
            OpenError::PermissionDenied(p) => write!(
                f,
                "No permission to open {}. Install the udev rule (just install-udev), then log out and back in.",
                p.display()
            ),
            OpenError::NoResponse => write!(f, "Razer device found but it didn't respond."),
        }
    }
}

/// Some laptops read back the wave effect without its direction, so the last
/// direction set is kept in a file that every applet instance can see.
fn wave_file() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))?;
    Some(state.join("cosmic-ext-applet-razer/wave-direction"))
}

fn remembered_wave_right() -> bool {
    wave_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_some_and(|s| s.trim() == "right")
}

fn remember_wave_right(right: bool) {
    let Some(path) = wave_file() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, if right { "right\n" } else { "left\n" });
}

pub struct Razer {
    file: File,
    lock: Option<File>,
    pub info: DeviceInfo,
}

/// The panel runs one copy of the applet per monitor, and they all share the
/// laptop's single request/response slot. Whole exchanges are serialized with
/// a lock file so one copy can't read another's reply.
fn open_lock() -> Option<File> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("cosmic-ext-applet-razer.lock"))
        .ok()
}

/// USB product ID of a /dev/hidrawN node, if it belongs to a Razer device.
fn hidraw_pid(path: &Path) -> Option<u16> {
    let name = path.file_name()?;
    let uevent = Path::new("/sys/class/hidraw")
        .join(name)
        .join("device/uevent");
    let (vid, pid) = parse_hid_id(&std::fs::read_to_string(uevent).ok()?)?;
    (vid == RAZER_VENDOR).then_some(pid)
}

/// List /dev/hidrawN nodes belonging to supported Razer laptops.
pub fn candidates() -> Vec<(PathBuf, u16)> {
    if let Ok(p) = std::env::var("RAZER_HIDRAW") {
        let path = PathBuf::from(p);
        let pid = hidraw_pid(&path).unwrap_or(0);
        return vec![(path, pid)];
    }
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/hidraw") else {
        return out;
    };
    for e in entries.flatten() {
        let uevent = e.path().join("device/uevent");
        let Ok(text) = std::fs::read_to_string(&uevent) else {
            continue;
        };
        let Some((vid, pid)) = parse_hid_id(&text) else {
            continue;
        };
        if vid == RAZER_VENDOR && LAPTOP_PIDS.contains(&pid) {
            out.push((Path::new("/dev").join(e.file_name()), pid));
        }
    }
    out.sort();
    out
}

/// Parses `HID_ID=0003:00001532:0000029F` out of a uevent file.
pub fn parse_hid_id(uevent: &str) -> Option<(u16, u16)> {
    let line = uevent.lines().find_map(|l| l.strip_prefix("HID_ID="))?;
    let mut parts = line.split(':');
    let _bus = parts.next()?;
    let vid = u32::from_str_radix(parts.next()?, 16).ok()?;
    let pid = u32::from_str_radix(parts.next()?, 16).ok()?;
    Some((vid as u16, pid as u16))
}

impl Razer {
    /// Find the laptop's control interface (the one that answers feature reports).
    pub fn open() -> Result<Self, OpenError> {
        let cands = candidates();
        if cands.is_empty() {
            return Err(OpenError::NotFound);
        }
        let mut denied = None;
        let mut fallback = None;
        let lock = open_lock();
        for (path, pid) in cands {
            let file = match OpenOptions::new().read(true).write(true).open(&path) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                    denied.get_or_insert(path);
                    continue;
                }
                Err(_) => continue,
            };
            let info = DeviceInfo {
                pid,
                path,
                ..Default::default()
            };
            let lock = lock.as_ref().and_then(|l| l.try_clone().ok());
            let mut dev = Razer { file, lock, info };
            // Same probe RazerControl uses: ask for the keyboard lighting mode.
            match dev.transact(Packet::new(CLASS_LIGHTING, 0x8a, 0x50)) {
                Ok(r) if r.status() == STATUS_OK => return Ok(dev.with_info()),
                Ok(_) => {
                    fallback.get_or_insert(dev);
                }
                Err(_) => {}
            }
        }
        if let Some(dev) = fallback {
            return Ok(dev.with_info());
        }
        match denied {
            Some(p) => Err(OpenError::PermissionDenied(p)),
            None => Err(OpenError::NoResponse),
        }
    }

    fn with_info(mut self) -> Self {
        self.info.read_dmi();
        self.info.firmware = self.get_firmware().ok();
        self
    }

    pub fn transact(&mut self, mut pkt: Packet) -> io::Result<Response> {
        pkt.finalize();
        let lock = self.lock.as_ref().map(|l| l.as_raw_fd());
        if let Some(fd) = lock {
            // SAFETY: plain advisory lock on a file we own; released below.
            unsafe { libc::flock(fd, libc::LOCK_EX) };
        }
        // Retry if the reply belongs to some other command, e.g. one sent by a
        // tool that doesn't take the lock.
        let mut result = self.exchange(&mut pkt);
        for _ in 0..2 {
            match &result {
                Err(e) if e.kind() == io::ErrorKind::TimedOut => result = self.exchange(&mut pkt),
                _ => break,
            }
        }
        if let Some(fd) = lock {
            // SAFETY: as above.
            unsafe { libc::flock(fd, libc::LOCK_UN) };
        }
        result
    }

    fn exchange(&mut self, pkt: &mut Packet) -> io::Result<Response> {
        let fd = self.file.as_raw_fd();
        // SAFETY: buffer is REPORT_LEN bytes, matching the length encoded in the ioctl.
        let rc = unsafe { libc::ioctl(fd, HIDIOCSFEATURE as _, pkt.0.as_mut_ptr()) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = [0u8; REPORT_LEN];
        let answers = |buf: &[u8; REPORT_LEN]| buf[7..9] == pkt.0[7..9];
        for _ in 0..12 {
            std::thread::sleep(Duration::from_millis(8));
            buf = [0u8; REPORT_LEN];
            // SAFETY: as above.
            let rc = unsafe { libc::ioctl(fd, HIDIOCGFEATURE as _, buf.as_mut_ptr()) };
            if rc < 0 {
                return Err(io::Error::last_os_error());
            }
            if buf[1] != STATUS_BUSY && answers(&buf) {
                break;
            }
        }
        if !answers(&buf) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "reply was for another command",
            ));
        }
        match buf[1] {
            STATUS_FAIL => Err(io::Error::other("command failed")),
            STATUS_UNSUPPORTED => Err(io::Error::new(io::ErrorKind::Unsupported, "not supported")),
            _ => Ok(Response(buf)),
        }
    }

    // ---- device -----------------------------------------------------------

    pub fn get_firmware(&mut self) -> io::Result<(u8, u8)> {
        let r = self.transact(Packet::new(CLASS_DEVICE, 0x81, 0x02))?;
        Ok((r.arg(0), r.arg(1)))
    }

    // ---- performance / fan -------------------------------------------------

    /// Returns (power mode, manual fan enabled).
    pub fn get_power(&mut self) -> io::Result<(u8, bool)> {
        let r = self.transact(Packet::new(CLASS_PERFORMANCE, 0x82, 0x04).arg(1, ZONE_CPU))?;
        Ok((r.arg(2), r.arg(3) == 1))
    }

    pub fn set_power(&mut self, mode: u8, fan_manual: bool) -> io::Result<()> {
        self.transact(
            Packet::new(CLASS_PERFORMANCE, 0x02, 0x04)
                .arg(1, ZONE_CPU)
                .arg(2, mode)
                .arg(3, fan_manual as u8),
        )?;
        Ok(())
    }

    /// Custom-mode level: 0 Low, 1 Medium, 2 High, 3 Boost (CPU only).
    pub fn get_level(&mut self, zone: u8) -> io::Result<u8> {
        let r = self.transact(Packet::new(CLASS_PERFORMANCE, 0x87, 0x03).arg(1, zone))?;
        Ok(r.arg(2))
    }

    pub fn set_level(&mut self, zone: u8, level: u8) -> io::Result<()> {
        self.transact(
            Packet::new(CLASS_PERFORMANCE, 0x07, 0x03)
                .arg(1, zone)
                .arg(2, level),
        )?;
        Ok(())
    }

    pub fn get_fan_rpm(&mut self) -> io::Result<u32> {
        let r = self.transact(Packet::new(CLASS_PERFORMANCE, 0x81, 0x03).arg(1, ZONE_CPU))?;
        Ok(r.arg(2) as u32 * 100)
    }

    pub fn set_fan_rpm(&mut self, rpm: u32) -> io::Result<()> {
        let v = (rpm.clamp(FAN_RPM_MIN, FAN_RPM_MAX) / 100) as u8;
        for zone in [ZONE_CPU, ZONE_GPU] {
            self.transact(
                Packet::new(CLASS_PERFORMANCE, 0x01, 0x03)
                    .arg(1, zone)
                    .arg(2, v),
            )?;
        }
        Ok(())
    }

    // ---- keyboard ---------------------------------------------------------

    pub fn get_keyboard(&mut self) -> io::Result<Keyboard> {
        let r = self.transact(Packet::new(CLASS_LIGHTING, 0x8a, 0x50))?;
        let mut kb = Keyboard {
            mode: KbMode::from_code(r.arg(0)).unwrap_or(KbMode::Spectrum),
            ..Default::default()
        };
        match kb.mode {
            KbMode::Starlight => {
                kb.speed = r.arg(2);
                kb.rgb = [r.arg(3), r.arg(4), r.arg(5)];
            }
            KbMode::Reactive => {
                kb.speed = r.arg(1);
                kb.rgb = [r.arg(2), r.arg(3), r.arg(4)];
            }
            KbMode::Static => kb.rgb = [r.arg(1), r.arg(2), r.arg(3)],
            KbMode::Wave => {
                kb.wave_right = match r.arg(1) {
                    0x01 => false,
                    0x02 => true,
                    // Not reported (e.g. Blade 14 2021): use the last one we set.
                    _ => remembered_wave_right(),
                }
            }
            _ => {}
        }
        kb.speed = kb.speed.clamp(1, 15);
        Ok(kb)
    }

    pub fn set_keyboard(&mut self, kb: &Keyboard) -> io::Result<()> {
        let [r, g, b] = kb.rgb;
        let speed = kb.speed.clamp(1, 15);
        let p = Packet::new(CLASS_LIGHTING, 0x0a, 0x50).arg(0, kb.mode.code());
        let p = match kb.mode {
            KbMode::Wave => p.arg(1, if kb.wave_right { 0x02 } else { 0x01 }),
            KbMode::Reactive => p.arg(1, speed).arg(2, r).arg(3, g).arg(4, b),
            KbMode::Static => p.arg(1, r).arg(2, g).arg(3, b),
            KbMode::Starlight => p.arg(1, 0x01).arg(2, speed).arg(3, r).arg(4, g).arg(5, b),
            KbMode::Off | KbMode::Spectrum => p,
        };
        self.transact(p)?;
        if kb.mode == KbMode::Wave {
            remember_wave_right(kb.wave_right);
        }
        Ok(())
    }

    // ---- logo -------------------------------------------------------------

    pub fn get_logo(&mut self) -> io::Result<(bool, bool)> {
        let on = self.transact(Packet::new(CLASS_LIGHTING, 0x80, 0x03).arg(0, 1).arg(1, 4))?;
        let mode = self.transact(Packet::new(CLASS_LIGHTING, 0x82, 0x03).arg(0, 1).arg(1, 4))?;
        Ok((on.arg(2) == 1, mode.arg(2) == 0x02))
    }

    pub fn set_logo_on(&mut self, on: bool) -> io::Result<()> {
        self.transact(
            Packet::new(CLASS_LIGHTING, 0x00, 0x03)
                .arg(0, 1)
                .arg(1, 4)
                .arg(2, on as u8),
        )?;
        Ok(())
    }

    pub fn set_logo_breathing(&mut self, breathing: bool) -> io::Result<()> {
        let m = if breathing { 0x02 } else { 0x00 };
        self.transact(
            Packet::new(CLASS_LIGHTING, 0x02, 0x03)
                .arg(0, 1)
                .arg(1, 4)
                .arg(2, m),
        )?;
        Ok(())
    }

    // ---- battery health optimizer -----------------------------------------

    pub fn get_battery_limit(&mut self) -> io::Result<(bool, u8)> {
        let r = self.transact(Packet::new(CLASS_BATTERY, 0x92, 0x01))?;
        let b = r.arg(0);
        Ok((b & 0x80 != 0, (b & 0x7f).clamp(50, 80)))
    }

    pub fn set_battery_limit(&mut self, on: bool, level: u8) -> io::Result<()> {
        let level = level.clamp(50, 80);
        let b = if on { level | 0x80 } else { level };
        self.transact(Packet::new(CLASS_BATTERY, 0x12, 0x01).arg(0, b))?;
        Ok(())
    }

    /// Read everything. Individual failures (e.g. a model with no logo LED)
    /// just leave that field empty; only a dead device is an error.
    pub fn read_state(&mut self) -> io::Result<State> {
        let mut s = State {
            info: self.info.clone(),
            ..Default::default()
        };
        let (mode, manual) = self.get_power()?;
        s.power_mode = Some(mode);
        s.fan_manual = manual;
        s.fan_rpm = self.get_fan_rpm().ok();
        s.cpu_level = self.get_level(ZONE_CPU).ok();
        s.gpu_level = self.get_level(ZONE_GPU).ok();
        s.keyboard = self.get_keyboard().ok();
        if let Ok((on, breathing)) = self.get_logo() {
            s.logo_on = Some(on);
            s.logo_breathing = breathing;
        }
        if let Ok((on, level)) = self.get_battery_limit() {
            s.battery_limit_on = Some(on);
            s.battery_limit = level;
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_kernel_headers() {
        // HIDIOCSFEATURE(91) / HIDIOCGFEATURE(91) as computed by <linux/hidraw.h>
        assert_eq!(HIDIOCSFEATURE, 0xC05B4806);
        assert_eq!(HIDIOCGFEATURE, 0xC05B4807);
    }

    #[test]
    fn packet_layout_matches_razercontrol() {
        // SetCustomPerformancePacket(GPU, High) from RazerControl
        let mut p = Packet::new(CLASS_PERFORMANCE, 0x07, 0x03)
            .arg(1, ZONE_GPU)
            .arg(2, 2);
        p.finalize();
        assert_eq!(
            &p.0[..12],
            &[0x00, 0x00, 0x1f, 0, 0, 0, 0x03, 0x0d, 0x07, 0x00, 0x02, 0x02]
        );
        assert_eq!(p.0[89], 0x03 ^ 0x0d ^ 0x07 ^ 0x02 ^ 0x02);
        assert_eq!(p.0[90], 0);
    }

    #[test]
    fn parses_uevent() {
        let u = "DRIVER=hid-generic\nHID_ID=0003:00001532:0000029F\nHID_NAME=Razer Blade\n";
        assert_eq!(parse_hid_id(u), Some((0x1532, 0x029f)));
    }

    #[test]
    fn keyboard_modes_roundtrip() {
        for m in KbMode::ALL {
            assert_eq!(KbMode::from_code(m.code()), Some(m));
        }
    }
}
