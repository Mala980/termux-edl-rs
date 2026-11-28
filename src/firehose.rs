//! Firehose protocol (XML over the bulk transport) — port of edlclient/Library/firehose.py,
//! covering the core commands: configure, read, program, erase, nop, reset, storage info.
use crate::transport::Transport;
use crate::xmlparser;
use anyhow::{anyhow, Result};

#[derive(Debug, Clone)]
pub struct FirehoseResponse {
    pub resp: bool,
    pub data: Vec<u8>,
    pub attrs: Vec<(String, String)>,
    pub log: Vec<String>,
    pub error: String,
}

#[derive(Debug, Clone)]
pub struct FirehoseConfig {
    pub memory_name: String,
    pub sector_size: u32,
    pub max_payload_size_to_target: u32,
    pub max_payload_size_from_target: u32,
    pub max_xml_size: u32,
    pub zlp_aware_host: u32,
    pub skip_storage_init: bool,
    pub skip_write: bool,
    pub pages_per_block: u32,
    pub target_name: String,
    pub version: String,
    pub max_lun: u32,
}

impl Default for FirehoseConfig {
    fn default() -> Self {
        Self {
            memory_name: String::new(),
            sector_size: 0,
            max_payload_size_to_target: 0x100000,
            max_payload_size_from_target: 4096,
            max_xml_size: 4096,
            zlp_aware_host: 1,
            skip_storage_init: false,
            skip_write: false,
            pages_per_block: 0,
            target_name: "Unknown".into(),
            version: "Unknown".into(),
            max_lun: 8,
        }
    }
}

pub struct Firehose<'a> {
    pub cdc: &'a mut dyn Transport,
    pub cfg: FirehoseConfig,
    pub supported_functions: Vec<String>,
    pub skipresponse: bool,
    pub debug: bool,
}

fn attr<'r>(attrs: &'r [(String, String)], key: &str) -> Option<&'r str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

impl<'a> Firehose<'a> {
    pub fn new(cdc: &'a mut dyn Transport) -> Self {
        Self { cdc, cfg: FirehoseConfig::default(), supported_functions: Vec::new(), skipresponse: false, debug: false }
    }

    /// Drain pending loader boot output before the first command, exactly like
    /// python firehose.connect(): read until an empty read or a complete
    /// <response ...></data> is seen; collect LOG lines meanwhile.
    pub fn connect(&mut self) {
        let mut info: Vec<String> = Vec::new();
        loop {
            let v = self.cdc.read(0x10000, 50).unwrap_or_default();
            if v.is_empty() {
                break;
            }
            if v.windows(9).any(|w| w == b"</data>") {
                info.extend(xmlparser::getlog(&v));
                break;
            }
            info.extend(xmlparser::getlog(&v));
        }
        for line in &info {
            println!("[firehose] {line}");
            if line.to_lowercase().contains("supported functions") {
                self.supported_functions.push(line.clone());
            }
        }
    }

