//! AT89S51 SPI-based In-System Programming (ISP) protocol.
//!
//! Reference: Atmel AT89S51 datasheet, "Serial Programming Instruction Set".
//! SCK must stay below 1/16 of the target's crystal frequency, so keep the
//! bus clock conservative (the CLI defaults to 100 kHz).

use std::thread::sleep;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use embedded_hal::digital::OutputPin;
use embedded_hal::spi::SpiBus;

/// Flash size assumed when the signature is not recognised.
pub const DEFAULT_FLASH_SIZE: usize = 4096;

/// Supported chips, identified by their signature bytes
/// (0x000 = 0x1E Atmel, 0x100 = device id, 0x200 = 0x06).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    At89s51,
    At89s52,
}

impl Chip {
    pub fn from_signature(sig: [u8; 3]) -> Option<Self> {
        match sig {
            [0x1E, 0x51, 0x06] => Some(Chip::At89s51),
            [0x1E, 0x52, 0x06] => Some(Chip::At89s52),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Chip::At89s51 => "AT89S51",
            Chip::At89s52 => "AT89S52",
        }
    }

    pub fn flash_size(self) -> usize {
        match self {
            Chip::At89s51 => 4096,
            Chip::At89s52 => 8192,
        }
    }
}

/// Delay after each written byte, waiting for the internal write cycle to finish.
const BYTE_WRITE_DELAY: Duration = Duration::from_millis(4);
/// Delay after a chip-erase command, waiting for the erase cycle to finish.
const CHIP_ERASE_DELAY: Duration = Duration::from_millis(500);
/// Delay after asserting RST, letting the crystal oscillator restart/stabilize.
const RESET_SETTLE_DELAY: Duration = Duration::from_millis(20);

pub struct At89s51Programmer<SPI, RST> {
    spi: SPI,
    rst: RST,
}

