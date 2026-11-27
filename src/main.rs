//! termux-edl-rs: Rust rewrite of the termux-edl branch of bkerler/edl
//! (Qualcomm Sahara / Firehose EDL client) for Android/Termux, Linux arm64/armv7.
//! GPLv3 — derived from B.Kerler's edl (https://github.com/bkerler/edl).


use anyhow::{anyhow, Result};
use termux_edl_rs::{firehose, gpt, loader_db, sahara, transport};
use clap::Parser as ClapParser;
use transport::{SerialTransport, TermuxTransport, Transport, UsbTransport};

/// Qualcomm Sahara / Firehose EDL Client (Rust) V1.0
#[derive(ClapParser, Debug)]
#[command(version, about, long_about = None, disable_help_subcommand = true)]
struct Args {
    /// Show the partition table
    #[arg(long)]
    printgpt: bool,
    /// Save gpt table to given directory
    #[arg(long)]
    gpt: Option<String>,
    /// Read flash to filename
    #[arg(long, num_args = 2)]
    r: Option<Vec<String>>,
    /// Read all partitions from flash to a directory
    #[arg(long)]
    rl: Option<String>,
    /// Read whole flash to file
    #[arg(long)]
    rf: Option<String>,
    /// Read sectors: <start_sector> <sectors> <filename>
    #[arg(long, num_args = 3)]
    rs: Option<Vec<String>>,
    /// Write filename to partition: <partitionname> <filename>
    #[arg(long, num_args = 2)]
    w: Option<Vec<String>>,
    /// Write all files from directory to flash
    #[arg(long)]
    wl: Option<String>,
    /// Write whole filename to flash
    #[arg(long)]
    wf: Option<String>,
    /// Write filename to flash at start_sector: <start_sector> <filename>
    #[arg(long, num_args = 2)]
    ws: Option<Vec<String>>,
    /// Erase partition
    #[arg(long)]
    e: Option<String>,
    /// Erase sectors: <start_sector> <sectors>
    #[arg(long, num_args = 2)]
    es: Option<Vec<String>>,
    /// Read crypto footer
    #[arg(long)]
    footer: Option<String>,
    /// Send firehose nop
    #[arg(long)]
    nop: bool,
    /// Send firehose reset command (reset|off|edl)
    #[arg(long)]
    reset: Option<Option<String>>,
    /// Print secureboot fields
    #[arg(long)]
    secureboot: bool,
    /// Dump primary bootloader
    #[arg(long)]
    pbl: Option<String>,
    /// Dump QFPROM fuses
    #[arg(long)]
    qfp: Option<String>,
    /// Print storage info
    #[arg(long)]
    getstorageinfo: bool,
    /// Enable verbose mode
    #[arg(long)]
    debugmode: bool,
    /// Use specific EDL loader, disable autodetection
    #[arg(long)]
    loader: Option<String>,
    /// Set usb vendor id used for EDL
    #[arg(long, default_value_t = 0xFFFF)]
    vid: u16,
    /// Set usb product id used for EDL
    #[arg(long, default_value_t = 0xFFFF)]
    pid: u16,
    /// Set lun to read/write from (UFS only)
    #[arg(long)]
    lun: Option<u32>,
    /// Set memory type (NAND, eMMC, UFS, spinor)
    #[arg(long)]
    memory: Option<String>,
    /// Set default sector size
    #[arg(long)]
    sectorsize: Option<u32>,
    /// Do not allow any writes to flash (simulate only)
    #[arg(long)]
    skipwrite: bool,
    /// Do not expect a response from phone on read/write
    #[arg(long)]
    skipresponse: bool,
    /// Skip storage initialisation
    #[arg(long)]
    skipstorageinit: bool,
    /// Use serial port (port autodetection)
    #[arg(long)]
    serial: bool,
    /// Set serial port name (e.g. /dev/ttyUSB0)
    #[arg(long)]
    portname: Option<String>,
    /// Set the maximum payload for EDL
    #[arg(long, default_value_t = 0x100000)]
    maxpayload: u32,
    /// List connected EDL devices and exit
    #[arg(long)]
    list: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("Qualcomm Sahara / Firehose EDL Client (Rust) V1.0 (c) B.Kerler 2018-2025, rust port Mala980.");

