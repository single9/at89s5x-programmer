mod bitbang;
mod hexfile;
mod isp;

use std::fs;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use embedded_hal::digital::OutputPin;
use embedded_hal::spi::SpiBus;
use ftdi_embedded_hal as hal;
use ftdi_embedded_hal::libftd2xx::{Ft232h, Ftdi};

use isp::{At89s51Programmer, Chip, DEFAULT_FLASH_SIZE};

/// ISP programmer for AT89S51 / AT89S52, driven through an FT232RL or FT232H.
///
/// The target must be powered and have its own crystal running; the adapter
/// does not supply a clock or drive VCC. See the README for wiring.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// FTDI device serial number. If omitted, the first supported adapter found is used.
    #[arg(long, global = true)]
    serial: Option<String>,

    /// USB adapter type. Detected automatically from the USB IDs when omitted.
    /// ft232h uses MPSSE hardware SPI; ft232r (FT232RL) uses software SPI over
    /// synchronous bit-bang (D0=SCK, D1=MOSI, D2=MISO, D3=RST).
    #[arg(long, global = true, value_enum)]
    adapter: Option<Adapter>,

    /// SPI clock in Hz. Must stay below 1/16 of the target's crystal frequency.
    #[arg(long, global = true, default_value_t = 100_000)]
    clock_hz: u32,

    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum Adapter {
    Ft232h,
    Ft232r,
}

#[derive(Clone, Copy, ValueEnum)]
enum RstLevel {
    High,
    Low,
}

#[derive(Subcommand)]
enum Command {
    /// Read and print the device signature bytes.
    Info,
    /// Bench diagnostic: hold RST high and repeat Programming Enable so the
    /// pins can be probed; prints the reply whenever it changes.
    Probe {
        /// How long to run, in seconds.
        #[arg(long, default_value_t = 30)]
        seconds: u64,
    },
    /// Bench diagnostic: drive RST to a fixed level so pin 9 can be measured.
    Rst {
        /// `high` or `low`.
        level: RstLevel,
        /// How long to hold it, in seconds.
        #[arg(long, default_value_t = 20)]
        seconds: u64,
    },
    /// Adapter test with no target: jumper MOSI to MISO and check the echo.
    Selftest,
    /// Erase the whole chip (flash contents and lock bits).
    Erase,
    /// Write a HEX file to flash.
    Flash {
        file: PathBuf,
        /// Skip the chip-erase step before writing.
        #[arg(long)]
        no_erase: bool,
        /// Skip read-back verification after writing.
        #[arg(long)]
        no_verify: bool,
    },
    /// Read the flash contents back and compare against a HEX file.
    Verify { file: PathBuf },
}

fn open_ft232h(
    serial: Option<&str>,
    clock_hz: u32,
) -> Result<At89s51Programmer<hal::Spi<Ft232h>, hal::OutputPin<Ft232h>>> {
    let device = match serial {
        Some(serial) => Ft232h::with_serial_number(serial)
            .with_context(|| format!("failed to open FT232H with serial {serial}"))?,
        None => {
            let ftdi = Ftdi::new().context("no FTDI device found")?;
            Ft232h::try_from(ftdi).context("first FTDI device found is not an FT232H")?
        }
    };

    let hal = hal::FtHal::init_freq(device, clock_hz).context("failed to init MPSSE/SPI mode")?;
    let spi = hal.spi().context("failed to claim SPI bus (AD0/AD1/AD2)")?;
    let rst = hal.ad3().context("failed to claim AD3 as RST output")?;

    Ok(At89s51Programmer::new(spi, rst))
}

/// One line of tool output: a fixed-width label, then the message.
fn status(label: &str, msg: impl std::fmt::Display) {
    eprintln!("{label:<10}{msg}");
}

/// Progress bar on a terminal; a single summary line when output is piped.
struct Progress {
    label: &'static str,
    tty: bool,
    start: Instant,
    last_pct: usize,
}

impl Progress {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            tty: std::io::stderr().is_terminal(),
            start: Instant::now(),
            last_pct: usize::MAX,
        }
    }

    fn update(&mut self, done: usize, total: usize) {
        let pct = done * 100 / total.max(1);
        if !self.tty || pct == self.last_pct {
            return;
        }
        self.last_pct = pct;
        let filled = pct * 30 / 100;
        eprint!(
            "\r{:<10}[{}{}] {pct:>3}%  {done}/{total} B",
            self.label,
            "#".repeat(filled),
            "-".repeat(30 - filled)
        );
        let _ = std::io::stderr().flush();
    }

    fn finish(self, summary: impl std::fmt::Display) {
        if self.tty {
            eprint!("\r\x1b[2K");
        }
        status(
            self.label,
            format!("{summary}  ({:.1} s)", self.start.elapsed().as_secs_f32()),
        );
    }
}