impl<SPI, RST, E1, E2> At89s51Programmer<SPI, RST>
where
    SPI: SpiBus<u8, Error = E1>,
    RST: OutputPin<Error = E2>,
    E1: std::error::Error + Send + Sync + 'static,
    E2: std::error::Error + Send + Sync + 'static,
{
    pub fn new(spi: SPI, rst: RST) -> Self {
        Self { spi, rst }
    }

    /// Assert RST, wait for the oscillator to settle, then issue Programming
    /// Enable. Retries once with a fresh RST pulse, since the datasheet notes
    /// the first attempt can occasionally fail if SCK wasn't stable at power-up.
    pub fn enter_programming_mode(&mut self) -> Result<()> {
        let mut last = [0u8; 4];
        for attempt in 0..2 {
            self.pulse_reset()?;

            let mut buf = [0xAC, 0x53, 0x00, 0x00];
            self.spi
                .transfer_in_place(&mut buf)
                .context("SPI transfer failed during Programming Enable")?;

            // AT89S51/52 answer Programming Enable with 0x69 in byte 4
            // (datasheet, Serial Programming Instruction Set). The 0x53 echo
            // in byte 3 is AVR behaviour, not 8051.
            if buf[3] == 0x69 {
                return Ok(());
            }
            last = buf;

            if attempt == 0 {
                eprintln!("Programming Enable reply mismatch, retrying with a fresh RST pulse...");
            }
        }
        bail!(
            "Programming Enable failed: no 0x69 reply from the target (last reply: {:02X} {:02X} {:02X} {:02X}).\n\
             An all-00 reply usually means MISO is stuck low / not connected; all-FF means it floats high.\n\
             Check wiring (RST/SCK/MOSI/MISO), that the target has power, and\n\
             that its crystal/oscillator is actually running.",
            last[0], last[1], last[2], last[3]
        );
    }

    /// Adapter self-test: with MOSI jumpered to MISO (and no target needed),
    /// every byte clocked out must come straight back. Does not touch RST.
    pub fn loopback_test(&mut self) -> Result<()> {
        let tx = [0xA5, 0x3C, 0x00, 0xFF, 0x81, 0x7E];
        let mut rx = tx;
        self.spi
            .transfer_in_place(&mut rx)
            .context("SPI transfer failed during loopback test")?;
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ");
        eprintln!("sent:     {}", hex(&tx));
        eprintln!("received: {}", hex(&rx));
        if rx == tx {
            eprintln!("Loopback OK: SCK/MOSI/MISO timing and sampling work.");
            return Ok(());
        }
        let hint = if rx.iter().all(|&b| b == 0xFF) {
            "all FF: MISO floats high - the MOSI<->MISO jumper is missing or on the wrong pin."
        } else if rx.iter().all(|&b| b == 0x00) {
            "all 00: MISO is stuck low - check the jumper and that nothing else drives RTS#."
        } else {
            "partial match: sampling is off by a bit/sample (timing issue) or the clock is too fast."
        };
        bail!("Loopback FAILED, {hint}");
    }

    /// Bench diagnostic: drive RST to a fixed level for `seconds`, then release.
    pub fn hold_rst(&mut self, high: bool, seconds: u64) -> Result<()> {
        if high {
            self.rst.set_high().context("failed to drive RST high")?;
        } else {
            self.rst.set_low().context("failed to drive RST low")?;
        }
        eprintln!(
            "RST driven {} for {seconds} s - measure pin 9 now.",
            if high { "HIGH" } else { "LOW" }
        );
        sleep(Duration::from_secs(seconds));
        self.rst.set_low().context("failed to release RST")?;
        Ok(())
    }

    /// Bench diagnostic: hold RST high and send Programming Enable over and
    /// over, so pin 9 / SCK / MOSI can be probed with a meter or scope.
    /// Prints the reply whenever it changes.
    pub fn probe(&mut self, seconds: u64) -> Result<()> {
        self.pulse_reset()?;
        eprintln!("RST is now held HIGH (pin 9 should read ~VCC) for {seconds} s.");
        eprintln!("Sending AC 53 00 00 every 50 ms; printing replies as they change.");
        let end = std::time::Instant::now() + Duration::from_secs(seconds);
        let mut last: Option<[u8; 4]> = None;
        while std::time::Instant::now() < end {
            let mut buf = [0xAC, 0x53, 0x00, 0x00];
            self.spi
                .transfer_in_place(&mut buf)
                .context("SPI transfer failed during probe")?;
            if last != Some(buf) {
                eprintln!("reply: {:02X} {:02X} {:02X} {:02X}", buf[0], buf[1], buf[2], buf[3]);
                last = Some(buf);
            }
            sleep(Duration::from_millis(50));
        }
        self.rst.set_low().context("failed to release RST")?;
        Ok(())
    }

    fn pulse_reset(&mut self) -> Result<()> {
        self.rst.set_low().context("failed to drive RST low")?;
        sleep(Duration::from_millis(1));
        self.rst.set_high().context("failed to drive RST high")?;
        sleep(RESET_SETTLE_DELAY);
        Ok(())
    }

    /// Release RST so the target resumes normal execution.
    pub fn exit_programming_mode(&mut self) -> Result<()> {
        self.rst.set_low().context("failed to release RST")?;
        Ok(())
    }

    pub fn read_signature(&mut self) -> Result<[u8; 3]> {
        let mut sig = [0u8; 3];
        // Signature bytes live at 0x000, 0x100, 0x200: the index goes in the
        // upper address byte (A11..A8).
        for (index, byte) in sig.iter_mut().enumerate() {
            let mut buf = [0x28, index as u8, 0x00, 0x00];
            self.spi
                .transfer_in_place(&mut buf)
                .context("SPI transfer failed during Read Signature Byte")?;
            *byte = buf[3];
        }
        Ok(sig)
    }

    pub fn chip_erase(&mut self) -> Result<()> {
        let mut buf = [0xAC, 0x80, 0x00, 0x00];
        self.spi
            .transfer_in_place(&mut buf)
            .context("SPI transfer failed during Chip Erase")?;
        sleep(CHIP_ERASE_DELAY);
        Ok(())
    }

    pub fn write_byte(&mut self, addr: u16, data: u8) -> Result<()> {
        let mut buf = [0x40, (addr >> 8) as u8, addr as u8, data];
        self.spi
            .transfer_in_place(&mut buf)
            .context("SPI transfer failed during Write Program Memory")?;
        sleep(BYTE_WRITE_DELAY);
        Ok(())
    }

    pub fn read_byte(&mut self, addr: u16) -> Result<u8> {
        let mut buf = [0x20, (addr >> 8) as u8, addr as u8, 0x00];
        self.spi
            .transfer_in_place(&mut buf)
            .context("SPI transfer failed during Read Program Memory")?;
        Ok(buf[3])
    }

    pub fn write_flash(&mut self, image: &[u8], mut progress: impl FnMut(usize, usize)) -> Result<()> {
        // Erased flash already reads 0xFF, so blank bytes are skipped.
        for (i, &byte) in image.iter().enumerate() {
            if byte != 0xFF {
                self.write_byte(i as u16, byte)?;
            }
            progress(i + 1, image.len());
        }
        Ok(())
    }

    pub fn verify_flash(&mut self, image: &[u8], mut progress: impl FnMut(usize, usize)) -> Result<()> {
        for (i, &expected) in image.iter().enumerate() {
            let actual = self.read_byte(i as u16)?;
            if actual != expected {
                bail!(
                    "Verify failed at address 0x{i:04X}: expected 0x{expected:02X}, read 0x{actual:02X}"
                );
            }
            progress(i + 1, image.len());
        }
        Ok(())
    }
}