    if args.list && on_termux() {
        let nodes = TermuxTransport::list_nodes();
        if nodes.is_empty() {
            println!("No EDL devices found.");
        }
        for node in &nodes {
            if let Ok(mut t) = TermuxTransport::connect_node(node) {
                if let Ok(d) = t.device_descriptor() {
                    println!("EDL device candidate {node}: {:04x}:{:04x}", d.vendor_id(), d.product_id());
                }
                t.close();
            } else {
                println!("{node} (no access — run termux-usb to grant)");
            }
        }
        std::process::exit(0);
    }
    if args.list {
        let devs = UsbTransport::list_devices();
        if devs.is_empty() {
            println!("No EDL devices found.");
        } else {
            for (vid, pid) in devs {
                println!("EDL device: {vid:04x}:{pid:04x}");
            }
        }
        return Ok(());
    }

    let has_cmd = args.printgpt
        || args.gpt.is_some()
        || args.r.is_some()
        || args.rl.is_some()
        || args.rf.is_some()
        || args.rs.is_some()
        || args.w.is_some()
        || args.wl.is_some()
        || args.wf.is_some()
        || args.ws.is_some()
        || args.e.is_some()
        || args.es.is_some()
        || args.footer.is_some()
        || args.nop
        || args.reset.is_some()
        || args.secureboot
        || args.pbl.is_some()
        || args.qfp.is_some()
        || args.getstorageinfo;
    if !has_cmd {
        eprintln!("No command given. Use --help for usage.");
        std::process::exit(1);
    }

    run(args)
}

fn open_transport(args: &Args) -> Result<Box<dyn Transport>> {
    if args.serial || args.portname.is_some() {
        let path = match &args.portname {
            Some(p) => p.clone(),
            None => SerialTransport::available_ports()
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("No serial ports detected"))?,
        };
        println!("[edl] Using serial port {path}");
        return Ok(Box::new(SerialTransport::open(&path)?));
    }
    if on_termux() {
        // Termux: must use termux-usb fd passing. Find the matching node by
        // opening each candidate and comparing VID/PID (like python _try_device).
        let (vid, pid) = resolve_vid_pid(args);
        let nodes = TermuxTransport::list_nodes();
        for node in &nodes {
            match TermuxTransport::connect_node(node) {
                Ok(mut t) => {
                    // Check identity: read descriptor via the wrapped handle.
                    if let Ok(desc) = t.device_descriptor() {
                        if desc.vendor_id() == vid && desc.product_id() == pid {
                            println!("Detected EDL device {vid:04x}:{pid:04x} at {node}");
                            return Ok(Box::new(t));
                        }
                    }
                    // Not a match; close and continue.
                    t.close();
                }
                Err(e) => eprintln!("[LIB] {node}: {e}"),
            }
        }
        anyhow::bail!("No EDL device {vid:04x}:{pid:04x} found via termux-usb. Grant USB permission and replug.");
    }
    Ok(Box::new(UsbTransport::new()))
}

/// True when running inside Termux (Android).
fn on_termux() -> bool {
    std::env::var("TERMUX_VERSION").is_ok()
        || std::env::var("PREFIX").map(|p| p.contains("com.termux")).unwrap_or(false)
        || std::path::Path::new("/data/data/com.termux").exists()
}

/// Expose device_descriptor() on TermuxTransport for identity check.
trait DeviceDescriptor {
    fn device_descriptor(&self) -> Result<rusb::DeviceDescriptor>;
}

fn resolve_vid_pid(args: &Args) -> (u16, u16) {
    if args.vid != 0xFFFF && args.pid != 0xFFFF {
        return (args.vid, args.pid);
    }
    let devs = UsbTransport::list_devices();
    if let Some(&d) = devs.first() {
        return d;
    }
    // Fall back to the canonical Qualcomm EDL id.
    (0x05c6, 0x9008)
}

