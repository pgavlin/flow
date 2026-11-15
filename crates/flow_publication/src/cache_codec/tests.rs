use super::*;

fn fixture() -> PublicationPageIndexArtifact {
    PublicationPageIndexArtifact {
        pages: vec![Some(3), None, Some(7)],
    }
}

#[test]
fn indexes_round_trip_deterministically_with_partial_and_empty_counts() {
    let dependency = DependencyDigest::for_bytes(b"publication-index");
    let limits = PublicationIndexCodecLimits::default();
    for index in [
        fixture(),
        PublicationPageIndexArtifact { pages: vec![] },
        PublicationPageIndexArtifact {
            pages: vec![None, None],
        },
        PublicationPageIndexArtifact {
            pages: vec![Some(u32::MAX), Some(u32::MAX)],
        },
    ] {
        let bytes = encode_publication_page_index(&index, dependency, limits).unwrap();
        assert_eq!(
            bytes,
            encode_publication_page_index(&index, dependency, limits).unwrap()
        );
        let decoded =
            decode_publication_page_index(&bytes, dependency, index.pages.len(), limits).unwrap();
        assert_eq!(decoded, index);
        assert_eq!(
            encode_publication_page_index(&decoded, dependency, limits).unwrap(),
            bytes
        );
    }
}

#[test]
fn indexes_match_the_version_one_wire_fixture() {
    // Golden bytes for a version-one publication page index.
    let fixture_hex = "464c5743414348310400000001000000060000000000000042424242424242424242424242424242424242424242424242424242424242429e7630ba8463fa7d0e4c20509495816b01d8edfcb9ecf427c7a747b4a41fee98030103000107";
    let expected_bytes = fixture_hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect::<Vec<_>>();
    let dependency = DependencyDigest([0x42; 32]);
    let limits = PublicationIndexCodecLimits::default();
    assert_eq!(
        decode_publication_page_index(&expected_bytes, dependency, 3, limits).unwrap(),
        fixture()
    );
    assert_eq!(
        encode_publication_page_index(&fixture(), dependency, limits).unwrap(),
        expected_bytes
    );
}

#[test]
fn indexes_must_match_the_expected_reading_order_length() {
    let dependency = DependencyDigest::for_bytes(b"publication-index");
    let limits = PublicationIndexCodecLimits::default();
    let bytes = encode_publication_page_index(&fixture(), dependency, limits).unwrap();
    for length in [0, 2, 4] {
        assert_eq!(
            decode_publication_page_index(&bytes, dependency, length, limits).unwrap_err(),
            CacheCodecError::Validation
        );
    }
}

#[test]
fn zero_page_counts_and_reading_order_limits_are_checked_on_encode_and_decode() {
    let dependency = DependencyDigest::for_bytes(b"publication-index");
    let limits = PublicationIndexCodecLimits::default();
    let zero = PublicationPageIndexArtifact {
        pages: vec![Some(0)],
    };
    let small = PublicationIndexCodecLimits {
        max_reading_order_entries: 2,
        ..limits
    };
    for (index, bound) in [(zero, limits), (fixture(), small)] {
        assert_eq!(
            encode_publication_page_index(&index, dependency, bound).unwrap_err(),
            CacheCodecError::StructuralLimit
        );
        // Construct a checksummed payload without the publication validation.
        let raw = transport::encode_binary(
            PUBLICATION_INDEX_FORMAT,
            dependency,
            &index,
            bound.max_encoded_bytes,
        )
        .unwrap();
        assert_eq!(
            decode_publication_page_index(&raw, dependency, index.pages.len(), bound).unwrap_err(),
            CacheCodecError::StructuralLimit
        );
    }
}

#[test]
fn publication_codec_rejects_stale_dependencies_corruption_wrong_kind_and_version() {
    let dependency = DependencyDigest::for_bytes(b"publication-index");
    let limits = PublicationIndexCodecLimits::default();
    let bytes = encode_publication_page_index(&fixture(), dependency, limits).unwrap();
    assert_eq!(
        decode_publication_page_index(&bytes, DependencyDigest::for_bytes(b"other"), 3, limits)
            .unwrap_err(),
        CacheCodecError::Dependency
    );
    for (offset, expected) in [
        (0, CacheCodecError::Magic),
        (8, CacheCodecError::Kind),
        (9, CacheCodecError::Malformed),
        (12, CacheCodecError::Version),
        (56, CacheCodecError::Checksum),
    ] {
        let mut invalid = bytes.clone();
        invalid[offset] ^= 1;
        assert_eq!(
            decode_publication_page_index(&invalid, dependency, 3, limits).unwrap_err(),
            expected
        );
    }
    for end in 0..bytes.len() {
        assert_eq!(
            decode_publication_page_index(&bytes[..end], dependency, 3, limits).unwrap_err(),
            CacheCodecError::Truncated
        );
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert_eq!(
        decode_publication_page_index(&trailing, dependency, 3, limits).unwrap_err(),
        CacheCodecError::Truncated
    );
}

#[test]
fn publication_codec_enforces_the_encoded_byte_bound_including_framing() {
    let dependency = DependencyDigest::for_bytes(b"publication-index");
    let defaults = PublicationIndexCodecLimits::default();
    let bytes = encode_publication_page_index(&fixture(), dependency, defaults).unwrap();
    let exact = PublicationIndexCodecLimits {
        max_encoded_bytes: bytes.len(),
        ..defaults
    };
    assert_eq!(
        encode_publication_page_index(&fixture(), dependency, exact).unwrap(),
        bytes
    );
    assert!(decode_publication_page_index(&bytes, dependency, 3, exact).is_ok());
    let short = PublicationIndexCodecLimits {
        max_encoded_bytes: bytes.len() - 1,
        ..defaults
    };
    assert_eq!(
        encode_publication_page_index(&fixture(), dependency, short).unwrap_err(),
        CacheCodecError::ByteLimit
    );
    assert_eq!(
        decode_publication_page_index(&bytes, dependency, 3, short).unwrap_err(),
        CacheCodecError::ByteLimit
    );
}

#[test]
fn malformed_binary_indexes_are_rejected_even_with_valid_checksums() {
    let dependency = DependencyDigest::for_bytes(b"publication-index");
    let limits = PublicationIndexCodecLimits::default();
    for payload in [
        vec![],
        vec![0xff; 19],                     // unterminated/overflowing vector length
        vec![0xff, 0xff, 0xff, 0xff, 0x7f], // huge length, no elements
        vec![1, 2],                         // invalid Option discriminant
        vec![1, 1],                         // present count with a truncated integer
        vec![0, 0],                         // trailing bytes after an empty vector
    ] {
        let bytes = transport::encode_payload(
            PUBLICATION_INDEX_FORMAT,
            dependency,
            &payload,
            limits.max_encoded_bytes,
        )
        .unwrap();
        assert_eq!(
            decode_publication_page_index(&bytes, dependency, 3, limits).unwrap_err(),
            CacheCodecError::Malformed
        );
    }
}
