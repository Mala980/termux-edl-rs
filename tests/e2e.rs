//! End-to-end protocol tests: a mock Sahara device answers on the wire and the
//! real client code (Sahara state machine over the Transport trait) must
//! handshake, enter command mode, run cmd_info and cmd_done successfully.
//! GPLv3 — derived from B.Kerler's edl test approach.

use std::collections::VecDeque;
use termux_edl_rs::hdlc;
use termux_edl_rs::sahara::{Sahara, defs};
use termux_edl_rs::transport::Transport;

/// Mock EDL device: queues responses per write(), like a real Sahara endpoint.
struct MockSaharaDevice {
    queue: VecDeque<Vec<u8>>,
    connected: bool,
}

fn u32b(v: u32) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

impl MockSaharaDevice {
    fn new() -> Self {
        Self { queue: VecDeque::new(), connected: false }
    }

    /// Build a full Sahara HELLO_REQ packet (48 bytes) as the device sends it.
    fn hello_req() -> Vec<u8> {
        [
            u32b(defs::cmd::HELLO_REQ),
            u32b(0x30),          // length
            u32b(2),             // version
            u32b(2),             // version_supported
            u32b(0x30),          // max_packet_len
            u32b(defs::mode::IMAGE_TX_PENDING),
            u32b(1), u32b(2), u32b(3),
            u32b(4), u32b(5), u32b(6),
        ]
        .concat()
    }

    /// EXECUTE_RSP is 16 bytes: cmd, len=0x10, reserved, data_len.
    fn exec_rsp(data_len: u32) -> Vec<u8> {
        [
            u32b(defs::cmd::EXECUTE_RSP),
            u32b(0x10),
            u32b(0),
            u32b(data_len),
        ]
        .concat()
    }

    /// Queue the device's reply for the next client write.
    fn respond(&mut self, pkt: Vec<u8>) {
        self.queue.push_back(pkt);
    }
}

impl Transport for MockSaharaDevice {
    fn connect(&mut self, _vid: u16, _pid: u16) -> anyhow::Result<()> {
        self.connected = true;
        Ok(())
    }
    fn close(&mut self) {
        self.connected = false;
    }
    fn write(&mut self, _data: &[u8]) -> anyhow::Result<usize> {
        Ok(_data.len())
    }
    fn read(&mut self, resplen: usize, _timeout_ms: u32) -> anyhow::Result<Vec<u8>> {
        match self.queue.pop_front() {
            Some(mut pkt) => {
                pkt.truncate(resplen.max(pkt.len()));
                Ok(pkt)
            }
            None => Ok(Vec::new()),
        }
    }
    fn max_packet(&self) -> usize {
        512
    }
}

#[test]
fn e2e_sahara_handshake_and_cmd_info() {
    let mut dev = MockSaharaDevice::new();
    dev.connect(0x18D1, 0xD00D).unwrap();

    // Device immediately greets with HELLO_REQ.
    dev.respond(MockSaharaDevice::hello_req());

    // Responses to the client's HELLO_RSP(COMMAND), EXECUTE_REQ(s serial),
    // EXECUTE_REQ(hw id), EXECUTE_REQ(pk hash), DONE_REQ.
    dev.respond({
        let mut p = vec![];
        p.extend(u32b(defs::cmd::CMD_READY));
        p.extend(u32b(8));
        p
    });
    dev.respond(MockSaharaDevice::exec_rsp(4)); // serial num
    dev.respond(0xDEADBEEFu32.to_le_bytes().to_vec()); // serial
    dev.respond(MockSaharaDevice::exec_rsp(8)); // hwid
    dev.respond(0x0090_0E60_0100_7A71u64.to_le_bytes().to_vec()); // hwid (sm8250 family)
    dev.respond(MockSaharaDevice::exec_rsp(40)); // pkhash
    dev.respond(vec![0xAAu8; 40]); // pkhash

    // Client must detect mode "sahara" from HELLO_REQ.
    let mut s = Sahara::new(&mut dev, false);
    let mode = s.connect();
    assert_eq!(mode, "sahara", "mode detection must recognise HELLO_REQ");

    // cmd_info exercises enter_command_mode + 3 cmd_exec round-trips.
    let loaderdb = termux_edl_rs::loader_db::LoaderDb::default();
    assert!(
        s.cmd_info(2, &loaderdb),
        "cmd_info must complete the full command-mode exchange"
    );
    assert_eq!(s.info.serial, 0xDEADBEEF, "serial read back over the wire");
    assert!(
        s.info.hwid.is_some(),
        "hwid must be parsed from the MSM_HW_ID_READ response"
    );

    drop(s);
    // cmd_done round-trip: queue DONE_RSP(status=0), then a fresh client drives it.
    dev.respond({
        let mut p = vec![];
        p.extend(u32b(defs::cmd::DONE_RSP));
        p.extend(u32b(8));
        p.extend(u32b(0));
        p
    });
    let mut s2 = Sahara::new(&mut dev, false);
    assert!(s2.cmd_done(), "cmd_done must succeed against DONE_RSP");
}

#[test]
fn e2e_hdlc_framing_roundtrip() {
    // The wire layer: arbitrary payload must survive hdlc encode->decode.
    let payload: Vec<u8> = (0..=255u8).chain([0x7E, 0x7D, 0x11, 0x13]).collect();
    let frame = hdlc::convert_cmdbuf(&payload);
    assert_eq!(frame.last(), Some(&0x7E), "frame must end with flag");
    let decoded = hdlc::unescape(&frame[..frame.len()-1]).expect("valid frame must unescape");
    // decoded = payload + CRC16 (2 bytes, little endian)
    let n = decoded.len();
    assert!(n > 2);
    let (data, crc_bytes) = decoded.split_at(n - 2);
    let expect_crc = hdlc::crc16(0xFFFF, &payload);
    assert_eq!(data, &payload, "HDLC payload must survive framing");
    assert_eq!(
        u16::from_le_bytes([crc_bytes[0], crc_bytes[1]]),
        expect_crc,
        "trailing CRC16 must match crc16(0xFFFF, payload)"
    );
}

#[test]
fn e2e_firehose_xml_response_parse() {
    // Realistic firehose <response> over the wire must parse to ack=true.
    let xml = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><data><response value=\"ACK\" rawmode=\"false\"/></data>";
    let pairs = termux_edl_rs::xmlparser::getresponse(xml);
    assert!(
        pairs.iter().any(|(k, v)| k == "value" && v == "ACK"),
        "firehose response must parse to value=ACK, got: {pairs:?}"
    );
}
