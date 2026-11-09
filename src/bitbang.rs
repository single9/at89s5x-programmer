//! Software SPI over FT232R/FT232RL synchronous bit-bang mode.
//!
//! The FT232R has no MPSSE engine, so SCK/MOSI/MISO/RST are toggled by
//! writing one byte per pin-state change; in synchronous bit-bang mode every
//! byte written also latches one byte of pin readback, which is how MISO is
//! sampled.
//!
//! Talks to the chip directly over libusb (rusb), so the kernel's `ftdi_sio`
//! driver is detached automatically and no D2XX library is needed.
//!
//! Pin mapping (bit number == FT232R bit-bang data bit):
//!   D0 (TXD)  -> SCK   (output)
//!   D1 (RXD)  -> MOSI  (output)
//!   D2 (RTS#) -> MISO  (input)
//!   D3 (CTS#) -> RST   (output)

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use anyhow::{Context, Result};
use embedded_hal::digital::{self, OutputPin};
use embedded_hal::spi::{self, SpiBus};
use rusb::{Direction, Recipient, RequestType, request_type};
use std::time::{Duration, Instant};

const SCK: u8 = 1 << 0;
const MOSI: u8 = 1 << 1;
const MISO: u8 = 1 << 2;
const RST: u8 = 1 << 3;
const OUTPUT_MASK: u8 = SCK | MOSI | RST;

/// What to check when the adapter cannot be opened or claimed.
const PLATFORM_HINT: &str = if cfg!(target_os = "windows") {
    "Windows: bind the WinUSB driver to the adapter with Zadig; see the README"
} else if cfg!(target_os = "macos") {
    "macOS: the system FTDI driver may own the device; try sudo or unload it, see the README"
} else {
    "Linux: USB permissions? run scripts/setup-linux.sh, see the README"
};

const FTDI_VID: u16 = 0x0403;
const FT232R_PID: u16 = 0x6001;
const EP_OUT: u8 = 0x02;
const EP_IN: u8 = 0x81;
const USB_PACKET: usize = 64;
/// Samples sent before reading back. The FT232R holds 256 bytes of readback,
/// so stay below that; bigger batches mean fewer USB round trips.
const BATCH: usize = 240;
const TIMEOUT: Duration = Duration::from_secs(1);

// FTDI vendor requests.
const REQ_RESET: u8 = 0x00;
const REQ_SET_BAUDRATE: u8 = 0x03;
const REQ_SET_LATENCY: u8 = 0x09;
const REQ_SET_BITMODE: u8 = 0x0B;
const MODE_RESET: u8 = 0x00;
const MODE_SYNC_BITBANG: u8 = 0x04;

#[derive(Debug)]
pub struct BitbangError(String);

impl fmt::Display for BitbangError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BitbangError {}

impl spi::Error for BitbangError {
    fn kind(&self) -> spi::ErrorKind {
        spi::ErrorKind::Other
    }
}

impl digital::Error for BitbangError {
    fn kind(&self) -> digital::ErrorKind {
        digital::ErrorKind::Other
    }
}

struct Inner {
    dev: rusb::DeviceHandle<rusb::GlobalContext>,
    /// Last output pin state, so a pin change keeps the other outputs intact.
    out: u8,
}

fn usb_err(what: &str, e: rusb::Error) -> BitbangError {
    BitbangError(format!("FT232R {what}: {e}"))
}

impl Inner {
    fn control(&self, request: u8, value: u16, index: u16) -> Result<(), BitbangError> {
        let rt = request_type(Direction::Out, RequestType::Vendor, Recipient::Device);
        self.dev
            .write_control(rt, request, value, index, &[], TIMEOUT)
            .map(|_| ())
            .map_err(|e| usb_err("control transfer", e))
    }

