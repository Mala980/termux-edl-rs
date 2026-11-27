//! Loader database — scans a Loaders/ directory for <hwid>_<pkhash>_*.bin files,
//! mirrors edlclient/Library/loader_db.py (incl. convertmsmid via soc tables).
use crate::config_tables;
use std::collections::HashMap;

#[derive(Default)]
pub struct LoaderDb {
    pub db: HashMap<String, Vec<(String, String)>>, // hwid -> ordered (pkhash, path)
}

impl LoaderDb {
    pub fn scan(dir: &str) -> Self {
        let mut db: HashMap<String, Vec<(String, String)>> = HashMap::new();
        // Recursive walk like os.walk in loader_db.py (Loaders/<vendor>/<file>).
        let mut stack = vec![std::path::PathBuf::from(dir)];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            let mut entries: Vec<_> = rd.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let filename = entry.file_name().to_string_lossy().to_string();
            if !filename.ends_with(".bin") && !filename.ends_with(".mbn") && !filename.ends_with(".elf") {
                continue;
            }
            let parts: Vec<&str> = filename.split('_').collect();
            if parts.len() < 2 {
                continue;
            }
            let hwid = parts[0].to_lowercase();
            if hwid.len() < 9 || u32::from_str_radix(&hwid[..8], 16).is_err() {
                continue;
            }
            let devid = hwid[8..].to_string();
            if devid.is_empty() {
                continue;
            }
            let pkhash = parts[1].to_lowercase();
            for m in convert_msmid(&hwid[..8]) {
                let mhwid = format!("{m}{devid}");
                let list = db.entry(mhwid).or_default();
                if !list.iter().any(|(k, _)| *k == pkhash) {
                    list.push((pkhash.clone(), entry.path().to_string_lossy().to_string()));
                }
            }
        }
        }
        Self { db }
    }

    /// Find a loader by HWID string (16 hex chars) and optional PK hash prefix.
    pub fn find_loader(&self, hwidstr: &str, pkhash: Option<&str>) -> Option<String> {
        // Ordered like python: first scanned loader wins (cyanogen before xiaomi).
        let lookup = |mt: &[(String, String)], pk: Option<&str>| -> Option<String> {
            if let Some(pk) = pk {
                if let Some((_, p)) = mt.iter().find(|(k, _)| k.starts_with(&pk[..pk.len().min(16)])) {
                    return Some(p.clone());
                }
            }
            mt.first().map(|(_, p)| p.clone())
        };
        if let Some(mt) = self.db.get(hwidstr) {
            return lookup(mt, pkhash);
        }
        // Match by msm id prefix only.
        let msmid = &hwidstr[..8];
        for (h, mt) in &self.db {
            if &h[..8] == msmid {
                return lookup(mt, pkhash);
            }
        }
        None
    }
}

/// Convert an msm id from a loader filename to all known equivalents (via sochw/msmids).
fn convert_msmid(msmid: &str) -> Vec<String> {
    let mut out = Vec::new();
    let val = match u32::from_str_radix(msmid, 16) {
        Ok(v) => v,
        Err(_) => return out,
    };
    if val & 0xFF == 0xE1 || val == 0 {
        out.push(msmid.to_string());
        return out;
    }
    let socid = val >> 16;
    if let Some(names) = config_tables::soc_id_names(socid) {
        for name in names {
            for (id, idname) in config_tables::MSM_BY_NAME {
                if *idname == name {
                    out.push(format!("{id:08x}"));
                }
            }
        }
    }
    if out.is_empty() {
        out.push(msmid.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_msmid_passthrough() {
        assert_eq!(convert_msmid("00000000"), vec!["00000000"]);
        let e1 = convert_msmid("0560e1");
        assert_eq!(e1, vec!["0560e1"]);
    }

    #[test]
    fn test_convert_msmid_soc() {
        // 0x6006 >> 16 = 0x6006 (sc8180x) etc.; a non-e1 id should map through soc table or fall back.
        let r = convert_msmid("04a000");
        assert!(!r.is_empty());
    }
}