    fn read_until_response(&mut self, timeout_ms: u32) -> Vec<u8> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms.max(1) as u64);
        let mut rdata = Vec::new();
        while std::time::Instant::now() < deadline {
            let tmp = self.cdc.read(0x10000, 200).unwrap_or_default();
            if !tmp.is_empty() {
                rdata.extend_from_slice(&tmp);
                if find_sub(&rdata, b"<response value").is_some() {
                    break;
                }
            }
        }
        rdata
    }

    fn xmlsend(&mut self, data: &[u8], skipresponse: bool) -> FirehoseResponse {
        let _ = self.cdc.write(&data[..data.len().min(self.cfg.max_xml_size as usize)]);
        if skipresponse {
            return FirehoseResponse { resp: true, data: Vec::new(), attrs: Vec::new(), log: Vec::new(), error: String::new() };
        }
        let rdata = self.read_until_response(4000);
        let attrs = xmlparser::getresponse(&rdata);
        let log = xmlparser::getlog(&rdata);
        let value = attr(&attrs, "value").unwrap_or("");
        if value == "ACK" {
            FirehoseResponse { resp: true, data: rdata, attrs, log, error: String::new() }
        } else {
            let error = log.join("\n");
            FirehoseResponse { resp: false, data: rdata, attrs, log, error }
        }
    }

    /// Send the configure command and negotiate payload sizes.
    pub fn configure(&mut self) -> Result<()> {
        if self.cfg.sector_size == 0 {
            self.cfg.sector_size = if self.cfg.memory_name.is_empty()
                || self.cfg.memory_name.eq_ignore_ascii_case("emmc")
            { 512 } else { 4096 };
        }
        let mut connectcmd = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" ?><data><configure MemoryName=\"{}\" Verbose=\"0\" \
             AlwaysValidate=\"0\" MaxDigestTableSizeInBytes=\"2048\" \
             MaxPayloadSizeToTargetInBytes=\"{}\" ZLPAwareHost=\"{}\" \
             SkipStorageInit=\"{}\" SkipWrite=\"{}\"",
            self.cfg.memory_name,
            self.cfg.max_payload_size_to_target,
            self.cfg.zlp_aware_host,
            self.cfg.skip_storage_init as u8,
            self.cfg.skip_write as u8,
        );
        if self.cfg.memory_name.eq_ignore_ascii_case("nand") && self.cfg.pages_per_block != 0 {
            connectcmd += &format!(" PAGES_PER_BLOCK=\"{}\"", self.cfg.pages_per_block);
        }
        connectcmd += "/></data>";
        let rsp = self.xmlsend(connectcmd.as_bytes(), false);
        if !rsp.resp {
            // The first configure is often NAKed with the target's real limits
            // (python does the same): adopt them and re-send a REBUILT command.
            if let Some(v) = attr(&rsp.attrs, "MaxPayloadSizeToTargetInBytes") {
                if let Ok(n) = v.parse() {
                    self.cfg.max_payload_size_to_target = n;
                }
            }
            if let Some(v) = attr(&rsp.attrs, "MemoryName") {
                self.cfg.memory_name = v.to_string();
            }
            // Rebuild the configure XML with the adopted values.
            let mut connectcmd2 = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" ?><data><configure MemoryName=\"{}\" Verbose=\"0\"  AlwaysValidate=\"0\" MaxDigestTableSizeInBytes=\"2048\"  MaxPayloadSizeToTargetInBytes=\"{}\" ZLPAwareHost=\"{}\"  SkipStorageInit=\"{}\" SkipWrite=\"{}\"",
                self.cfg.memory_name,
                self.cfg.max_payload_size_to_target,
                self.cfg.zlp_aware_host,
                self.cfg.skip_storage_init as u8,
                self.cfg.skip_write as u8,
            );
            if self.cfg.memory_name.eq_ignore_ascii_case("nand") && self.cfg.pages_per_block != 0 {
                connectcmd2 += &format!(" PAGES_PER_BLOCK=\"{}\"", self.cfg.pages_per_block);
            }
            connectcmd2 += "/></data>";
            let rsp2 = self.xmlsend(connectcmd2.as_bytes(), false);
            if !rsp2.resp {
                if rsp2.error.contains("Not support configure MemoryName eMMC") {
                    println!("[firehose] eMMC not supported by this loader — trying UFS");
                    self.cfg.memory_name = "UFS".into();
                    return self.configure();
                }
                if rsp2.error.contains("payload to target size is too large")
                    && self.cfg.max_payload_size_to_target > 8192
                {
                    self.cfg.max_payload_size_to_target /= 2;
                    return self.configure();
                }
                return Err(anyhow!("configure failed: {}", rsp2.error));
            }
        }
        println!("[firehose] TargetName={}", self.cfg.target_name);
        println!("[firehose] MemoryName={}", self.cfg.memory_name);
        println!("[firehose] Version={}", self.cfg.version);
        // Sector-0 probe is informational only (python continues when it
        // fails; the real GPT read below retries with skipresponse).
        let rsp = self.cmd_read_buffer(0, 1, 1, false)?;
        if !rsp.resp {
            eprintln!("[firehose] Warning: sector-0 probe failed, continuing");
        }
        Ok(())
    }

    pub fn wait_for_data(&mut self) -> Vec<u8> {
        let mut tmp = Vec::new();
        let mut empties = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while find_sub(&tmp, b"response value").is_none() {
            if std::time::Instant::now() > deadline {
                break;
            }
            let res = self.cdc.read(0x10000, 1000).unwrap_or_default();
            if res.is_empty() {
                empties += 1;
                if empties == 4 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            } else {
                empties = 0;
                tmp.extend_from_slice(&res);
            }
        }
        tmp
    }

    pub fn cmd_nop(&mut self) -> Result<Vec<String>> {
        let data = b"<?xml version=\"1.0\" ?><data><nop /></data>";
        let rsp = self.xmlsend(data, false);
        let mut info = Vec::new();
        let mut tmp = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if std::time::Instant::now() > deadline {
                break;
            }
            let chunk = self.cdc.read(0x10000, 500).unwrap_or_default();
            if chunk.is_empty() {
                break;
            }
            tmp.extend_from_slice(&chunk);
        }
        if !tmp.is_empty() {
            info = xmlparser::getlog(&tmp);
        }
        if rsp.resp {
            Ok(info)
        } else {
            Err(anyhow!("Nop failed: {}", rsp.error))
        }
    }

    pub fn cmd_reset(&mut self, mode: &str) -> bool {
        let data = format!("<?xml version=\"1.0\" ?><data><power value=\"{mode}\"/></data>");
        let val = self.xmlsend(data.as_bytes(), false);
        if val.resp {
            println!("[firehose] Reset succeeded.");
        } else {
            eprintln!("[firehose:err] Reset failed: {}", val.error);
        }
        val.resp
    }

    pub fn cmd_read_buffer(
        &mut self,
        physical_partition_number: u32,
        start_sector: u64,
        num_partition_sectors: u64,
        display: bool,
    ) -> Result<FirehoseResponse> {
        if display {
            println!(
                "[firehose] Reading from physical partition {physical_partition_number}, sector {start_sector}, sectors {num_partition_sectors}"
            );
        }
        let mut data = format!(
            "<?xml version=\"1.0\" ?><data><read SECTOR_SIZE_IN_BYTES=\"{}\" num_partition_sectors=\"{}\" \
             physical_partition_number=\"{}\" start_sector=\"{}\"",
            self.cfg.sector_size, num_partition_sectors, physical_partition_number, start_sector
        );
        data += &self.nand_pages_attr();
        data += "/></data>";
        let mut rsp = self.xmlsend(data.as_bytes(), self.skipresponse);
        if !rsp.resp {
            if display {
                eprintln!("[firehose:err] {}", rsp.error);
            }
            // python get_gpt: on failure retry once with skipresponse=True
            // (loader then emits raw data without expecting a response).
            self.skipresponse = true;
            rsp = self.xmlsend(data.as_bytes(), true);
            self.skipresponse = false;
            if !rsp.resp {
                return Ok(rsp);
            }
        }
        let bytestoread = self.cfg.sector_size as usize * num_partition_sectors as usize;
        let mut res_data = Vec::with_capacity(bytestoread);
        // read_until_response may have already pulled RAW sector bytes into
        // rsp.data before the ACK — keep only the raw part (strip "<?xml...).
        {
            let raw = match find_sub(&rsp.data, b"<?xml") {
                Some(idx) => &rsp.data[..idx],
                None => &rsp.data[..],
            };
            res_data.extend_from_slice(raw);
        }
        let mut remaining = bytestoread.saturating_sub(res_data.len());
        while remaining > 0 {
            let want = remaining.min(0x100000);
            let tmp = self.cdc.read(want, 5000).unwrap_or_default();
            if tmp.is_empty() {
                break;
            }
            remaining -= tmp.len();
            res_data.extend_from_slice(&tmp);
        }
        if self.debug {
            if res_data.starts_with(b"<?xml") {
                eprintln!(
                    "[debug] read l{} p{} n{} -> XML RESPONSE ({} bytes): {}",
                    physical_partition_number, start_sector, num_partition_sectors,
                    res_data.len(),
                    String::from_utf8_lossy(&res_data)
                );
            } else {
                eprintln!(
                    "[debug] read l{} p{} n{} -> {} bytes RAW, head: {}",
                    physical_partition_number, start_sector, num_partition_sectors,
                    res_data.len(),
                    res_data.iter().take(24).map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
                );
            }
        }
        let wd = self.wait_for_data();
        let log = xmlparser::getlog(&wd);
        let attrs = xmlparser::getresponse(&wd);
        let value = attr(&attrs, "value").unwrap_or("");
        // Python only fails when an explicit NAK ("value" != ACK) arrives;
        // an absent response (data already consumed) is success.
        if value.is_empty() || value == "ACK" {
            Ok(FirehoseResponse { resp: true, data: res_data, attrs, log, error: String::new() })
        } else {
            Ok(FirehoseResponse { resp: false, data: res_data, attrs, error: log.join("\n"), log })
        }
    }

    pub fn cmd_read(
        &mut self,
        physical_partition_number: u32,
        start_sector: u64,
        num_partition_sectors: u64,
        filename: &str,
        display: bool,
    ) -> Result<bool> {
        // Streaming variant of python cmd_read: send the <read> command,
        // write raw bytes to disk AS THEY ARRIVE (no giant RAM buffer), then
        // wait for the final ACK. Bounded by an overall deadline so a dead
        // device can never spin the loop (and the USB stack) indefinitely.
        let mut data = format!(
            "<?xml version=\"1.0\" ?><data><read SECTOR_SIZE_IN_BYTES=\"{}\" num_partition_sectors=\"{}\" \
             physical_partition_number=\"{}\" start_sector=\"{}\"",
            self.cfg.sector_size, num_partition_sectors, physical_partition_number, start_sector
        );
        data += &self.nand_pages_attr();
        data += "/>\n</data>";
        let rsp = self.xmlsend(data.as_bytes(), self.skipresponse);
        if !rsp.resp {
            eprintln!("[firehose:err] {}", rsp.error);
            return Ok(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut f = std::io::BufWriter::new(std::fs::File::create(filename)?);
        let bytestoread = self.cfg.sector_size as usize * num_partition_sectors as usize;
        let total = bytestoread;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        // Split big reads into 1 MB <read> commands, each with its own ACK.
        // One giant read of tens of MB floods the phone's usbfs and has been
        // observed to reboot it; small commands keep the loader's queue sane.
        let chunk_bytes: usize = 0x100000;
        let ss = self.cfg.sector_size as u64;
        let sectors_per_chunk = (chunk_bytes as u64 / ss).max(1);
        let mut start = start_sector;
        let mut remaining_sectors = num_partition_sectors;
        let mut got = 0usize;
        while remaining_sectors > 0 {
            if std::time::Instant::now() > deadline {
                eprintln!("[firehose:err] Read timed out after {} bytes", got);
                return Ok(false);
            }
            let n_sectors = remaining_sectors.min(sectors_per_chunk);
            let cmd = format!(
                "<?xml version=\"1.0\" ?><data><read SECTOR_SIZE_IN_BYTES=\"{}\" num_partition_sectors=\"{}\" \
                 physical_partition_number=\"{}\" start_sector=\"{}\"/>\n</data>",
                self.cfg.sector_size, n_sectors, physical_partition_number, start
            );
            let mut rsp = self.xmlsend(cmd.as_bytes(), self.skipresponse);
            if !rsp.resp {
                if self.debug {
                    eprintln!(
                        "[debug] chunk read p{} s{} n{} got no ACK ({} bytes buffered): {}",
                        physical_partition_number, start, n_sectors, rsp.data.len(),
                        String::from_utf8_lossy(&rsp.data[..rsp.data.len().min(400)])
                    );
                }
                // Old MSM8916 loader: retry once with skipresponse (python
                // get_gpt parity) — it then streams raw data w/o response.
                self.skipresponse = true;
                rsp = self.xmlsend(cmd.as_bytes(), true);
                self.skipresponse = false;
            }
            if !rsp.resp {
                eprintln!("[firehose:err] {}", rsp.error);
                return Ok(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            let want = ss as usize * n_sectors as usize;
            let mut chunk_got = 0usize;
            while chunk_got < want {
                if std::time::Instant::now() > deadline {
                    eprintln!("[firehose:err] Read timed out after {} bytes", got);
                    return Ok(false);
                }
                let tmp = self.cdc.read(want - chunk_got, 5000).unwrap_or_default();
                if tmp.is_empty() {
                    eprintln!("[firehose:err] USB read stalled at {}/{} bytes", got, total);
                    return Ok(false);
                }
                chunk_got += tmp.len();
                got += tmp.len();
                std::io::Write::write_all(&mut f, &tmp)?;
                // Pace the bus: a short breather every 256 KB keeps the
                // phone's usbfs from being saturated for minutes on end.
                if got % 0x40000 < 0x10000 {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            if display {
                show_progress("Read", got, total);
            }
            // Wait for this chunk's final ACK so the wire stays in sync.
            let wd = self.wait_for_data();
            let attrs = xmlparser::getresponse(&wd);
            let value = attr(&attrs, "value").unwrap_or("");
            if value == "NAK" {
                let log = xmlparser::getlog(&wd);
                eprintln!("[firehose:err] Read failed at sector {start}:");
                for line in &log {
                    eprintln!("  {line}");
                }
                return Ok(false);
            }
            start += n_sectors;
            remaining_sectors -= n_sectors;
        }
        std::io::Write::flush(&mut f)?;
        drop(f);
        Ok(true)
    }

    pub fn cmd_program(
        &mut self,
        physical_partition_number: u32,
        start_sector: u64,
        filename: &str,
        display: bool,
    ) -> Result<bool> {
        let filedata = std::fs::read(filename)?;
        self.cmd_program_buffer(physical_partition_number, start_sector, &filedata, display)
    }

    pub fn cmd_program_buffer(
        &mut self,
        physical_partition_number: u32,
        start_sector: u64,
        wfdata: &[u8],
        display: bool,
    ) -> Result<bool> {
        let total = wfdata.len();
        let ss = self.cfg.sector_size as usize;
        let mut num_partition_sectors = total / ss;
        if total % ss != 0 {
            num_partition_sectors += 1;
        }
        if display {
            println!(
                "[firehose] Writing to physical partition {physical_partition_number}, sector {start_sector}, sectors {num_partition_sectors}"
            );
        }
        let mut data = format!(
            "<?xml version=\"1.0\" ?><data><program SECTOR_SIZE_IN_BYTES=\"{}\" num_partition_sectors=\"{}\" \
             physical_partition_number=\"{}\" start_sector=\"{}\"",
            self.cfg.sector_size, num_partition_sectors, physical_partition_number, start_sector
        );
        data += &self.nand_pages_attr();
        data += " /></data>";
        let rsp = self.xmlsend(data.as_bytes(), self.skipresponse);
        if !rsp.resp {
            eprintln!("[firehose:err] {}", rsp.error);
            return Ok(false);
        }
        let mut pos = 0usize;
        let maxpayload = self.cfg.max_payload_size_to_target as usize;
        while pos < total {
            let wlen = (total - pos).min(maxpayload);
            let mut wrdata = wfdata[pos..pos + wlen].to_vec();
            pos += wlen;
            if wrdata.len() % ss != 0 {
                let fill = (wrdata.len() / ss + 1) * ss;
                wrdata.resize(fill, 0);
            }
            self.cdc.write(&wrdata)?;
            // Zero-length write (ZLP) as in the original.
            let _ = self.cdc.write(b"");
            show_progress("Write", pos, total);
        }
        let wd = self.wait_for_data();
        let log = xmlparser::getlog(&wd);
        let attrs = xmlparser::getresponse(&wd);
        let value = attr(&attrs, "value").unwrap_or("");
        if value != "ACK" {
            eprintln!("[firehose:err] Program failed:");
            for line in &log {
                eprintln!("  {line}");
            }
            return Ok(false);
        }
        Ok(true)
    }

    pub fn cmd_erase(
        &mut self,
        physical_partition_number: u32,
        start_sector: u64,
        num_partition_sectors: u64,
        display: bool,
    ) -> Result<bool> {
        if display {
            println!(
                "[firehose] Erasing from physical partition {physical_partition_number}, sector {start_sector}, sectors {num_partition_sectors}"
            );
        }
        let has_erase = self.supported_functions.iter().any(|f| f.eq_ignore_ascii_case("erase"));
        let cmd = if has_erase { "erase" } else { "program" };
        let mut data = format!(
            "<?xml version=\"1.0\" ?><data><{cmd} SECTOR_SIZE_IN_BYTES=\"{}\" num_partition_sectors=\"{}\" \
             physical_partition_number=\"{}\" start_sector=\"{}\"",
            self.cfg.sector_size, num_partition_sectors, physical_partition_number, start_sector
        );
        data += &self.nand_pages_attr();
        data += " /></data>";
        let rsp = self.xmlsend(data.as_bytes(), self.skipresponse);
        if !rsp.resp {
            eprintln!("[firehose:err] {}", rsp.error);
            return Ok(false);
        }
        if has_erase {
            return Ok(true);
        }
        // Write zeros over the range (program-based erase).
        let empty = vec![0u8; self.cfg.max_payload_size_to_target as usize];
        let ss = self.cfg.sector_size as usize;
        let mut bytestowrite = ss * num_partition_sectors as usize;
        let total = bytestowrite;
        while bytestowrite > 0 {
            let wlen = bytestowrite.min(self.cfg.max_payload_size_to_target as usize);
            self.cdc.write(&empty[..wlen])?;
            self.cdc.write(b"")?;
            bytestowrite -= wlen;
            show_progress("Erase", total - bytestowrite, total);
        }
        let wd = self.wait_for_data();
        let log = xmlparser::getlog(&wd);
        let attrs = xmlparser::getresponse(&wd);
        Ok(attr(&attrs, "value").unwrap_or("") == "ACK" || {
            for line in &log {
                eprintln!("  {line}");
            }
            false
        })
    }

    fn nand_pages_attr(&self) -> String {
        if self.cfg.memory_name.eq_ignore_ascii_case("nand") && self.cfg.pages_per_block != 0 {
            format!(" PAGES_PER_BLOCK=\"{}\"", self.cfg.pages_per_block)
        } else {
            String::new()
        }
    }

    /// Read the GPT from the given LUN and parse it.
    pub fn get_gpt(
        &mut self,
        lun: u32,
        sector_size_hint: Option<u32>,
    ) -> Result<Option<(Vec<u8>, crate::gpt::Gpt)>> {
        let ss = sector_size_hint.unwrap_or(self.cfg.sector_size);
        let rsp = self.cmd_read_buffer(lun, 0, 1, false)?;
        if !rsp.resp {
            return Err(anyhow!("Failed reading sector 0 on lun {lun}"));
        }
        let mut data = rsp.data.clone();
        let rsp2 = self.cmd_read_buffer(lun, 1, 1, false)?;
        data.extend_from_slice(&rsp2.data);
        // Nand partition table magic check (0x844bdcd1 header at sector 0).
        if data.len() >= 4 && u32::from_le_bytes(data[0..4].try_into().unwrap()) == 0x844bdcd1 {
            println!("[firehose] Nand storage detected — use 'rs' for raw sector access.");
            return Ok(None);
        }
        let mut guid_gpt = crate::gpt::Gpt::new();
        if self.debug && data.len() > ss as usize {
            eprintln!(
                "[debug] gpt total {} bytes, hdr16@ss: {}",
                data.len(),
                data[ss as usize..(ss as usize + 16).min(data.len())]
                    .iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
            );
        }
        if let Some(hdr) = crate::gpt::Gpt::parseheader(&data, ss as usize) {
            if hdr.num_part_entries > 0 && hdr.part_entry_size > 0 {
                let part_table_size = hdr.num_part_entries as u64 * hdr.part_entry_size as u64;
                let mut sectors = part_table_size / ss as u64;
                if part_table_size % ss as u64 != 0 {
                    sectors += 1;
                }
                let sectors = sectors.clamp(1, 64);
                let start_lba = if hdr.part_entry_start_lba != 0 { hdr.part_entry_start_lba } else { 2 };
                let rsp3 = self.cmd_read_buffer(lun, start_lba, sectors, false)?;
                data.extend_from_slice(&rsp3.data);
                if guid_gpt.parse(&data, ss as usize) {
                    return Ok(Some((data, guid_gpt)));
                }
            }
        }
        Ok(None)
    }

    /// Parse storage info from a getstorageinfo response and adapt sector size / luns.
    pub fn cmd_getstorageinfo(&mut self) -> Result<()> {
        let data = b"<?xml version=\"1.0\" ?><data><getstorageinfo physical_partition_number=\"0\"/></data>";
        let rsp = self.xmlsend(data, false);
        let mut info = Vec::new();
        if !rsp.data.is_empty() {
            info.extend(rsp.log.clone());
            // Some loaders emit storage_info JSON inside log lines.
            for line in &rsp.log {
                println!("[storageinfo] {line}");
                if line.contains("UFS Erase Block Size") {
                    if let Some(v) = extract_hex(line) {
                        if v > 0 {
                            self.cfg.sector_size = 4096;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn getluns(&self, lun_arg: Option<u32>) -> Vec<u32> {
        if let Some(l) = lun_arg {
            return vec![l];
        }
        if self.cfg.memory_name.eq_ignore_ascii_case("ufs") {
            (0..self.cfg.max_lun).collect()
        } else {
            vec![0]
        }
    }
}

fn extract_hex(line: &str) -> Option<u32> {
    let idx = line.find("0x")?;
    let rest = &line[idx + 2..];
    let end = rest
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(rest.len());
    u32::from_str_radix(&rest[..end], 16).ok()
}

pub fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

pub fn show_progress(prefix: &str, pos: usize, total: usize) {
    if total == 0 {
        return;
    }
    let pct = (pos as f64 / total as f64 * 100.0).min(100.0);
    let filled = (pct / 2.0) as usize;
    let bar = "=".repeat(filled) + &" ".repeat(50 - filled);
    print!("\r{prefix} Progress: [{bar}] {:.1}% complete", pct);
    use std::io::Write;
    let _ = std::io::stdout().flush();
    if pct >= 100.0 {
        println!();
    }
}
