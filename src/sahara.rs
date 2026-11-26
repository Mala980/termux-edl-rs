//! Sahara protocol — port of edlclient/Library/sahara.py + sahara_defs.py.
use crate::loader_db::LoaderDb;
use crate::transport::Transport;
use anyhow::{anyhow, Result};

pub mod defs {
    pub const SAHARA_VERSION: u32 = 2;
    pub mod cmd {
        pub const HELLO_REQ: u32 = 0x1;
        pub const HELLO_RSP: u32 = 0x2;
        pub const READ_DATA: u32 = 0x3;
        pub const END_TRANSFER: u32 = 0x4;
        pub const DONE_REQ: u32 = 0x5;
        pub const DONE_RSP: u32 = 0x6;
        pub const RESET_REQ: u32 = 0x7;
        pub const RESET_RSP: u32 = 0x8;
        pub const MEMORY_DEBUG: u32 = 0x9;
        pub const MEMORY_READ: u32 = 0xA;
        pub const CMD_READY: u32 = 0xB;
        pub const SWITCH_MODE: u32 = 0xC;
        pub const EXECUTE_REQ: u32 = 0xD;
        pub const EXECUTE_RSP: u32 = 0xE;
        pub const EXECUTE_DATA: u32 = 0xF;
        pub const MEMORY_READ_64: u32 = 0x12;
        pub const RESET_STATE_MACHINE_ID: u32 = 0x13;
    }
    pub mod mode {
        pub const IMAGE_TX_PENDING: u32 = 0x0;
        pub const IMAGE_TX_COMPLETE: u32 = 0x1;
        pub const MEMORY_DEBUG: u32 = 0x2;
        pub const COMMAND: u32 = 0x3;
    }
    pub mod exec {
        pub const NOP: u32 = 0x00;
        pub const SERIAL_NUM_READ: u32 = 0x01;
        pub const MSM_HW_ID_READ: u32 = 0x02;
        pub const OEM_PK_HASH_READ: u32 = 0x03;
        pub const READ_CHIP_ID_V3: u32 = 0x0A;
    }
    pub const SUCCESS: u32 = 0x00;
    pub const NAK_INVALID_CMD: u32 = 0x01;
}

fn rd_u32(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(d[off..off + 4].try_into().unwrap())
}

#[derive(Debug, Clone, Default)]
pub struct ReadDataPkt {
    pub image_id: u64,
    pub data_offset: u64,
    pub data_len: u64,
}

#[derive(Debug, Clone, Default)]
pub struct EndTransferPkt {
    pub image_tx_status: u32,
}

#[derive(Debug, Clone)]
pub enum SaharaPkt {
    Hello { version: u32, version_supported: u32, pktsize: u32, mode: u32 },
    ReadData(ReadDataPkt),
    EndTransfer(EndTransferPkt),
    Done,
    CmdReady,
    ResetRsp,
    ExecuteRsp { data_len: u32 },
    MemoryDebug { table_addr: u64, table_len: u64 },
    XmlFirehose,
    Empty,
}

#[derive(Debug, Clone, Default)]
pub struct SaharaInfo {
    pub serial: u32,
    pub hwid: Option<u64>,
    pub pkhash: Option<String>,
    pub hwidstr: Option<String>,
    pub msm_str: Option<String>,
    pub msm_id: Option<u32>,
    pub oem_str: Option<String>,
    pub model_str: Option<String>,
    pub bit64: bool,
    pub version: u32,
}

pub struct Sahara<'a> {
    pub cdc: &'a mut dyn Transport,
    pub info: SaharaInfo,
    pub programmer: Option<String>,
    pub mode: String,
    loglevel_debug: bool,
}

impl<'a> Sahara<'a> {
    pub fn new(cdc: &'a mut dyn Transport, debug: bool) -> Self {
        Self {
            cdc,
            info: SaharaInfo { version: 2, ..Default::default() },
            programmer: None,
            mode: String::new(),
            loglevel_debug: debug,
        }
    }

    fn dbg(&self, s: &str) {
        if self.loglevel_debug {
            println!("[sahara:dbg] {s}");
        }
    }

