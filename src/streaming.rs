//! Streaming (nandprg/enandprg) minimal client — port of edlclient/Library/streaming.py
//! (mode 1/2 support: read/write/erase raw sectors over the HDLC-framed streaming protocol).

use anyhow::{anyhow, Result};

pub const MAX_PACKET_LEN: usize = 1024;

pub mod streaming_defs {
    pub mod cmd {
        pub const ACK: u8 = 0x01;
        pub const NAK: u8 = 0x02;
        pub const CONNECT: u8 = 0x05;
        pub const READ: u8 = 0x06;
        pub const WRITE: u8 = 0x07;
        pub const ERASE: u8 = 0x09;
        pub const POWERDOWN: u8 = 0x0B;
        pub const NOP: u8 = 0xFF;
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StreamingFeature {
    OpenMulti = 0x01,
    OpenSingle = 0x02,
    Read = 0x04,
    Write = 0x10,
    Nop = 0x20,
    Erase = 0x40,
    PowerDown = 0x80,
}

pub struct StreamingFeatures {
    pub version: u8,
    pub preferred_block_size: u32,
    pub total_blocks: u32,
    pub feature_bits: u8,
}

impl StreamingFeatures {
    pub fn has(&self, f: StreamingFeature) -> bool {
        self.feature_bits & f as u8 != 0
    }
}

pub struct Streaming {
    /// HDLC transport send/receive callbacks (framed by hdlc module).
    pub send: Box<dyn FnMut(&[u8]) -> Result<()>>,
    pub recv: Box<dyn FnMut(u32) -> Result<Vec<u8>>>,
}

impl Streaming {
    pub fn connect(&mut self, mode: u8) -> Result<bool> {
        // connect packet: 0x7e handled by framing; payload: cmd CONNECT, mode, version
        let mut buf = vec![streaming_defs::cmd::CONNECT, mode, 0x01, 0x00];
        (self.send)(&buf)?;
        let rsp = (self.recv)(3000)?;
        if rsp.is_empty() {
            return Ok(false);
        }
        Ok(rsp[0] == streaming_defs::cmd::ACK)
    }

    /// Get feature information (NOP cmd with data request).
    pub fn get_features(&mut self) -> Result<StreamingFeatures> {
        let buf = vec![streaming_defs::cmd::NOP, 0x01, 0x00];
        (self.send)(&buf)?;
        let rsp = (self.recv)(3000)?;
        if rsp.len() < 12 || rsp[0] != streaming_defs::cmd::ACK {
            return Err(anyhow!("No streaming feature response"));
        }
        Ok(StreamingFeatures {
            version: rsp[1],
            preferred_block_size: u32::from_le_bytes(rsp[2..6].try_into().unwrap()),
            total_blocks: u32::from_le_bytes(rsp[6..10].try_into().unwrap()),
            feature_bits: rsp[11],
        })
    }

    /// Read sectors: address (byte offset), length in bytes.
    pub fn read(&mut self, addr: u32, length: u32) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        buf.push(streaming_defs::cmd::READ);
        buf.extend_from_slice(&addr.to_le_bytes());
        buf.extend_from_slice(&length.to_le_bytes());
        (self.send)(&buf)?;
        let rsp = (self.recv)(5000)?;
        if rsp.is_empty() || rsp[0] != streaming_defs::cmd::ACK {
            return Err(anyhow!("Streaming read failed at {addr:#x}"));
        }
        Ok(rsp[1..].to_vec())
    }

    /// Write sectors from data at addr.
    pub fn write(&mut self, addr: u32, data: &[u8]) -> Result<bool> {
        let mut buf = Vec::new();
        buf.push(streaming_defs::cmd::WRITE);
        buf.extend_from_slice(&addr.to_le_bytes());
        buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        (self.send)(&buf)?;
        // data chunks
        for chunk in data.chunks(MAX_PACKET_LEN) {
            (self.send)(chunk)?;
        }
        let rsp = (self.recv)(5000)?;
        Ok(!rsp.is_empty() && rsp[0] == streaming_defs::cmd::ACK)
    }

    pub fn erase(&mut self, addr: u32, length: u32) -> Result<bool> {
        let mut buf = Vec::new();
        buf.push(streaming_defs::cmd::ERASE);
        buf.extend_from_slice(&addr.to_le_bytes());
        buf.extend_from_slice(&length.to_le_bytes());
        (self.send)(&buf)?;
        let rsp = (self.recv)(5000)?;
        Ok(!rsp.is_empty() && rsp[0] == streaming_defs::cmd::ACK)
    }

    pub fn powerdown(&mut self) -> Result<bool> {
        let buf = vec![streaming_defs::cmd::POWERDOWN];
        (self.send)(&buf)?;
        let rsp = (self.recv)(3000)?;
        Ok(!rsp.is_empty() && rsp[0] == streaming_defs::cmd::ACK)
    }
}