/// Pick the adapter type from the USB IDs of the connected FTDI devices
/// (0403:6001 = FT232R, 0403:6014 = FT232H).
fn detect_adapter(serial: Option<&str>) -> Result<Adapter> {
    let mut first = None;
    for device in rusb::devices()
        .context("failed to enumerate USB devices")?
        .iter()
    {
        let Ok(desc) = device.device_descriptor() else {
            continue;
        };
        if desc.vendor_id() != 0x0403 {
            continue;
        }
        let adapter = match desc.product_id() {
            0x6001 => Adapter::Ft232r,
            0x6014 => Adapter::Ft232h,
            _ => continue,
        };
        if let Some(want) = serial {
            let matches = device
                .open()
                .ok()
                .and_then(|h| h.read_serial_number_string_ascii(&desc).ok())
                .as_deref()
                == Some(want);
            if !matches {
                continue;
            }
        }
        first.get_or_insert(adapter);
    }
    let adapter = first.context(
        "no supported adapter found (FT232R 0403:6001 or FT232H 0403:6014); check the USB connection, or pass --adapter",
    )?;
    Ok(adapter)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let adapter = match cli.adapter {
        Some(a) => a,
        None => detect_adapter(cli.serial.as_deref())?,
    };
    status(
        "Adapter",
        format!(
            "{}, SCK {} kHz",
            match adapter {
                Adapter::Ft232h => "FT232H (MPSSE)",
                Adapter::Ft232r => "FT232R (bit-bang)",
            },
            cli.clock_hz as f32 / 1000.0
        ),
    );
    match adapter {
        Adapter::Ft232h => {
            let prog = open_ft232h(cli.serial.as_deref(), cli.clock_hz)?;
            execute(prog, cli.command)
        }
        Adapter::Ft232r => {
            let (spi, rst) = bitbang::open(cli.serial.as_deref(), cli.clock_hz)?;
            execute(At89s51Programmer::new(spi, rst), cli.command)
        }
    }
}

fn execute<SPI, RST, E1, E2>(mut prog: At89s51Programmer<SPI, RST>, command: Command) -> Result<()>
where
    SPI: SpiBus<u8, Error = E1>,
    RST: OutputPin<Error = E2>,
    E1: std::error::Error + Send + Sync + 'static,
    E2: std::error::Error + Send + Sync + 'static,
{
    match command {
        Command::Selftest => return prog.loopback_test(),
        Command::Probe { seconds } => return prog.probe(seconds),
        Command::Rst { level, seconds } => {
            return prog.hold_rst(matches!(level, RstLevel::High), seconds);
        }
        _ => {}
    }
    prog.enter_programming_mode()?;
    let result = run(&mut prog, command);
    prog.exit_programming_mode()?;
    result
}

fn run<SPI, RST, E1, E2>(prog: &mut At89s51Programmer<SPI, RST>, command: Command) -> Result<()>
where
    SPI: SpiBus<u8, Error = E1>,
    RST: OutputPin<Error = E2>,
    E1: std::error::Error + Send + Sync + 'static,
    E2: std::error::Error + Send + Sync + 'static,
{
    let t0 = Instant::now();
    let signature = prog.read_signature()?;
    let chip = Chip::from_signature(signature);
    let sig = format!(
        "{:02X} {:02X} {:02X}",
        signature[0], signature[1], signature[2]
    );
    let flash_size = chip.map_or(DEFAULT_FLASH_SIZE, Chip::flash_size);
    match chip {
        Some(chip) => status(
            "Device",
            format!(
                "{}  signature {sig}  {} KB flash",
                chip.name(),
                chip.flash_size() / 1024
            ),
        ),
        None => status(
            "Device",
            format!(
                "unrecognised signature {sig}; assuming {} KB flash",
                DEFAULT_FLASH_SIZE / 1024
            ),
        ),
    }

    match command {
        Command::Info | Command::Selftest | Command::Probe { .. } | Command::Rst { .. } => {}
        Command::Erase => erase(prog)?,
        Command::Flash {
            file,
            no_erase,
            no_verify,
        } => {
            let image = load_hex(&file, flash_size)?;
            let used = image.iter().filter(|&&b| b != 0xFF).count();
            status("Image", format!("{}  {used} B to write", file.display()));

            if !no_erase {
                erase(prog)?;
            }

            let mut p = Progress::new("Writing");
            prog.write_flash(&image, |d, t| p.update(d, t))?;
            p.finish(format!("{used} B written"));

            if !no_verify {
                // After our own chip erase everything past the last data byte
                // is blank, so only the used range needs reading back.
                let end = if no_erase {
                    image.len()
                } else {
                    image.iter().rposition(|&b| b != 0xFF).map_or(0, |i| i + 1)
                };
                let mut p = Progress::new("Verifying");
                prog.verify_flash(&image[..end], |d, t| p.update(d, t))?;
                p.finish(format!("{end} B OK"));
            }
            status("Done", format!("{:.1} s total", t0.elapsed().as_secs_f32()));
        }
        Command::Verify { file } => {
            let image = load_hex(&file, flash_size)?;
            let mut p = Progress::new("Verifying");
            prog.verify_flash(&image, |d, t| p.update(d, t))?;
            p.finish(format!("{} B OK", image.len()));
        }
    }

    Ok(())
}

fn erase<SPI, RST, E1, E2>(prog: &mut At89s51Programmer<SPI, RST>) -> Result<()>
where
    SPI: SpiBus<u8, Error = E1>,
    RST: OutputPin<Error = E2>,
    E1: std::error::Error + Send + Sync + 'static,
    E2: std::error::Error + Send + Sync + 'static,
{
    let start = Instant::now();
    eprint!("{:<10}", "Erasing");
    let _ = std::io::stderr().flush();
    prog.chip_erase()?;
    eprintln!("chip erased  ({:.1} s)", start.elapsed().as_secs_f32());
    Ok(())
}

fn load_hex(file: &PathBuf, flash_size: usize) -> Result<Vec<u8>> {
    let text =
        fs::read_to_string(file).with_context(|| format!("failed to read {}", file.display()))?;
    let image = hexfile::load(&text, flash_size)?;
    ensure!(!image.is_empty(), "HEX file produced an empty image");
    Ok(image)
}