    fn err(&self, s: &str) {
        eprintln!("[sahara:err] {s}");
    }

    fn info(&self, s: &str) {
        println!("[sahara] {s}");
    }

    fn raw_read(&mut self, resplen: usize, timeout_ms: u32) -> Vec<u8> {
        self.cdc.read(resplen, timeout_ms).unwrap_or_default()
    }

    fn get_rsp(&mut self) -> SaharaPkt {
        let data = self.raw_read(0x40, 2000);
        if data.is_empty() {
            return SaharaPkt::Empty;
        }
        if data.windows(5).any(|w| w == b"<?xml") {
            return SaharaPkt::XmlFirehose;
        }
        if data.len() < 8 {
            return SaharaPkt::Empty;
        }
        match rd_u32(&data, 0) {
            defs::cmd::HELLO_REQ if data.len() >= 0x30 => SaharaPkt::Hello {
                version: rd_u32(&data, 8),
                version_supported: rd_u32(&data, 12),
                pktsize: rd_u32(&data, 16),
                mode: rd_u32(&data, 20),
            },
            defs::cmd::END_TRANSFER if data.len() >= 0x10 => {
                SaharaPkt::EndTransfer(EndTransferPkt { image_tx_status: rd_u32(&data, 12) })
            }
            defs::cmd::DONE_RSP => SaharaPkt::Done,
            defs::cmd::CMD_READY => SaharaPkt::CmdReady,
            defs::cmd::RESET_RSP => SaharaPkt::ResetRsp,
            defs::cmd::MEMORY_READ_64 if data.len() >= 0x18 => {
                self.info.bit64 = true;
                SaharaPkt::MemoryDebug {
                    table_addr: u64::from_le_bytes(data[8..16].try_into().unwrap()),
                    table_len: u64::from_le_bytes(data[16..24].try_into().unwrap()),
                }
            }
            defs::cmd::READ_DATA if data.len() >= 0x14 => {
                self.info.bit64 = false;
                SaharaPkt::ReadData(ReadDataPkt {
                    image_id: rd_u32(&data, 8) as u64,
                    data_offset: rd_u32(&data, 12) as u64,
                    data_len: rd_u32(&data, 16) as u64,
                })
            }
            defs::cmd::EXECUTE_RSP if data.len() >= 0x10 => {
                SaharaPkt::ExecuteRsp { data_len: rd_u32(&data, 12) }
            }
            _ => SaharaPkt::Empty,
        }
    }

    fn cmd_hello(&mut self, mode: u32, version_min: u32) -> bool {
        let buf: Vec<u8> = [
            defs::cmd::HELLO_RSP.to_le_bytes(),
            0x30u32.to_le_bytes(),
            defs::SAHARA_VERSION.to_le_bytes(),
            version_min.to_le_bytes(),
            0u32.to_le_bytes(), // max_cmd_len
            mode.to_le_bytes(),
            1u32.to_le_bytes(), 2u32.to_le_bytes(), 3u32.to_le_bytes(),
            4u32.to_le_bytes(), 5u32.to_le_bytes(), 6u32.to_le_bytes(),
        ]
        .concat();
        self.cdc.write(&buf).is_ok()
    }

