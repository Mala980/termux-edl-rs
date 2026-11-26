//! GPT parsing/printing — port of edlclient/Library/gpt.py.
use std::collections::HashMap;

pub const AB_FLAG_OFFSET: usize = 6;
pub const AB_PARTITION_ATTR_SLOT_ACTIVE: u64 = 0x1 << 2;

fn rd_u16(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(d[off..off + 2].try_into().unwrap())
}
fn rd_u32(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(d[off..off + 4].try_into().unwrap())
}
fn rd_u64(d: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(d[off..off + 8].try_into().unwrap())
}

pub struct GptHeader {
    pub num_part_entries: u32,
    pub part_entry_size: u32,
    pub part_entry_start_lba: u64,
}

pub struct PartitionEntry {
    pub unique: String,
    pub sector: u64,
    pub sectors: u64,
    pub flags: u64,
    pub name: String,
    pub entryoffset: u64,
}

pub struct Gpt {
    pub header: Option<GptHeader>,
    pub sectorsize: usize,
    pub partentries: HashMap<String, PartitionEntry>,
    pub totalsectors: u64,
}

fn efi_type_name(type_guid: u32) -> String {
    match type_guid {
        0x00000000 => "EFI_UNUSED".into(),
        0xC12A7328 => "EFI_SYSTEM".into(),
        0x21686148 => "EFI_BIOS_BOOT".into(),
        0xE3C9E316 => "EFI_MSR".into(),
        0xEBD0A0A2 => "EFI_BASIC_DATA".into(),
        0xDE94BBA4 => "EFI_RECOVERY".into(),
        0x44479540 => "EFI_LINUX_ROOT32".into(),
        0x4F68BCE3 => "EFI_LINUX_ROOT64".into(),
        0x69DAD710 => "EFI_LINUX_ROOT_ARM32".into(),
        0xB921B045 => "EFI_LINUX_ROOT_ARM64".into(),
        0x0657FD6D => "EFI_LINUX_SWAP".into(),
        0xE6D6D379 => "EFI_LINUX_LVM".into(),
        _ => format!("{type_guid:08x}"),
    }
}

impl Default for Gpt {
    fn default() -> Self {
        Self::new()
    }
}

impl Gpt {
    pub fn new() -> Self {
        Self { header: None, sectorsize: 512, partentries: HashMap::new(), totalsectors: 0 }
    }

    pub fn parseheader(gptdata: &[u8], sectorsize: usize) -> Option<GptHeader> {
        if gptdata.len() < sectorsize + 0x5C {
            return None;
        }
        let h = &gptdata[sectorsize..];
        Some(GptHeader {
            num_part_entries: rd_u32(h, 0x50),
            part_entry_size: rd_u32(h, 0x54),
            part_entry_start_lba: rd_u64(h, 0x48),
        })
    }

    pub fn parse(&mut self, gptdata: &[u8], sectorsize: usize) -> bool {
        if gptdata.len() < sectorsize + 0x5C {
            return false;
        }
        let h = &gptdata[sectorsize..];
        if &h[0..8] != b"EFI PART" {
            return false;
        }
        let num_part_entries = rd_u32(h, 0x50);
        let part_entry_size = rd_u32(h, 0x54);
        let part_entry_start_lba = rd_u64(h, 0x48);
        self.header = Some(GptHeader { num_part_entries, part_entry_size, part_entry_start_lba });
        self.sectorsize = sectorsize;

        let start = if part_entry_start_lba != 0 {
            (part_entry_start_lba as usize) * sectorsize
        } else {
            2 * sectorsize // mbr + header + part_table
        };

        self.partentries.clear();
        for idx in 0..num_part_entries as usize {
            let esz = part_entry_size as usize;
            let off = start + idx * esz;
            if off + esz > gptdata.len() {
                break;
            }
            let entry = &gptdata[off..off + esz];
            if entry.len() < 128 {
                break;
            }
            if rd_u64(entry, 16) == 0 {
                break; // unique guid zero => unused
            }
            let guid1 = rd_u32(entry, 16);
            let guid2 = rd_u16(entry, 20);
            let guid3 = rd_u16(entry, 22);
            let guid4 = rd_u16(entry, 24);
            let guid5: String = entry[26..32].iter().map(|b| format!("{b:02x}")).collect();
            let unique = format!("{guid1:08x}-{guid2:04x}-{guid3:04x}-{guid4:04x}-{guid5}");
            let first_lba = rd_u64(entry, 32);
            let last_lba = rd_u64(entry, 40);
            let flags = rd_u64(entry, 48);
            let name = decode_utf16(&entry[56..128]);
            let type_guid = rd_u32(entry, 0);
            if type_guid == 0 {
                continue;
            }
            let pa = PartitionEntry {
                unique,
                sector: first_lba,
                sectors: last_lba - first_lba + 1,
                flags,
                name: name.clone(),
                entryoffset: (part_entry_start_lba as u64 * sectorsize as u64) + (idx as u64 * esz as u64),
            };
            let _ = efi_type_name(type_guid);
            self.partentries.insert(name, pa);
        }
        if let Some(hdr) = &self.header {
            let _ = hdr;
        }
        let first_usable = rd_u64(h, 0x28);
        let last_usable = rd_u64(h, 0x30);
        self.totalsectors = first_usable + last_usable;
        true
    }

