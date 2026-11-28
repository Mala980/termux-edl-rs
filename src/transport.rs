//! USB transport (Linux/Android via rusb/libusb) + serial (Linux tty) — port of usblib.py/seriallib.py.
use crate::config_tables::{DEFAULT_IDS, MAX_USB_BULK_BUFFER_SIZE};
use anyhow::{anyhow, Result};
use std::time::Duration;
use rusb::UsbContext;
use libusb1_sys as libusb1_sys;

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
            let mut tries = 0;
            loop {
                match h.write_bulk(ep, chunk, Duration::from_secs(5)) {
                    Ok(n) => { total += n; break; }
                    Err(rusb::Error::Timeout) => {
                        tries += 1;
                        if tries >= 3 { return Ok(total); }
                    }
                    Err(e) => return Err(e.into()),
                }
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



/// Raw libusb context with NO_DEVICE_DISCOVERY enabled (required on Android,
/// mirrors LibUsbCtx.get_ctx() in termux_usblib.py which sets option 2).
#[derive(Clone)]
pub struct WeakCtx(*mut libusb1_sys::libusb_context);

unsafe impl Send for WeakCtx {}
unsafe impl Sync for WeakCtx {}

impl rusb::UsbContext for WeakCtx {
    fn as_raw(&self) -> *mut libusb1_sys::libusb_context {
        self.0
    }
}

impl WeakCtx {
    // NOTE: no Drop/libusb_exit — rusb clones the context into every
    // DeviceHandle, and exiting libusb per clone would use-after-free
    // (segfault). The context lives for the process lifetime, same as
    // LibUsbCtx in termux_usblib.py.
    pub fn new_android() -> Result<Self> {
        unsafe {
            libusb1_sys::libusb_set_option(
                std::ptr::null_mut(),
                libusb1_sys::constants::LIBUSB_OPTION_NO_DEVICE_DISCOVERY,
            );
        }
        let mut ctx: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
        let ret = unsafe { libusb1_sys::libusb_init(&mut ctx) };
        if ret != 0 {
            anyhow::bail!("libusb_init failed: {ret}");
        }
        Ok(WeakCtx(ctx))
    }
}

/// Termux USB transport: uses `termux-usb -E -r <node>` to obtain an usbfs FD
/// (fd-passing over a unix socketpair) and wraps it with
/// libusb_wrap_sys_device — exactly like termux_usblib.py. Required on
/// Android where direct /dev/bus/usb access is blocked by SELinux.
pub struct TermuxTransport {
    handle: Option<rusb::DeviceHandle<WeakCtx>>,
    fd: i32,
    ep_out: u8,
    ep_in: u8,
    maxsize: usize,
}

extern "C" {
    fn socketpair(domain: i32, typ: i32, protocol: i32, sv: *mut i32) -> i32;
    fn sendmsg(fd: i32, msg: *const Msghdr, flags: i32) -> isize;
    fn recvmsg(fd: i32, msg: *mut Msghdr, flags: i32) -> isize;
    fn fcntl(fd: i32, cmd: i32, arg: i32) -> i32;
}

#[repr(C)]
struct IOVec {
    iov_base: *mut u8,
    iov_len: usize,
}

#[repr(C)]
struct Cmsghdr {
    cmsg_len: usize,
    cmsg_level: i32,
    cmsg_type: i32,
    // fd payload follows
}

#[repr(C)]
struct Msghdr {
    msg_name: *mut u8,
    msg_namelen: u32,
    msg_iov: *mut IOVec,
    msg_iovlen: usize,
    msg_control: *mut u8,
    msg_controllen: usize,
    msg_flags: i32,
}

const AF_UNIX: i32 = 1;
const SOCK_STREAM: i32 = 1;
const SOL_SOCKET: i32 = 1;
const SCM_RIGHTS: i32 = 1;
const F_GETFD: i32 = 1;
const F_SETFD: i32 = 2;
const FD_CLOEXEC: i32 = 1;

impl TermuxTransport {
    fn termux_usb_cmd() -> String {
        std::env::var("TERMUX_USB_CMD")
            .unwrap_or_else(|_| "/data/data/com.termux/files/usr/bin/termux-usb".into())
    }

    /// `termux-usb -l` -> list of device nodes like /dev/bus/usb/001/050.
    pub fn list_nodes() -> Vec<String> {
        let out = std::process::Command::new(Self::termux_usb_cmd())
            .arg("-l")
            .output();
        if let Ok(o) = out {
            if let Ok(txt) = String::from_utf8(o.stdout) {
                if let Ok(v) = serde_json_fallback(&txt) {
                    return v;
                }
            }
        }
        Vec::new()
    }

    /// Run `termux-usb -E -r <node>` with SCM_RIGHTS fd passing and return the FD.
    fn open_node_fd(node: &str) -> Result<i32> {
        use std::os::unix::net::UnixStream;
        use std::os::unix::io::AsRawFd;
        let (a, b) = UnixStream::pair()?;
        // Clear CLOEXEC on the child end.
        unsafe {
            let flags = fcntl(b.as_raw_fd(), F_GETFD, 0);
            fcntl(b.as_raw_fd(), F_SETFD, flags & !FD_CLOEXEC);
        }
        // Write a tiny shell/python helper via termux-usb -e <cmd>.
        // termux-usb -e expects a program path; use /system/bin/sh script that
        // passes the fd via python? Termux python may not exist; instead use
        // `termux-usb -e "sh -c ..."`. Actually -e runs a single command; we
        // build a helper shell script in TMPDIR that uses `juicy-run`-style
        // fd passing via... shell cannot sendmsg. Termux python3 IS used by
        // the original tool; replicate that: create python helper if python
        // exists, else fall back to `termux-usb -E -r` which prints fd? It
        // doesn't. So: prefer python3 helper (same as python impl).
        let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/data/data/com.termux/files/usr/tmp".into());
        let helper = format!("{tmp}/edl_fd_helper_rs.py");
        let script = format!(
            "#!/data/data/com.termux/files/usr/bin/python3\nimport os, socket, struct\ns=int(os.environ[\"EDL_SOCK_FD\"])\nu=int(os.environ[\"TERMUX_USB_FD\"])\nsock=socket.fromfd(s,socket.AF_UNIX,socket.SOCK_STREAM)\nsock.sendmsg([b\"1\"],[(socket.SOL_SOCKET,socket.SCM_RIGHTS,struct.pack(\"i\",u))])\nsock.close()\n"
        );
        std::fs::write(&helper, script)?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755));

        let status = std::process::Command::new(Self::termux_usb_cmd())
            .args(["-e", &helper, "-E", "-r", node])
            .env("EDL_SOCK_FD", b.as_raw_fd().to_string())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?
            .wait()?;
        drop(b);
        if !status.success() {
            anyhow::bail!("termux-usb failed for {node}");
        }
        // Receive FD over the socketpair.
        let fd = unsafe { receive_fd(a.as_raw_fd()) }?;
        Ok(fd)
    }

    /// Open a Termux device node and wrap the FD with libusb.
    pub fn connect_node(node: &str) -> Result<Self> {
        let fd = Self::open_node_fd(node)?;
        let ctx = WeakCtx::new_android()?;
        let handle = unsafe { ctx.open_device_with_fd(fd) }
            .map_err(|e| anyhow!("libusb_wrap_sys_device({node}) failed: {e}"))?;
        Ok(Self { handle: Some(handle), fd, ep_out: 0x01, ep_in: 0x81, maxsize: 512 })
    }

    /// VID/PID of the wrapped device (for identity checks).
    pub fn device_descriptor(&self) -> Result<rusb::DeviceDescriptor> {
        let h = self.handle.as_ref().ok_or_else(|| anyhow!("not connected"))?;
        Ok(h.device().device_descriptor()?)
    }

    fn discover_endpoint_defaults(&mut self, vid: u16, pid: u16) {
        // Try to read real endpoints from the wrapped handle; on failure keep
        // 0x01/0x81 defaults (same as python's fallback).
        let Some(h) = &self.handle else { return };
        let Ok(desc) = h.device().config_descriptor(0) else { return };
        let mut ep_out = None;
        let mut ep_in = None;
        let mut maxsize = 512u16;
        let count = desc.interfaces().count();
        for iface in desc.interfaces() {
            for setting in iface.descriptors() {
                if setting.class_code() != 0xFF && count > 1 {
                    continue;
                }
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
            if ep_out.is_some() && ep_in.is_some() { break; }
        }
        if let Some(o) = ep_out { self.ep_out = o; }
        if let Some(i) = ep_in { self.ep_in = i; }
        self.maxsize = maxsize as usize;
        let _ = vid; let _ = pid;
    }
}