    /// Initial handshake and mode detection. Returns the detected mode
    /// ("sahara" | "firehose" | "nandprg" | "error").
    pub fn connect(&mut self) -> String {
        let v = self.raw_read(0xC * 4, 1000);
        if v.len() > 1 && v[0] == 0x01 && rd_u32(&v, 0) == defs::cmd::HELLO_REQ {
            // Parse the HELLO_REQ we already read (do not re-read the wire).
            if v.len() >= 0x18 {
                self.info.version = rd_u32(&v, 8);
                self.dbg(&format!(
                    "protocol version {}, pktsize {:#x}",
                    self.info.version,
                    rd_u32(&v, 16)
                ));
            }
            return "sahara".into();
        }
        if v.windows(5).any(|w| w == b"<?xml") {
            return "firehose".into();
        }
        if !v.is_empty() && v[0] == 0x7E {
            return "nandprg".into();
        }
        // Proactive handshake attempt (as in original connect() else-branch).
        let hello_rsp: Vec<u8> = [
            defs::cmd::HELLO_RSP.to_le_bytes(),
            0x30u32.to_le_bytes(),
            2u32.to_le_bytes(), 1u32.to_le_bytes(), 0u32.to_le_bytes(),
            defs::mode::COMMAND.to_le_bytes(),
            1u32.to_le_bytes(), 2u32.to_le_bytes(), 3u32.to_le_bytes(),
            4u32.to_le_bytes(), 5u32.to_le_bytes(), 6u32.to_le_bytes(),
        ]
        .concat();
        let _ = self.cdc.write(&hello_rsp);
        let res = self.raw_read(0x40, 2000);
        if res.len() >= 4 {
            match rd_u32(&res, 0) {
                defs::cmd::HELLO_REQ => {
                    self.info.version = 2;
                    return "sahara".into();
                }
                defs::cmd::CMD_READY => {
                    self.info("Sahara device ready (proactive handshake)");
                    return "sahara".into();
                }
                defs::cmd::END_TRANSFER => return "sahara".into(),
                _ => {}
            }
        }
        if res.windows(5).any(|w| w == b"<?xml") {
            return "firehose".into();
        }
        // Try a firehose nop probe.
        let _ = self.cdc.write(b"<?xml version=\"1.0\" ?><data><nop /></data>");
        let res = self.raw_read(0x40, 1000);
        if res.windows(5).any(|w| w == b"<?xml") {
            return "firehose".into();
        }
        // Try a streaming nandprg probe.
        let probe: Vec<u8> = vec![
            0x7E, 0x11, 0x00, 0x12, 0x00, 0xA0, 0xE3, 0x00, 0x00, 0xC1, 0xE5, 0x01, 0x40, 0xA0, 0xE3,
            0x1E, 0xFF, 0x2F, 0xE1, 0x4B, 0xD9, 0x7E,
        ];
        let _ = self.cdc.write(&probe);
        let res = self.raw_read(0x40, 1500);
        if res.len() > 1 && res[1] == 0x12 {
            return "nandprg".into();
        }
        self.err("Device is in Sahara error state, please reboot the device.");
        "error".into()
    }

    fn enter_command_mode(&mut self, version_min: u32) -> bool {
        if !self.cmd_hello(defs::mode::COMMAND, version_min) {
            return false;
        }
        match self.get_rsp() {
            SaharaPkt::CmdReady => true,
            SaharaPkt::EndTransfer(p) => {
                self.err(&format!("end transfer status {:#x}", p.image_tx_status));
                false
            }
            _ => false,
        }
    }

    fn cmd_exec(&mut self, mcmd: u32) -> Option<Vec<u8>> {
        let req: Vec<u8> = [defs::cmd::EXECUTE_REQ.to_le_bytes(), 0xCu32.to_le_bytes(), mcmd.to_le_bytes()].concat();
        if self.cdc.write(&req).is_err() {
            return None;
        }
        match self.get_rsp() {
            SaharaPkt::ExecuteRsp { data_len } => {
                let req2: Vec<u8> = [defs::cmd::EXECUTE_DATA.to_le_bytes(), 0xCu32.to_le_bytes(), mcmd.to_le_bytes()].concat();
                if self.cdc.write(&req2).is_err() {
                    return None;
                }
                Some(self.raw_read(data_len as usize, 3000))
            }
            SaharaPkt::EndTransfer(p) => {
                self.err(&format!("exec error status {:#x}", p.image_tx_status));
                None
            }
            _ => None,
        }
    }

