//! USB transport (Linux/Android via rusb/libusb) + serial (Linux tty) — port of usblib.py/seriallib.py.
use crate::config_tables::{DEFAULT_IDS, MAX_USB_BULK_BUFFER_SIZE};
use anyhow::{anyhow, Result};
use std::time::Duration;

pub trait Transport: Send {
    fn connect(&mut self, vid: u16, pid: u16) -> Result<()>;
    fn close(&mut self);
    fn write(&mut self, data: &[u8]) -> Result<usize>;
    fn read(&mut self, resplen: usize, timeout_ms: u32) -> Result<Vec<u8>>;
    fn max_packet(&self) -> usize {
        MAX_USB_BULK_BUFFER_SIZE
    }
}

pub struct UsbTransport {
    dev: Option<rusb::DeviceHandle<rusb::GlobalContext>>,
    iface_num: Option<u8>,
    ep_out: Option<u8>,
    ep_in: Option<u8>,
    maxsize: usize,
}

impl UsbTransport {
    fn log(&self, msg: &str) {
        eprintln!("[LIB] {msg}");
    }
    pub fn new() -> Self {
        Self { dev: None, iface_num: None, ep_out: None, ep_in: None, maxsize: 512 }
    }

    /// List connected EDL devices matching the default VID/PID table.
    pub fn list_devices() -> Vec<(u16, u16)> {
        let mut out = Vec::new();
        if let Ok(ctx) = rusb::Context::new() {
            if let Ok(devices) = rusb::devices() {
                for d in devices.iter() {
                    if let Ok(desc) = d.device_descriptor() {
                        for (vid, pid, _) in DEFAULT_IDS {
                            if desc.vendor_id() == *vid && desc.product_id() == *pid {
                                out.push((*vid, *pid));
                                break;
                            }
                        }
                    }
                }
            }
        }
        out
    }
}

impl Transport for UsbTransport {
    fn connect(&mut self, vid: u16, pid: u16) -> Result<()> {
        self.close();
        let ctx = rusb::Context::new()?;
        let mut found = None;
        for d in rusb::devices()?.iter() {
            let desc = d.device_descriptor()?;
            if desc.vendor_id() == vid && desc.product_id() == pid {
                found = Some(d);
                break;
            }
        }
        let device = found.ok_or_else(|| anyhow!("No EDL device found ({vid:04x}:{pid:04x}). Is it connected?"))?;
        let mut handle = device.open()?;
        handle.set_auto_detach_kernel_driver(true).ok();
        let desc = device.config_descriptor(0)?;
        let mut ep_out = None;
        let mut ep_in = None;
        let mut maxsize = 512u16;
        let mut ifnum = 0u8;
        let count = desc.interfaces().count();
        for iface in desc.interfaces() {
            for setting in iface.descriptors() {
                if setting.class_code() != 0xFF && count > 1 {
                    continue;
                }
                ifnum = setting.interface_number();
                for ep in setting.endpoint_descriptors() {
                    match ep.direction() {
                        rusb::Direction::Out => ep_out = Some(ep.address()),
                        rusb::Direction::In => {
                            ep_in = Some(ep.address());
                            maxsize = ep.max_packet_size().max(512);
                        }
                    }
                }
                break;
            }
            if ep_out.is_some() && ep_in.is_some() {
                break;
            }
        }
        if ep_out.is_none() || ep_in.is_none() {
            // Same fallback as termux_usblib.py: default bulk endpoints.
            ep_out = Some(0x01);
            ep_in = Some(0x81);
            ifnum = 0;
            maxsize = 512;
        }
        // Claim, but don't hard-fail: python logs and continues (root/fuse
        // fd-passing may already have claimed it).
        let claim = handle.claim_interface(ifnum);
        if let Err(e) = claim {
            self.log(&format!("claim interface {ifnum}: {e} (continuing)"));
        }
        self.dev = Some(handle);
        self.iface_num = Some(ifnum);
        self.ep_out = ep_out;
        self.ep_in = ep_in;
        self.maxsize = maxsize as usize;
        Ok(())
    }