    /// Clock `pattern` out as consecutive bit-bang samples and return the
    /// pin readback for each one.
    fn run(&mut self, pattern: &[u8]) -> Result<Vec<u8>, BitbangError> {
        let mut rx = Vec::with_capacity(pattern.len());
        for chunk in pattern.chunks(BATCH) {
            self.dev
                .write_bulk(EP_OUT, chunk, TIMEOUT)
                .map_err(|e| usb_err("bulk write", e))?;

            // Every IN packet starts with two modem-status bytes; strip them.
            let want = rx.len() + chunk.len();
            let deadline = Instant::now() + TIMEOUT;
            let mut buf = [0u8; USB_PACKET];
            while rx.len() < want {
                if Instant::now() > deadline {
                    return Err(BitbangError(
                        "FT232R: timed out waiting for readback".into(),
                    ));
                }
                let n = match self.dev.read_bulk(EP_IN, &mut buf, TIMEOUT) {
                    Ok(n) => n,
                    Err(rusb::Error::Timeout) => continue,
                    Err(e) => return Err(usb_err("bulk read", e)),
                };
                if n > 2 {
                    rx.extend_from_slice(&buf[2..n]);
                }
            }
        }
        if let Some(&last) = pattern.last() {
            self.out = last;
        }
        Ok(rx)
    }

    fn spi_transfer(&mut self, tx: &[u8]) -> Result<Vec<u8>, BitbangError> {
        let rst = self.out & RST;
        let mut pattern = Vec::with_capacity(tx.len() * 16 + 1);
        for &byte in tx {
            for bit in (0..8).rev() {
                let mosi = if byte >> bit & 1 != 0 { MOSI } else { 0 };
                pattern.push(rst | mosi); // SCK low: data setup
                pattern.push(rst | mosi | SCK); // SCK high: target samples MOSI
            }
        }
        // Trailing sample: readback lags the output by one entry, so this one
        // carries the pin state produced by the final SCK-high entry.
        pattern.push(rst);

        let rx = self.run(&pattern)?;

        // MISO is stable while SCK is high; it appears one entry later.
        let mut out = Vec::with_capacity(tx.len());
        for byte_idx in 0..tx.len() {
            let mut value = 0u8;
            for bit in 0..8 {
                let sample = rx[(byte_idx * 8 + bit) * 2 + 2];
                value = value << 1 | u8::from(sample & MISO != 0);
            }
            out.push(value);
        }
        Ok(out)
    }

    fn set_rst(&mut self, high: bool) -> Result<(), BitbangError> {
        let state = if high {
            self.out | RST
        } else {
            self.out & !RST
        };
        // Second entry: readback lags by one sample, so it shows the settled pins.
        let rx = self.run(&[state, state])?;
        if std::env::var_os("AT89S5X_ISP_TRACE").is_some() {
            let p = rx[1];
            eprintln!(
                "trace: drove RST {} -> pins read back: SCK={} MOSI={} MISO={} RST={}",
                if high { "high" } else { "low" },
                p & SCK != 0,
                p & MOSI != 0,
                p & MISO != 0,
                p & RST != 0
            );
        }
        Ok(())
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Give the pins back to the UART and the interface back to the kernel.
        let _ = self.control(REQ_SET_BITMODE, u16::from(MODE_RESET) << 8, 1);
        let _ = self.dev.release_interface(0);
    }
}

pub struct BitbangSpi(Rc<RefCell<Inner>>);
pub struct BitbangRst(Rc<RefCell<Inner>>);

impl spi::ErrorType for BitbangSpi {
    type Error = BitbangError;
}

impl SpiBus<u8> for BitbangSpi {
    fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        let rx = self.0.borrow_mut().spi_transfer(&vec![0u8; words.len()])?;
        words.copy_from_slice(&rx);
        Ok(())
    }

    fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
        self.0.borrow_mut().spi_transfer(words).map(|_| ())
    }

    fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
        let len = read.len().max(write.len());
        let mut tx = write.to_vec();
        tx.resize(len, 0);
        let rx = self.0.borrow_mut().spi_transfer(&tx)?;
        read.copy_from_slice(&rx[..read.len()]);
        Ok(())
    }

    fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        let rx = self.0.borrow_mut().spi_transfer(words)?;
        words.copy_from_slice(&rx);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl digital::ErrorType for BitbangRst {
    type Error = BitbangError;
}