    pub fn tostring(&self) -> String {
        let mut mstr = "\nGPT Table:\n-------------\n".to_string();
        for (partitionname, partition) in &self.partentries {
            let active = ((partition.flags >> (AB_FLAG_OFFSET * 8)) & 0xFF) & AB_PARTITION_ATTR_SLOT_ACTIVE
                == AB_PARTITION_ATTR_SLOT_ACTIVE;
            mstr += &format!(
                "{:<20} Offset 0x{:016x}, Length 0x{:016x}, Flags 0x{:016x}, UUID {}, Active {}\n",
                format!("{partitionname}:"),
                partition.sector * self.sectorsize as u64,
                partition.sectors * self.sectorsize as u64,
                partition.flags,
                partition.unique,
                active
            );
        }
        mstr += &format!(
            "\nTotal disk size:0x{:016x}, sectors:0x{:016x}\n",
            self.totalsectors * self.sectorsize as u64,
            self.totalsectors
        );
        mstr
    }

    /// Parse a raw GPT image file, trying 512 and 4096 sector sizes.
    pub fn parse_file(data: &[u8]) -> Option<(usize, Gpt)> {
        for sectorsize in [512usize, 4096] {
            let mut g = Gpt::new();
            if g.parse(data, sectorsize) {
                return Some((sectorsize, g));
            }
        }
        None
    }
}

fn decode_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
    String::from_utf16_lossy(&units).trim_matches('\0').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal synthetic GPT image (128-byte LBA, 512 sector size).
    fn build_gpt() -> Vec<u8> {
        let mut img = vec![0u8; 512 * 4];
        // protective MBR magic
        img[510] = 0x55;
        img[511] = 0xAA;
        let mut h = vec![0u8; 0x5C];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x10000u32.to_le_bytes()); // revision
        h[0x18..0x20].copy_from_slice(&2u64.to_le_bytes()); // current_lba
        h[0x20..0x28].copy_from_slice(&3u64.to_le_bytes()); // backup_lba
        h[0x28..0x30].copy_from_slice(&4u64.to_le_bytes()); // first_usable_lba
        h[0x30..0x38].copy_from_slice(&100u64.to_le_bytes()); // last_usable_lba
        h[0x48..0x50].copy_from_slice(&0u64.to_le_bytes()); // part_entry_start_lba
        h[0x50..0x54].copy_from_slice(&2u32.to_le_bytes()); // num entries
        h[0x54..0x58].copy_from_slice(&128u32.to_le_bytes()); // entry size
        img[512..512 + h.len()].copy_from_slice(&h);
        // entry 0: type basic data (ebd0a0a2), unique, lba 10..20, name "boot_a" utf16
        let mut e = vec![0u8; 128];
        e[0..4].copy_from_slice(&0xEBD0A0A2u32.to_le_bytes());
        e[16..20].copy_from_slice(&0x1234u32.to_le_bytes());
        e[32..40].copy_from_slice(&10u64.to_le_bytes());
        e[40..48].copy_from_slice(&20u64.to_le_bytes());
        e[48..56].copy_from_slice(&0x100u64.to_le_bytes());
        let name: Vec<u8> = "boot_a"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        e[56..56 + name.len()].copy_from_slice(&name);
        img[1024..1152].copy_from_slice(&e);
        // entry 1: unused (unique zero)
        img
    }

    #[test]
    fn test_parse_gpt() {
        let img = build_gpt();
        let mut g = Gpt::new();
        assert!(g.parse(&img, 512));
        let p = g.partentries.get("boot_a").expect("boot_a missing");
        assert_eq!(p.sector, 10);
        assert_eq!(p.sectors, 11);
        assert_eq!(p.flags, 0x100);
        let s = g.tostring();
        assert!(s.contains("boot_a"));
        assert!(s.contains("Offset 0x0000000000001400")); // 10*512
    }

    #[test]
    fn test_parse_rejects_non_gpt() {
        let mut g = Gpt::new();
        assert!(!g.parse(&vec![0u8; 2048], 512));
    }
}