    fn close(&mut self) {
        if let Some(h) = &mut self.dev {
            let _ = h.release_interface(self.iface_num.unwrap_or(0));
        }
        self.dev = None;
        self.iface_num = None;
        self.ep_out = None;
        self.ep_in = None;
    }

    fn write(&mut self, data: &[u8]) -> Result<usize> {
        let ep = self.ep_out.ok_or_else(|| anyhow!("not connected"))?;
        let h = self.dev.as_mut().unwrap();
        let mut total = 0usize;
        let pkt = MAX_USB_BULK_BUFFER_SIZE;
        for chunk in data.chunks(pkt) {
            match h.write_bulk(ep, chunk, Duration::from_secs(10)) {
                Ok(n) => total += n,
                Err(rusb::Error::Timeout) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(total)
    }

    fn read(&mut self, resplen: usize, timeout_ms: u32) -> Result<Vec<u8>> {
        let ep = self.ep_in.ok_or_else(|| anyhow!("not connected"))?;
        let h = self.dev.as_mut().unwrap();
        let mut res = Vec::with_capacity(resplen);
        let timeout = Duration::from_millis(timeout_ms.max(1) as u64);
        let pkt = self.maxsize;
        while res.len() < resplen {
            let want = (resplen - res.len()).min(pkt.max(512));
            let mut buf = vec![0u8; want];
            match h.read_bulk(ep, &mut buf, timeout) {
                Ok(0) => break,
                Ok(n) => {
                    res.extend_from_slice(&buf[..n]);
                    if n < want {
                        break; // short packet ends transfer
                    }
                }
                Err(rusb::Error::Timeout) => return Ok(res),
                // Python usblib returns whatever it has on other errors
                // instead of aborting the session.
                Err(e) => {
                    if res.is_empty() {
                        return Err(e.into());
                    }
                    return Ok(res);
                }
            }
        }
        Ok(res)
    }
}

/// Serial transport for /dev/ttyUSB-style diag ports (Linux/Android).
pub struct SerialTransport {
    fd: i32,
    path: String,
}

impl SerialTransport {
    pub fn available_ports() -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir("/dev") {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with("ttyUSB") || name.starts_with("ttyACM") || name.starts_with("ttyHS") {
                    out.push(format!("/dev/{name}"));
                }
            }
        }
        out.sort();
        out
    }

    pub fn open(path: &str) -> Result<Self> {
        use std::io::{Read as _, Write as _};
use std::os::unix::io::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(path)?;
        let fd = f.as_raw_fd();
        std::mem::forget(f); // keep the fd open for the process lifetime
        Ok(Self { fd, path: path.to_string() })
    }

    fn set_raw(&self) {
        let mut tio: libc::termios = unsafe { std::mem::zeroed() };
        unsafe {
            if libc::tcgetattr(self.fd, &mut tio) == 0 {
                libc::cfmakeraw(&mut tio);
                tio.c_cc[libc::VMIN] = 0;
                tio.c_cc[libc::VTIME] = 1;
                libc::tcsetattr(self.fd, libc::TCSANOW, &tio);
            }
        }
    }
}

impl Transport for SerialTransport {
    fn connect(&mut self, _vid: u16, _pid: u16) -> Result<()> {
        self.set_raw();
        Ok(())
    }

    fn close(&mut self) {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
            self.fd = -1;
        }
    }

    fn write(&mut self, data: &[u8]) -> Result<usize> {
        let mut written = 0usize;
        while written < data.len() {
            let n = unsafe { libc::write(self.fd, data[written..].as_ptr() as *const libc::c_void, data.len() - written) };
            if n <= 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                return Err(anyhow!("serial write failed on {}", self.path));
            }
            written += n as usize;
        }
        Ok(written)
    }

    fn read(&mut self, resplen: usize, timeout_ms: u32) -> Result<Vec<u8>> {
        let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms.max(1) as u64);
        let mut res = Vec::new();
        loop {
            let mut buf = vec![0u8; resplen.min(65536)];
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n > 0 {
                res.extend_from_slice(&buf[..n as usize]);
                if res.len() >= resplen {
                    break;
                }
            } else if n == 0 || std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                if std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            } else {
                break;
            }
        }
        Ok(res)
    }
}
