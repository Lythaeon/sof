#![no_main]

use libfuzzer_sys::fuzz_target;
use sof::shred::wire::{
    ParsedShred, ParsedShredHeader, SIZE_OF_DATA_SHRED_HEADERS, parse_shred, parse_shred_header,
};

fuzz_target!(|data: &[u8]| {
    let parsed_header = parse_shred_header(data);
    let parsed_shred = parse_shred(data);

    if parsed_header.is_ok() {
        assert!(parsed_shred.is_ok());
    }

    if let (Ok(header), Ok(shred)) = (&parsed_header, &parsed_shred) {
        match (header, shred) {
            (ParsedShredHeader::Data(header), ParsedShred::Data(shred)) => {
                assert_eq!(header.common, shred.common);
                assert_eq!(header.data_header, shred.data_header);
                assert_eq!(header.payload_len, shred.payload.len());
                assert_eq!(
                    usize::from(shred.data_header.size),
                    SIZE_OF_DATA_SHRED_HEADERS + shred.payload.len()
                );
            }
            (ParsedShredHeader::Code(header), ParsedShred::Code(shred)) => {
                assert_eq!(header.common, shred.common);
                assert_eq!(header.coding_header, shred.coding_header);
            }
            _ => panic!("parsed header and parsed shred disagree on shred type"),
        }
    }
});
