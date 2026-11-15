use super::*;
use serde::{Deserialize, Serialize};

const FORMAT: ArtifactFormat = ArtifactFormat {
    kind: 200,
    version: 17,
};
const BYTE_LIMIT: usize = 4096;

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
struct IndependentRecord {
    label: String,
    offsets: Vec<Option<u32>>,
    delta: i64,
}

#[test]
fn transport_accepts_owner_defined_kinds_and_schemas() {
    let value = IndependentRecord {
        label: "arbitrary owner data λ".into(),
        offsets: vec![Some(7), None, Some(u32::MAX)],
        delta: -128,
    };
    let dependency = DependencyDigest::for_bytes(b"independent inputs");
    let bytes = encode_binary(FORMAT, dependency, &value, BYTE_LIMIT).unwrap();
    assert_eq!(
        bytes,
        encode_binary(FORMAT, dependency, &value, BYTE_LIMIT).unwrap()
    );
    let decoded: IndependentRecord = decode_binary(&bytes, FORMAT, dependency, BYTE_LIMIT).unwrap();
    assert_eq!(decoded, value);
    assert_eq!(
        encode_binary(FORMAT, dependency, &decoded, BYTE_LIMIT).unwrap(),
        bytes
    );
}

#[test]
fn raw_payloads_round_trip_with_exact_byte_bounds() {
    let dependency = DependencyDigest([9; 32]);
    for payload in [b"".as_slice(), b"not a document or publication"] {
        let total = HEADER_LEN + payload.len();
        let bytes = encode_payload(FORMAT, dependency, payload, total).unwrap();
        assert_eq!(bytes.len(), total);
        assert_eq!(
            decode_payload(&bytes, FORMAT, dependency, total).unwrap(),
            payload
        );
        assert_eq!(
            encode_payload(FORMAT, dependency, payload, total - 1).unwrap_err(),
            CacheCodecError::ByteLimit
        );
        assert_eq!(
            decode_payload(&bytes, FORMAT, dependency, total - 1).unwrap_err(),
            CacheCodecError::ByteLimit
        );
    }
}

#[test]
fn framing_checks_reject_kind_version_dependency_and_checksum_mismatches() {
    let dependency = DependencyDigest([9; 32]);
    let bytes = encode_payload(FORMAT, dependency, b"opaque", BYTE_LIMIT).unwrap();
    assert_eq!(
        decode_payload(
            &bytes,
            ArtifactFormat {
                kind: 201,
                ..FORMAT
            },
            dependency,
            BYTE_LIMIT
        )
        .unwrap_err(),
        CacheCodecError::Kind
    );
    assert_eq!(
        decode_payload(
            &bytes,
            ArtifactFormat {
                version: 18,
                ..FORMAT
            },
            dependency,
            BYTE_LIMIT
        )
        .unwrap_err(),
        CacheCodecError::Version
    );
    assert_eq!(
        decode_payload(&bytes, FORMAT, DependencyDigest([8; 32]), BYTE_LIMIT).unwrap_err(),
        CacheCodecError::Dependency
    );
    for (offset, expected) in [
        (0, CacheCodecError::Magic),
        (9, CacheCodecError::Malformed),
        (56, CacheCodecError::Checksum),
        (HEADER_LEN, CacheCodecError::Checksum),
    ] {
        let mut invalid = bytes.clone();
        invalid[offset] ^= 1;
        assert_eq!(
            decode_payload(&invalid, FORMAT, dependency, BYTE_LIMIT).unwrap_err(),
            expected
        );
    }
}

#[test]
fn lengths_are_checked_without_truncation_overflow_or_trailing_data() {
    let dependency = DependencyDigest([9; 32]);
    let bytes = encode_payload(FORMAT, dependency, b"opaque", BYTE_LIMIT).unwrap();
    for end in 0..bytes.len() {
        assert_eq!(
            decode_payload(&bytes[..end], FORMAT, dependency, BYTE_LIMIT).unwrap_err(),
            CacheCodecError::Truncated
        );
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(
        decode_payload(&trailing, FORMAT, dependency, BYTE_LIMIT).unwrap_err(),
        CacheCodecError::Truncated
    );
    let mut overflow = bytes;
    overflow[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(
        decode_payload(&overflow, FORMAT, dependency, usize::MAX).unwrap_err(),
        CacheCodecError::ByteLimit
    );
}

#[test]
fn binary_counts_truncation_and_trailing_payload_bytes_are_rejected() {
    let dependency = DependencyDigest([9; 32]);
    for payload in [
        vec![0xff; 19],
        vec![0xff, 0xff, 0xff, 0xff, 0x7f],
        vec![1],
        vec![0, 0],
    ] {
        let bytes = encode_payload(FORMAT, dependency, &payload, BYTE_LIMIT).unwrap();
        assert_eq!(
            decode_binary::<Vec<u32>>(&bytes, FORMAT, dependency, BYTE_LIMIT).unwrap_err(),
            CacheCodecError::Malformed
        );
    }
}