    pub fn cmd_info(&mut self, version: u32, loaderdb: &LoaderDb) -> bool {
        if !self.enter_command_mode(1) {
            return false;
        }
        let serial = self.cmd_exec(defs::exec::SERIAL_NUM_READ)
            .map(|r| u32::from_le_bytes(r[..4].try_into().unwrap_or([0; 4])))
            .unwrap_or(0);
        self.info.serial = serial;
        self.info("Reading Chip Info : OK");
        self.info(&format!("- Sahara version  : {version}"));
        self.info(&format!("- Chip Serial Number : {serial:08x}"));

        let pkhash = self.cmd_exec(defs::exec::OEM_PK_HASH_READ).map(|r| r.hex());
        if version < 3 {
            let hwid = self.cmd_exec(defs::exec::MSM_HW_ID_READ)
                .map(|r| u64::from_le_bytes(r[..8].try_into().unwrap_or([0; 8])));
            if let Some(hwid) = hwid {
                let hwidstr = format!("{hwid:016x}");
                let msm_id = u32::from_str_radix(&hwidstr[2..8], 16).unwrap_or(0);
                let oem_id = u32::from_str_radix(&hwidstr[8..12], 16).unwrap_or(0);
                let model_id = u32::from_str_radix(&hwidstr[12..16], 16).unwrap_or(0);
                let msm_str = format!("{msm_id:08x}");
                let cpu = crate::config_tables::msm_id_name(msm_id)
                    .map(|n| format!("CPU detected: \"{n}\""))
                    .unwrap_or_else(|| "Unknown CPU (please report)".into());
                self.info(&format!(
                    "- HWID: 0x{hwidstr} (MSM_ID:0x{msm_str}, OEM_ID:0x{oem_id:04x}, MODEL_ID:0x{model_id:04x})\n- {cpu}"
                ));
                if let Some(pk) = &pkhash {
                    self.info(&format!("- PK_HASH: 0x{pk}"));
                }
                self.info.hwid = Some(hwid);
                self.info.hwidstr = Some(hwidstr.clone());
                self.info.msm_str = Some(msm_str);
                self.info.oem_str = Some(format!("{oem_id:04x}"));
                self.info.model_str = Some(format!("{model_id:04x}"));
                self.info.msm_id = Some(msm_id);
                if self.programmer.is_none() {
                    self.programmer = loaderdb.find_loader(&hwidstr, pkhash.as_deref());
                    if let Some(p) = &self.programmer {
                        self.info(&format!("Trying loader: {p}"));
                    } else {
                        self.err("Couldn't find a suitable loader :( (use --loader)");
                    }
                }
            }
        } else {
            // V3 extended chip info (cmd 0x0A).
            if let Some(res) = self.cmd_exec(defs::exec::READ_CHIP_ID_V3) {
                if res.len() >= 44 {
                    let msm_id = u32::from_le_bytes(res[36..40].try_into().unwrap());
                    let mut oem_id = u32::from_le_bytes(res[40..42].try_into().unwrap());
                    let model_id = u32::from_le_bytes(res[42..44].try_into().unwrap());
                    if oem_id == 0 && res.len() >= 46 {
                        oem_id = u32::from_le_bytes(res[44..46].try_into().unwrap());
                    }
                    self.info.msm_id = Some(msm_id);
                    self.info.oem_str = Some(format!("{oem_id:04x}"));
                    self.info.model_str = Some(format!("{model_id:04x}"));
                    let hwidstr = format!("00{msm_id:06x}{oem_id:04x}{model_id:04x}");
                    self.info.hwidstr = Some(hwidstr.clone());
                    self.info.hwid = Some(u64::from_str_radix(&hwidstr, 16).unwrap_or(0));
                    self.info(&format!("- MSM HWID : 0x{msm_id:06x} | model_id:{model_id:04x} | oem_id:{oem_id:04x}"));
                    if self.programmer.is_none() {
                        self.programmer = loaderdb.find_loader(&hwidstr, pkhash.as_deref());
                    }
                }
            }
            if self.programmer.is_none() {
                self.info("No autodetection of loader possible with sahara version 3, please use --loader option.");
            }
        }
        if let Some(pk) = pkhash {
            self.info.pkhash = Some(pk);
        }
        self.cmd_modeswitch(defs::mode::COMMAND);
        true
    }

    pub fn cmd_done(&mut self) -> bool {
        let buf: Vec<u8> = [defs::cmd::DONE_REQ.to_le_bytes(), 8u32.to_le_bytes()].concat();
        if self.cdc.write(&buf).is_err() {
            return false;
        }
        match self.get_rsp() {
            SaharaPkt::Done => true,
            SaharaPkt::EndTransfer(p) if p.image_tx_status == defs::NAK_INVALID_CMD => {
                self.err("Invalid Transfer command received.");
                false
            }
            _ => false,
        }
    }