/// Minimal JSON string-array parser for `termux-usb -l` output.
fn serde_json_fallback(txt: &str) -> Result<Vec<String>> {
    // Parse `["/dev/bus/usb/001/050", ...]`
    let t = txt.trim();
    let inner = t.strip_prefix('[').and_then(|x| x.strip_suffix(']')).ok_or_else(|| anyhow!("not a json array"))?;
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    let mut esc = false;
    for c in inner.chars() {
        if in_str {
            if esc { cur.push(c); esc = false; }
            else if c == '\\' { esc = true; }
            else if c == '"' { in_str = false; out.push(std::mem::take(&mut cur)); }
            else { cur.push(c); }
        } else if c == '"' {
            in_str = true;
        }
    }
    Ok(out)
}

unsafe fn receive_fd(sock_fd: i32) -> Result<i32> {
    let mut byte = [0u8; 1];
    let mut iov = IOVec { iov_base: byte.as_mut_ptr(), iov_len: 1 };
    let mut cmsg_buf = [0u8; 64]; // cmsghdr + int fd
    let mut msg = Msghdr {
        msg_name: std::ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: &mut iov,
        msg_iovlen: 1,
        msg_control: cmsg_buf.as_mut_ptr(),
        msg_controllen: cmsg_buf.len(),
        msg_flags: 0,
    };
    let n = recvmsg(sock_fd, &mut msg, 0);
    if n < 1 {
        anyhow::bail!("no fd received from termux-usb helper");
    }
    // Walk control messages.
    let mut pos = 0usize;
    while pos + std::mem::size_of::<Cmsghdr>() <= msg.msg_controllen {
        let cmsg = &*(cmsg_buf.as_ptr().add(pos) as *const Cmsghdr);
        if cmsg.cmsg_level == SOL_SOCKET && cmsg.cmsg_type == SCM_RIGHTS {
            let data_start = pos + std::mem::size_of::<Cmsghdr>();
            let fd_bytes = [cmsg_buf[data_start], cmsg_buf[data_start+1], cmsg_buf[data_start+2], cmsg_buf[data_start+3]];
            let fd = i32::from_ne_bytes(fd_bytes);
            return Ok(fd);
        }
        // advance to next cmsg (aligned to size_t)
        pos += (cmsg.cmsg_len + 7) & !7;
    }
    anyhow::bail!("SCM_RIGHTS not found in control message");
}

