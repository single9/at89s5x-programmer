mod bitbang;
mod hexfile;
mod isp;

use std::fs;
use std::path::PathBuf;

use anyhow::{ensure, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use embedded_hal::digital::OutputPin;
use embedded_hal::spi::SpiBus;
use ftdi_embedded_hal as hal;
use ftdi_embedded_hal::libftd2xx::{Ft232h, Ftdi};

use isp::{At89s51Programmer, Chip, DEFAULT_FLASH_SIZE};

/// AT89S51 SPI ISP programmer, driven through an FT232H USB-SPI adapter.
///
/// Wiring (FT232H -> AT89S51 DIP-40):
///   AD0 (SCK)  -> P1.7 (SCK, pin 8)
///   AD1 (MOSI) -> P1.5 (MOSI, pin 6)
///   AD2 (MISO) -> P1.6 (MISO, pin 7)
///   AD3        -> RST (pin 9)
///   GND        -> GND (pin 20)
/// The target must have its own crystal/oscillator running and be powered;
/// the FT232H does not supply a clock or drive VCC.
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

fn print_progress(done: usize, total: usize) {
    if done % 256 == 0 || done == total {
        eprint!("\r  {done}/{total} bytes");
    }
    if done == total {
        eprintln!();
    }
}

/// Pick the adapter type from the USB IDs of the connected FTDI devices
/// (0403:6001 = FT232R, 0403:6014 = FT232H).
fn detect_adapter(serial: Option<&str>) -> Result<Adapter> {
    let mut first = None;
    for device in rusb::devices().context("failed to enumerate USB devices")?.iter() {
        let Ok(desc) = device.device_descriptor() else { continue };
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
    eprintln!(
        "Detected adapter: {}",
        match adapter {
            Adapter::Ft232h => "FT232H",
            Adapter::Ft232r => "FT232R",
        }
    );
    Ok(adapter)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let adapter = match cli.adapter {
        Some(a) => a,
        None => detect_adapter(cli.serial.as_deref())?,
    };
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
            return prog.hold_rst(matches!(level, RstLevel::High), seconds)
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
    let signature = prog.read_signature()?;
    let chip = Chip::from_signature(signature);
    eprintln!(
        "Signature: {:02X} {:02X} {:02X} ({})",
        signature[0],
        signature[1],
        signature[2],
        match chip {
            Some(chip) => chip.name().to_string(),
            None => format!("unrecognised - assuming {DEFAULT_FLASH_SIZE} bytes flash, continuing anyway"),
        }
    );
    let flash_size = chip.map_or(DEFAULT_FLASH_SIZE, Chip::flash_size);

    match command {
        Command::Info | Command::Selftest | Command::Probe { .. } | Command::Rst { .. } => {}
        Command::Erase => {
            eprintln!("Erasing chip...");
            prog.chip_erase()?;
            eprintln!("Done.");
        }
        Command::Flash {
            file,
            no_erase,
            no_verify,
        } => {
            let image = load_hex(&file, flash_size)?;

            if !no_erase {
                eprintln!("Erasing chip...");
                prog.chip_erase()?;
            }

            eprintln!("Writing {} bytes...", image.len());
            prog.write_flash(&image, print_progress)?;

            if !no_verify {
                eprintln!("Verifying...");
                prog.verify_flash(&image, print_progress)?;
                eprintln!("Verify OK.");
            }
        }
        Command::Verify { file } => {
            let image = load_hex(&file, flash_size)?;
            eprintln!("Verifying {} bytes...", image.len());
            prog.verify_flash(&image, print_progress)?;
            eprintln!("Verify OK.");
        }
    }

    Ok(())
}

fn load_hex(file: &PathBuf, flash_size: usize) -> Result<Vec<u8>> {
    let text = fs::read_to_string(file).with_context(|| format!("failed to read {}", file.display()))?;
    let image = hexfile::load(&text, flash_size)?;
    ensure!(!image.is_empty(), "HEX file produced an empty image");
    Ok(image)
}