impl OutputPin for BitbangRst {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().set_rst(false)
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().set_rst(true)
    }
}

/// Encode an FTDI baud divisor (FT232R: 48 MHz clock, 16x oversampling).
fn baud_to_value_index(baud: u32) -> (u16, u16) {
    const FRAC_CODE: [u32; 8] = [0, 3, 2, 4, 1, 5, 6, 7];
    let baud = baud.max(184); // lowest rate the divisor can express
    let divisor = 48_000_000 / baud; // in 1/16 units
    let best = (divisor / 2 + divisor % 2).min(0x1FFFF); // in 1/8 units
    let mut encoded = (best >> 3) | (FRAC_CODE[(best & 7) as usize] << 14);
    // Divisors 1 and 0x4001 have special meanings on the wire.
    if encoded == 1 {
        encoded = 0;
    } else if encoded == 0x4001 {
        encoded = 1;
    }
    ((encoded & 0xFFFF) as u16, ((encoded >> 16) & 0xFFFF) as u16)
}

/// Open an FT232R and return its SPI bus and RST pin.
///
/// `clock_hz` is the SCK frequency. In synchronous bit-bang mode the sample
/// rate is `baud * 16` and each SPI bit takes two samples, so `baud = clock / 8`.
pub fn open(serial: Option<&str>, clock_hz: u32) -> Result<(BitbangSpi, BitbangRst)> {
    let mut found = None;
    for device in rusb::devices()
        .context("failed to enumerate USB devices")?
        .iter()
    {
        let Ok(desc) = device.device_descriptor() else {
            continue;
        };
        if desc.vendor_id() != FTDI_VID || desc.product_id() != FT232R_PID {
            continue;
        }
        let handle = match device.open() {
            Ok(h) => h,
            Err(e) => {
                eprintln!("warning: found an FT232R but could not open it: {e} ({PLATFORM_HINT})");
                continue;
            }
        };
        if let Some(want) = serial
            && handle
                .read_serial_number_string_ascii(&desc)
                .ok()
                .as_deref()
                != Some(want)
        {
            continue;
        }
        found = Some(handle);
        break;
    }
    let dev = found.context("no FT232R (0403:6001) found or accessible")?;

    // Linux can detach `ftdi_sio` automatically. Windows and macOS report
    // "not supported"; that is fine, claiming the interface below decides
    // whether the device is usable.
    match dev.set_auto_detach_kernel_driver(true) {
        Ok(()) | Err(rusb::Error::NotSupported) => {}
        Err(e) => {
            return Err(
                anyhow::Error::new(e).context("failed to enable automatic kernel-driver detach")
            );
        }
    }
    dev.claim_interface(0).with_context(|| {
        format!(
            "failed to claim the USB interface (is another program using it?) ({PLATFORM_HINT})"
        )
    })?;

    let mut inner = Inner { dev, out: 0 };
    inner.control(REQ_RESET, 0, 1)?; // reset
    let (value, index) = baud_to_value_index((clock_hz / 8).max(1));
    inner.control(REQ_SET_BAUDRATE, value, index)?;
    inner.control(REQ_SET_LATENCY, 1, 1)?; // 1 ms latency timer
    inner.control(
        REQ_SET_BITMODE,
        u16::from(OUTPUT_MASK) | u16::from(MODE_SYNC_BITBANG) << 8,
        1,
    )?;
    inner.control(REQ_RESET, 1, 1)?; // purge RX
    inner.control(REQ_RESET, 2, 1)?; // purge TX
    inner.set_rst(false).map_err(anyhow::Error::new)?; // all outputs low

    let shared = Rc::new(RefCell::new(inner));
    Ok((BitbangSpi(shared.clone()), BitbangRst(shared)))
}