    pub fn cmd_reset(&mut self) -> bool {
        let buf: Vec<u8> = [defs::cmd::RESET_REQ.to_le_bytes(), 8u32.to_le_bytes()].concat();
        if self.cdc.write(&buf).is_err() {
            return false;
        }
        matches!(self.get_rsp(), SaharaPkt::ResetRsp)
    }

    pub fn cmd_modeswitch(&mut self, mode: u32) {
        let buf: Vec<u8> = [defs::cmd::SWITCH_MODE.to_le_bytes(), 0xCu32.to_le_bytes(), mode.to_le_bytes()].concat();
        let _ = self.cdc.write(&buf);
    }

    /// Upload the firehose programmer over Sahara (image transfer).
    pub fn upload_loader(&mut self, version: u32, loader: &str) -> Result<String> {
        let programmer_data = std::fs::read(loader)
            .map_err(|e| anyhow!("Failed to open loader {loader}: {e}"))?;
        self.info(&format!("Uploading loader {loader} ..."));
        if !self.cmd_hello(defs::mode::IMAGE_TX_PENDING, version_min_for(version)) {
            return Err(anyhow!("Hello handshake failed during upload"));
        }
        let mut loop_count = 0u32;
        loop {
            match self.get_rsp() {
                SaharaPkt::XmlFirehose => return Ok("firehose".into()),
                SaharaPkt::ReadData(pkt) => {
                    self.info.bit64 = false;
                    if loop_count == 0 {
                        self.info("32-Bit mode detected.");
                    }
                    let mut img_id = pkt.image_id;
                    if img_id == 0x7 {
                        self.mode = "nandprg".into();
                    } else if img_id == 0xB {
                        self.mode = "enandprg".into();
                    } else if img_id >= 0xC {
                        self.mode = "firehose".into();
                    }
                    let offset = pkt.data_offset as usize;
                    let len = pkt.data_len as usize;
                    let mut out;
                    if offset + len > programmer_data.len() {
                        out = programmer_data.clone();
                        out.extend(std::iter::repeat(0xFF).take(offset + len - programmer_data.len()));
                    } else {
                        out = programmer_data.clone();
                    }
                    let data = out.drain(..).collect::<Vec<u8>>();
                    let chunk = &data[offset..offset + len];
                    self.cdc.write(chunk)?;
                    loop_count += 1;
                }
                SaharaPkt::EndTransfer(p) => {
                    if p.image_tx_status == defs::SUCCESS {
                        if self.cmd_done() {
                            self.info("Loader successfully uploaded.");
                            return Ok(self.mode.clone());
                        }
                        return Err(anyhow!("Error on uploading Loader (done handshake failed)"));
                    }
                    return Err(anyhow!("Upload failed with status {:#x}", p.image_tx_status));
                }
                SaharaPkt::Empty => {
                    self.dbg("empty rsp during upload, retrying");
                }
                _ => {}
            }
        }
    }
}

fn version_min_for(version: u32) -> u32 {
    if version >= 3 { 3 } else { 1 }
}

trait HexExt {
    fn hex(&self) -> String;
}

impl HexExt for [u8] {
    fn hex(&self) -> String {
        self.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hello_frame_layout() {
        let buf: Vec<u8> = [
            defs::cmd::HELLO_RSP.to_le_bytes(),
            0x30u32.to_le_bytes(),
            2u32.to_le_bytes(), 1u32.to_le_bytes(), 0u32.to_le_bytes(),
            defs::mode::COMMAND.to_le_bytes(),
        ]
        .concat();
        assert_eq!(buf.len(), 24);
        assert_eq!(rd_u32(&buf, 0), defs::cmd::HELLO_RSP);
        assert_eq!(rd_u32(&buf, 8), 2);
        assert_eq!(rd_u32(&buf, 20), defs::mode::COMMAND);
    }
}
