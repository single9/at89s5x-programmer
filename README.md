# at89s51-isp

A command-line ISP programmer for the Atmel/Microchip **AT89S51**, driven
through an **FT232H** USB-to-SPI adapter.

## How it works

The AT89S51 (unlike the STC 89 series) does not have a UART bootloader.
Instead it exposes an SPI-like serial programming interface: `RST`, `SCK`,
`MOSI`, `MISO`. Programming is done by holding `RST` high and clocking a
fixed instruction set (Programming Enable, Chip Erase, Read/Write Program
Memory, Read Signature Byte) into the part — see `src/isp.rs` for the exact
byte sequences, taken from the AT89S51 datasheet's "Serial Programming
Instruction Set" table.

Two hardware requirements that are easy to miss:

- **The target needs its own running crystal/oscillator.** The ISP shift
  register is clocked internally off `XTAL1`; the FT232H does not supply a
  clock signal.
- **SCK must stay below 1/16 of the target's crystal frequency.** The
  default `--clock-hz 100000` is conservative and safe for any crystal
  ≥ 3.2 MHz; lower it if you're using a slower crystal.

## Wiring (FT232H → AT89S51 DIP-40)

| FT232H | AT89S51 pin | Function |
|--------|-------------|----------|
| AD0    | 8           | SCK (P1.7) |
| AD1    | 6           | MOSI (P1.5) |
| AD2    | 7           | MISO (P1.6) |
| AD3    | 9           | RST |
| GND    | 20          | GND |

VCC (pin 40) and the crystal circuit on XTAL1/XTAL2 (pins 19/18) must
already be powered and running on the target board — this tool does not
supply either.

## Usage

```sh
# Identify the chip (reads the 3 signature bytes)
cargo run -- info

# Erase, write, and verify a HEX file (erase + verify are on by default)
cargo run -- flash firmware.hex

# Just erase
cargo run -- erase

# Verify flash contents against a HEX file without writing
cargo run -- verify firmware.hex

# Select a specific FT232H by serial number, and a slower clock
cargo run -- --serial FT1234AB --clock-hz 50000 flash firmware.hex
```

## Notes / limitations

- Sized for the AT89S51's 4 KB flash (`AT89S51_FLASH_SIZE` in `src/isp.rs`).
  Bumping that constant (and ideally exposing it as a `--size` flag) is the
  only change needed to support larger siblings like the AT89S52 (8 KB).
- Lock bits are not currently read/written — only flash contents.
- On Linux, the FT232H is usually claimed by the kernel's `ftdi_sio`/`usbserial`
  driver by default; you may need to unbind it (or use `libftd2xx`'s
  supplied udev rules) before this tool can open the device via D2XX.
