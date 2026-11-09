# AGENTS.md

Rust CLI that programs AT89S51/AT89S52 (8051 family) flash over the chip's
SPI-style ISP, through an FT232RL (bit-bang) or FT232H (MPSSE). See
`README.md` for user-facing install, wiring and usage; this file holds what
is not obvious from the code.

## Layout

| Path | Role |
|------|------|
| `src/main.rs` | CLI (clap), `--adapter ft232h\|ft232r`, `execute`/`run` generic over `SpiBus` + `OutputPin`; `selftest` = MOSI-MISO loopback, no target needed |
| `src/isp.rs` | ISP protocol (`At89s51Programmer`), `Chip` detection from signature, delays |
| `src/bitbang.rs` | FT232R software SPI over libusb (`rusb`), implements `SpiBus` + `OutputPin` |
| `src/hexfile.rs` | Intel HEX loader |
| `scripts/setup-linux.sh` | One-time udev rule + `plugdev` + libusb check |
| `docs/datasheet/` | AT89S51 / AT89S52 datasheets (`.pdf` and `.md`) |
| `docs/wiring.svg` | FT232H wiring diagram (not drawn for FT232RL) |

## Hardware facts (verified against the datasheets)

- Signatures: S51 = `1E 51 06`, S52 = `1E 52 06`, read at addresses
  `0x000`, `0x100`, `0x200`. In the serial command `28 AH AL xx` the index
  goes in the **upper** address byte (AH), AL = 0. Flash: S51 4 KB, S52 8 KB.
- Programming Enable `AC 53 xx xx`: the target answers **0x69 in byte 4**
  (datasheet table, "0110 1001 (Output)" under Byte 4). The 0x53 echo in
  byte 3 is AVR behaviour; the original code checked that and always failed.
- Chip Erase is self-timed, **500 ms** (`tERASE`); a read during erase
  returns 00H. Byte write `tSWC` = 64 tCLCL + 400 µs (code waits 4 ms).
- Wait >= 10 ms after power/RST before Programming Enable (code: 20 ms).
- SCK < XTAL1 / 16. RST is active **high** for ISP (opposite of AVR).
- DIP-40: RST 9, SCK 8 (P1.7), MISO 7 (P1.6), MOSI 6 (P1.5), EA/VPP 31
  (must be tied to VCC for internal flash), XTAL1 19, XTAL2 18, GND 20, VCC 40.
- Datasheet `.md` files have garbled tables (e.g. the serial instruction
  set). For exact fields use `pdftotext -layout docs/datasheet/*.pdf -`.

## FT232R backend (`src/bitbang.rs`)

- Pins: D0 TXD = SCK, D1 RXD = MOSI, D2 RTS# = MISO (input), D3 CTS# = RST.
- Uses synchronous bit-bang (mode 0x04). Sample rate = baud x 16 and one SPI
  bit = two samples, so `baud = clock_hz / 8`.
- Readback lags output by one sample, so MISO for a bit is read from the
  entry **after** the SCK-high sample; the pattern ends with one extra entry.
- Every IN packet carries 2 modem-status bytes that must be stripped.
- Uses libusb (`rusb`) with auto-detach of `ftdi_sio`. D2XX was tried first
  and abandoned: on Linux it cannot see a device `ftdi_sio` has claimed
  (`DEVICE_NOT_FOUND`), forcing `rmmod` every plug-in. FT232H still uses D2XX.
- Needs udev permission (`GROUP=plugdev`, `MODE=0660`, 0403:6001).

## Status / not yet verified

- FT232R side verified on real hardware (2026-11-08): `selftest` (MOSI
  jumpered to MISO, no target) echoes all bytes back, so the bit-bang
  timing, MISO sampling lag and baud divisor encoding are correct.
- Verified on a real AT89S52 (2026-11-09): `info` reads `1E 52 06`, `flash`
  (erase, write, verify) and `verify` pass. Earlier `FF FF FF FF` / unstable
  replies came from the target side (crystal/wiring), not the software. Blank
  0xFF bytes are skipped on write; progress prints every 256 bytes.
- Not yet tried on a real AT89S51.
- Diagnostics: `selftest`, `probe`, `rst`, env `AT89S51_ISP_TRACE=1`.
- The previous signature code read the index from the wrong address byte and
  expected `1E 51 FF`; both were changed per the datasheet.
- Lock bits and page-mode read/write are not implemented (byte mode only).
- Windows/macOS untested.

## Environment gotchas

- No TTY for `sudo` in agent shells: anything needing root must be run by
  the user in a separate terminal. Warn about side effects **before**
  giving a command (an earlier udev rule affected every 0403:6001 device).
- Project lives on a `/mnt` volume owned by root: `sed -i` prints a
  permission warning (use Edit instead) and git needs
  `-c safe.directory='*'`.
- Default adapter is `ft232h`; FT232RL users must pass `--adapter ft232r`.
- Reply to the user in 台灣正體中文.