fn run(args: Args) -> Result<()> {
    let mut t = open_transport(&args)?;
    let (vid, pid) = resolve_vid_pid(&args);
    t.connect(vid, pid)?;

    // --- Sahara stage ---
    let mut sah = sahara::Sahara::new(t.as_mut(), args.debugmode);
    let mut mode = sah.connect();
    if mode == "error" {
        return Err(anyhow!("Connection detected as error, quitting."));
    }

    let loaderdb = loader_db::LoaderDb::scan("Loaders");

    let mut programmer = args.loader.clone();
    if mode == "sahara" {
        let version = sah.info.version.max(2);
        if sah.cmd_info(version, &loaderdb) {
            if programmer.is_none() {
                programmer = sah.programmer.clone();
            }
        }
        if let Some(prog) = &programmer {
            mode = sah.upload_loader(version, prog)?;
        } else if mode == "sahara" && args.memory.is_none() {
            // No loader: only memory-debug / nop / reset paths make sense.
            if args.reset.is_some() || args.nop {
                return Err(anyhow!("No suitable loader found; --reset/--nop require a firehose loader (--loader path)"));
            }
            return Err(anyhow!(
                "Error, couldn't find suitable enprg/nprg loader :( — place loaders in ./Loaders or pass --loader"
            ));
        }
    }

    if mode == "nandprg" || mode == "enandprg" {
        return Err(anyhow!(
            "Streaming ({mode}) mode detected: streaming read/write is only partially implemented in this Rust port; use firehose loaders."
        ));
    }

    if mode != "firehose" {
        return Err(anyhow!("Failed to reach firehose mode (mode={mode})"));
    }

    println!("[edl] Successfully uploaded programmer :)");
    std::thread::sleep(std::time::Duration::from_millis(300));

    // --- Firehose stage ---
    let mut fh = firehose::Firehose::new(t.as_mut());
    fh.connect(); // drain loader boot log (python firehose.connect())
    fh.skipresponse = args.skipresponse;
    fh.debug = args.debugmode;
    if let Some(m) = &args.memory {
        fh.cfg.memory_name = m.clone();
    } else {
        fh.cfg.memory_name = "eMMC".into();
    }
    if let Some(s) = args.sectorsize {
        fh.cfg.sector_size = s;
    }
    fh.cfg.skip_storage_init = args.skipstorageinit;
    fh.cfg.skip_write = args.skipwrite;
    fh.cfg.max_payload_size_to_target = args.maxpayload;
    fh.configure()?;

    let luns = fh.getluns(args.lun);
    let ss = fh.cfg.sector_size;

    if args.nop {
        fh.cmd_nop()?;
        return Ok(());
    }

    if args.getstorageinfo {
        fh.cmd_getstorageinfo()?;
        return Ok(());
    }

    if let Some(mode_r) = &args.reset {
        let mode = mode_r.clone().unwrap_or_else(|| "reset".into());
        fh.cmd_reset(&mode);
        return Ok(());
    }

    if args.printgpt {
        for lun in &luns {
            match fh.get_gpt(*lun, Some(ss))? {
                Some((_, g)) => {
                    println!("\nParsing Lun {lun}:");
                    println!("{}", g.tostring());
                }
                None => eprintln!("Couldn't parse GPT on lun {lun}"),
            }
        }
        return Ok(());
    }

    if let Some(dir) = &args.gpt {
        std::fs::create_dir_all(dir)?;
        for lun in &luns {
            if let Some((data, g)) = fh.get_gpt(*lun, Some(ss))? {
                std::fs::write(format!("{dir}/gpt_main{lun}.bin"), &data)?;
                println!("Dumped GPT from Lun {lun} to {dir}/gpt_main{lun}.bin");
                let backup = data[ss as usize * 2..].to_vec();
                std::fs::write(format!("{dir}/gpt_backup{lun}.bin"), backup)?;
            } else {
                eprintln!("Couldn't fetch GPT on lun {lun}");
            }
        }
        return Ok(());
    }

    if let Some(parts) = &args.r {
        let (partitionname, filename) = (parts[0].clone(), parts[1].clone());
        let parts_list: Vec<&str> = partitionname.split(',').collect();
        let files: Vec<&str> = filename.split(',').collect();
        if parts_list.len() != files.len() {
            return Err(anyhow!("You need to give as many filenames as given partitions."));
        }
        for (i, partition) in parts_list.iter().enumerate() {
            if *partition == "gpt" {
                for lun in &luns {
                    let out = format!("{}.lun{lun}", files[i]);
                    fh.cmd_read(*lun, 0, 32, &out, true)?;
                }
                continue;
            }
            let mut done = false;
            for lun in &luns {
                if let Some((_, g)) = fh.get_gpt(*lun, Some(ss))? {
                    if let Some(p) = g.partentries.get(partition.to_lowercase().as_str())
                        .or_else(|| g.partentries.get(partition as &str))
                    {
                        if fh.cmd_read(*lun, p.sector, p.sectors, files[i], true)? {
                            done = true;
                        }
                        break;
                    }
                }
            }
            if !done {
                eprintln!("Error: Couldn't detect partition: {partition}");
            }
        }
        return Ok(());
    }

    if let Some(dir) = &args.rl {
        std::fs::create_dir_all(dir)?;
        for lun in &luns {
            let storedir = if luns.len() > 1 {
                let d = format!("{dir}/lun{lun}");
                std::fs::create_dir_all(&d)?;
                d
            } else {
                dir.clone()
            };
            if let Some((data, g)) = fh.get_gpt(*lun, Some(ss))? {
                std::fs::write(format!("{storedir}/gpt_main{lun}.bin"), &data)?;
                for (name, p) in &g.partentries {
                    let out = format!("{storedir}/{name}.bin");
                    println!("Dumping partition {name} with sector count {} as {out}.", p.sectors);
                    let _ = fh.cmd_read(*lun, p.sector, p.sectors, &out, true);
                }
            } else {
                eprintln!("Couldn't fetch GPT on lun {lun}");
            }
        }
        return Ok(());
    }

    if let Some(filename) = &args.rf {
        for lun in &luns {
            if let Some((_, g)) = fh.get_gpt(*lun, Some(ss))? {
                let out = if luns.len() > 1 { format!("{filename}.lun{lun}") } else { filename.clone() };
                println!("Dumping flash with sector count {} as {out}.", g.totalsectors);
                fh.cmd_read(*lun, 0, g.totalsectors, &out, true)?;
            }
        }
        return Ok(());
    }

    if let Some(parts) = &args.rs {
        let (start, nsect, filename) = (parts[0].clone(), parts[1].clone(), parts[2].clone());
        let start: u64 = parse_num(&start)?;
        let nsect: u64 = parse_num(&nsect)?;
        for lun in &luns {
            let out = if luns.len() > 1 { format!("{filename}.lun{lun}") } else { filename.clone() };
            fh.cmd_read(*lun, start, nsect, &out, true)?;
        }
        return Ok(());
    }

    if let Some(parts) = &args.w {
        if args.skipwrite {
            eprintln!("[edl] --skipwrite set: refusing to write.");
            return Ok(());
        }
        let (partitionname, filename) = (parts[0].clone(), parts[1].clone());
        let parts_list: Vec<&str> = partitionname.split(',').collect();
        let files: Vec<&str> = filename.split(',').collect();
        if parts_list.len() != files.len() {
            return Err(anyhow!("You need to give as many filenames as given partitions."));
        }
        for (i, partition) in parts_list.iter().enumerate() {
            let size = std::fs::metadata(files[i])?.len();
            let sectors = size.div_ceil(ss as u64);
            let (lun, start) = if partition.eq_ignore_ascii_case("gpt") {
                (args.lun.unwrap_or(0), 0)
            } else {
                let mut found = None;
                for l in &luns {
                    if let Some((_, g)) = fh.get_gpt(*l, Some(ss))? {
                        if let Some(p) = g.partentries.get(partition.to_lowercase().as_str())
                            .or_else(|| g.partentries.get(partition as &str))
                        {
                            if sectors > p.sectors {
                                return Err(anyhow!(
                                    "Error: {} has {sectors} sectors but partition only has {}.",
                                    files[i], p.sectors
                                ));
                            }
                            found = Some((*l, p.sector));
                            break;
                        }
                    }
                }
                found.ok_or_else(|| anyhow!("Couldn't detect partition: {partition}"))?
            };
            if fh.cmd_program(lun, start, files[i], true)? {
                println!("Wrote {} to sector {start}.", files[i]);
            } else {
                eprintln!("Error writing {} to sector {start}.", files[i]);
            }
        }
        return Ok(());
    }

    if let Some(dir) = &args.wl {
        if args.skipwrite {
            eprintln!("[edl] --skipwrite set: refusing to write.");
            return Ok(());
        }
        let lun = args.lun.unwrap_or(0);
        let (_, g) = fh
            .get_gpt(lun, Some(ss))?
            .ok_or_else(|| anyhow!("Can not fetch GPT table from device"))?;
        for entry in std::fs::read_dir(dir)?.flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let stem = name
                .strip_suffix(".bin")
                .or_else(|| name.strip_suffix(".img"))
                .or_else(|| name.strip_suffix(".mbn"))
                .unwrap_or(&name);
            if let Some(p) = g.partentries.get(stem) {
                println!("Writing {} to partition {}.", path.display(), p.name);
                let _ = fh.cmd_program(lun, p.sector, path.to_str().unwrap(), true);
            } else {
                println!("Can't find a partition named {stem} in the gpt, but continuing anyway.");
            }
        }
        return Ok(());
    }

    if let Some(filename) = &args.wf {
        if args.skipwrite {
            eprintln!("[edl] --skipwrite set: refusing to write.");
            return Ok(());
        }
        let lun = args.lun.unwrap_or(0);
        let size = std::fs::metadata(filename)?.len();
        let sectors = size.div_ceil(ss as u64);
        fh.cmd_program(lun, 0, filename, true)?;
        let _ = sectors;
        return Ok(());
    }

    if let Some(parts) = &args.ws {
        if args.skipwrite {
            eprintln!("[edl] --skipwrite set: refusing to write.");
            return Ok(());
        }
        let (start, filename) = (parts[0].clone(), parts[1].clone());
        let start: u64 = parse_num(&start)?;
        let lun = args.lun.unwrap_or(0);
        fh.cmd_program(lun, start, &filename, true)?;
        return Ok(());
    }

    if let Some(partition) = &args.e {
        if args.skipwrite {
            eprintln!("[edl] --skipwrite set: refusing to write.");
            return Ok(());
        }
        let lun = args.lun.unwrap_or(0);
        let (_, g) = fh
            .get_gpt(lun, Some(ss))?
            .ok_or_else(|| anyhow!("Couldn't fetch GPT"))?;
        let p = g
            .partentries
            .get(&partition.to_lowercase())
            .or_else(|| g.partentries.get(partition as &str))
            .ok_or_else(|| anyhow!("Couldn't detect partition: {partition}"))?;
        fh.cmd_erase(lun, p.sector, p.sectors, true)?;
        return Ok(());
    }

    if let Some(parts) = &args.es {
        if args.skipwrite {
            eprintln!("[edl] --skipwrite set: refusing to write.");
            return Ok(());
        }
        let start: u64 = parse_num(&parts[0])?;
        let nsect: u64 = parse_num(&parts[1])?;
        let lun = args.lun.unwrap_or(0);
        fh.cmd_erase(lun, start, nsect, true)?;
        return Ok(());
    }

    if let Some(filename) = &args.footer {
        for lun in &luns {
            // crypto footer lives in the last sectors; read the tail of the disk
            if let Some((_, g)) = fh.get_gpt(*lun, Some(ss))? {
                let end = g.totalsectors;
                let start = end.saturating_sub(16);
                fh.cmd_read(*lun, start, 16, filename, true)?;
                println!("Dumped footer area to {filename}");
            }
        }
        return Ok(());
    }

    if args.secureboot || args.pbl.is_some() || args.qfp.is_some() {
        return Err(anyhow!("secureboot/pbl/qfp need peek support in the loader; not implemented in this Rust port yet."));
    }

    Ok(())
}

fn parse_num(s: &str) -> Result<u64> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(Into::into)
    } else {
        s.parse().map_err(Into::into)
    }
}
