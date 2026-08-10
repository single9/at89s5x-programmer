//! Intel HEX loading into a flat flash image, filled with 0xFF (erased state).

use anyhow::{bail, Context, Result};
use ihex::{Reader, Record};

pub fn load(text: &str, flash_size: usize) -> Result<Vec<u8>> {
    let mut image = vec![0xFFu8; flash_size];
    let mut base_addr: u32 = 0;
    let mut any_data = false;

    for record in Reader::new(text) {
        let record = record.context("failed to parse Intel HEX record")?;
        match record {
            Record::Data { offset, value } => {
                let start = base_addr + offset as u32;
                for (i, byte) in value.into_iter().enumerate() {
                    let addr = start as usize + i;
                    if addr >= flash_size {
                        bail!(
                            "HEX file contains data at address 0x{addr:04X}, \
                             which is outside the {flash_size}-byte flash (AT89S51 = 4 KB)"
                        );
                    }
                    image[addr] = byte;
                }
                any_data = true;
            }
            Record::ExtendedLinearAddress(upper) => base_addr = (upper as u32) << 16,
            Record::ExtendedSegmentAddress(segment) => base_addr = (segment as u32) << 4,
            Record::EndOfFile => break,
            Record::StartSegmentAddress { .. } | Record::StartLinearAddress(_) => {}
        }
    }

    if !any_data {
        bail!("HEX file contained no data records");
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ihex::create_object_file_representation;

    #[test]
    fn loads_data_records_and_fills_gaps_with_0xff() {
        let records = vec![
            Record::Data {
                offset: 0x0000,
                value: vec![0x75, 0x81, 0x00],
            },
            Record::Data {
                offset: 0x0010,
                value: vec![0xAB],
            },
            Record::EndOfFile,
        ];
        let text = create_object_file_representation(&records).unwrap();

        let image = load(&text, 64).unwrap();
        assert_eq!(&image[0..3], &[0x75, 0x81, 0x00]);
        assert_eq!(image[0x10], 0xAB);
        assert_eq!(image[3], 0xFF, "gap between records must stay erased (0xFF)");
        assert_eq!(image.len(), 64);
    }

    #[test]
    fn rejects_data_outside_flash_size() {
        let records = vec![
            Record::Data {
                offset: 0x0000,
                value: vec![0x00; 8],
            },
            Record::EndOfFile,
        ];
        let text = create_object_file_representation(&records).unwrap();

        let err = load(&text, 4).unwrap_err();
        assert!(err.to_string().contains("outside the 4-byte flash"));
    }
}