impl Transport for TermuxTransport {
    fn connect(&mut self, vid: u16, pid: u16) -> Result<()> {
        // Discover endpoints for the already-opened handle.
        if self.handle.is_none() {
            anyhow::bail!("Termux transport not opened");
        }
        self.discover_endpoint_defaults(vid, pid);
        // Claim interface if possible (non-fatal, like python). Python's
        // termux backend detaches a kernel driver only if one is ACTIVE
        // before claiming; never use the blanket auto-detach ioctl.
        if let Some(h) = &mut self.handle {
            let _ = h.claim_interface(0);
        }
        Ok(())
    }

    fn close(&mut self) {
        self.handle = None;
        if self.fd > 0 {
            unsafe { libc::close(self.fd) };
            self.fd = 0;
        }
    }

    fn write(&mut self, data: &[u8]) -> Result<usize> {
        let h = self.handle.as_mut().ok_or_else(|| anyhow!("not connected"))?;
        let mut total = 0usize;
        // Python parity (termux_usblib.py write): a failing chunk is retried
        // at most 3 times, then the write is abandoned — an endless retry
        // loop hammers usbfs and has been observed to reboot the phone.
        for chunk in data.chunks(MAX_USB_BULK_BUFFER_SIZE) {
            let mut tries = 0;
            loop {
                match h.write_bulk(self.ep_out, chunk, Duration::from_secs(5)) {
                    Ok(n) => {
                        total += n;
                        break;
                    }
                    Err(_) => {
                        tries += 1;
                        if tries >= 3 {
                            return Ok(total);
                        }
                    }
                }
            }
        }
        Ok(total)
    }

    fn read(&mut self, resplen: usize, timeout_ms: u32) -> Result<Vec<u8>> {
        let h = self.handle.as_mut().ok_or_else(|| anyhow!("not connected"))?;
        let mut res = Vec::with_capacity(resplen);
        let timeout = Duration::from_millis(timeout_ms.max(1) as u64);
        while res.len() < resplen {
            // 16 KB per transfer, same as python usbread (bufsize=min(resplen,16384)).
            let want = (resplen - res.len()).min(16384);
            let mut buf = vec![0u8; want];
            match h.read_bulk(self.ep_in, &mut buf, timeout) {
                Ok(0) => break,
                Ok(n) => {
                    res.extend_from_slice(&buf[..n]);
                    if n < want { break; }
                }
                Err(rusb::Error::Timeout) => return Ok(res),
                Err(e) => {
                    if res.is_empty() { return Err(e.into()); }
                    return Ok(res);
                }
            }
        }
        Ok(res)
    }

    fn max_packet(&self) -> usize { MAX_USB_BULK_BUFFER_SIZE }
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
