//! End-to-end regression tests for the name (Subject/Issuer) decoding and
//! display behavior documented in README.md.
//!
//! These tests drive the real `chainview` binary so the public contract is
//! locked as a whole: the five output lines and their order on success, and
//! exit code 2 with empty stdout plus the `chainview: invalid DER
//! certificate:` stderr prefix on malformed input. Only name decoding is
//! exercised — signature verification, trust and expiry are intentionally out
//! of scope.
//!
//! Certificates are built with the tiny zero-dependency DER constructor at the
//! bottom of this file, which lets individual name components (including
//! truncated or non-minimal value encodings) be crafted byte for byte.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXED_SERIAL: &str = "0E8A4C2F9B17D603";
const FIXED_NOT_BEFORE: &str = "2026-01-15T09:30:00Z";
const FIXED_NOT_AFTER: &str = "2027-01-15T09:30:00Z";

// ----- Known X.520 attribute OID arcs -------------------------------------

const CN: &[u64] = &[2, 5, 4, 3];
const O: &[u64] = &[2, 5, 4, 10];
const C: &[u64] = &[2, 5, 4, 6];
const POSTAL_ADDRESS: &[u64] = &[2, 5, 4, 17];
const POSTAL_CODE: &[u64] = &[2, 5, 4, 18];

// The spec example: a SEQUENCE containing only UTF8String "abc", with its
// complete DER encoding.
const POSTAL_ADDRESS_ABC_HEX: &str = "30050C03616263";

// ----- Success cases -------------------------------------------------------

#[test]
fn unknown_oid_uses_dotted_oid_and_full_der_hex() {
    // The exact README example: UTF8String "abc" for type 1.2.3.4 must render
    // as 1.2.3.4=#0C03616263 — tag, length and content, readable text or not.
    let subject = name(&[rdn(&[atv_utf8(&[1, 2, 3, 4], "abc")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("unknown-basic", &build_cert(&subject, &issuer));

    assert_success(
        &out,
        "1.2.3.4=#0C03616263",
        "CN=Test CA",
    );
}

#[test]
fn unknown_oid_value_preserves_long_form_length() {
    // A 128-byte content needs the long-form length 0x81 0x80; the hex must
    // carry the original TLV header verbatim instead of collapsing the length
    // or emitting only the content bytes.
    let long_text = "a".repeat(128);
    let subject = name(&[rdn(&[atv_utf8(&[1, 2, 3, 4], &long_text)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("unknown-longlen", &build_cert(&subject, &issuer));

    let mut expected_hex = String::from("0C8180");
    expected_hex.push_str(&"61".repeat(128));
    assert_success(&out, &format!("1.2.3.4=#{expected_hex}"), "CN=Test CA");
}

#[test]
fn unknown_oid_under_2_999_keeps_numeric_arcs() {
    // 2.999.* is a legal OID whose first subidentifier (1079) needs multiple
    // base-128 bytes (0x88 0x37); it must stay a dotted decimal identifier.
    let subject = name(&[rdn(&[atv_utf8(&[2, 999, 1], "x")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("unknown-2999", &build_cert(&subject, &issuer));

    assert_success(&out, "2.999.1=#0C0178", "CN=Test CA");
}

#[test]
fn unknown_non_string_value_is_shown_as_full_der_hex() {
    // An OCTET STRING value (tag 0x04) is not text at all; the full encoding
    // still appears after '#'.
    let value = tlv(0x04, &[0x01, 0x02, 0x03]);
    let subject = name(&[rdn(&[atv(&[1, 2, 3, 4], &value)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("unknown-octet", &build_cert(&subject, &issuer));

    assert_success(&out, "1.2.3.4=#0403010203", "CN=Test CA");
}

#[test]
fn mixed_known_and_unknown_attributes_keep_their_own_forms() {
    // Encoded RDN order (display is reversed):
    //   SET { C=CN }
    //   SET { O=示例公司 }
    //   SET { 1.2.3.4 "abc", 1.2.3.4 "def", CN=example.com }
    // The last SET is one multi-valued RDN: joined with '+', never split into
    // groups; the repeated unknown attribute appears twice (never merged); the
    // adjacent CN stays text; CJK content passes through as raw UTF-8; groups
    // are shown in reverse order; '#' is the encoding marker, unescaped. The
    // members within the SET are listed in DER SET OF byte order: the two
    // unknown attributes (shorter complete encodings than CN) sort first,
    // then CN, and the duplicate unknown members stay adjacent in their own
    // ascending order — display preserves that encoding order verbatim.
    let subject = name(&[
        rdn(&[atv_utf8(C, "CN")]),
        rdn(&[atv_utf8(O, "示例公司")]),
        rdn(&[
            atv_utf8(&[1, 2, 3, 4], "abc"),
            atv_utf8(&[1, 2, 3, 4], "def"),
            atv_utf8(CN, "example.com"),
        ]),
    ]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("mixed", &build_cert(&subject, &issuer));

    assert_success(
        &out,
        "1.2.3.4=#0C03616263+1.2.3.4=#0C03646566+CN=example.com,O=示例公司,C=CN",
        "CN=Test CA",
    );
}

#[test]
fn subject_and_issuer_render_independently() {
    // Each name reflects only its own attributes: distinct OID sets and value
    // encodings must never bleed across Subject/Issuer.
    let subject = name(&[
        rdn(&[atv_utf8(O, "测试")]),
        rdn(&[atv_utf8(CN, "subject.example")]),
    ]);
    // The issuer's multi-valued RDN follows DER SET OF order: the unknown
    // attribute's complete encoding is shorter, so it sorts before CN even
    // though CN would come first as a displayed name; display keeps the
    // encoding order.
    let issuer = name(&[rdn(&[
        atv_utf8(&[1, 2, 3, 4], "abc"),
        atv_utf8(CN, "Example CA"),
    ])]);
    let (out, _cert) = run_inspect("separate-names", &build_cert(&subject, &issuer));

    assert_success(
        &out,
        "CN=subject.example,O=测试",
        "1.2.3.4=#0C03616263+CN=Example CA",
    );
}

#[test]
fn known_attribute_with_sequence_value_keeps_short_name_and_shows_der_hex() {
    // The exact spec example: postalAddress (2.5.4.17) is a known attribute
    // whose value is a SEQUENCE { UTF8String "abc" } rather than a string. It
    // must render as postalAddress=#30050C03616263 — tag 0x30, length 0x05
    // and content verbatim — instead of rejecting the whole certificate.
    let value = seq(&tlv(0x0C, b"abc"));
    assert_eq!(hex(&value), POSTAL_ADDRESS_ABC_HEX);
    let subject = name(&[rdn(&[atv(POSTAL_ADDRESS, &value)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("known-sequence", &build_cert(&subject, &issuer));

    assert_success(&out, "postalAddress=#30050C03616263", "CN=Test CA");
}

#[test]
fn known_non_string_value_applies_to_issuer_as_well() {
    // The same rule independently governs the Issuer name.
    let subject = simple_cn_name("subject.example");
    let issuer = name(&[
        rdn(&[atv(POSTAL_ADDRESS, &seq(&tlv(0x0C, b"abc")))]),
        rdn(&[atv_utf8(CN, "Test CA")]),
    ]);
    let (out, _cert) = run_inspect("known-sequence-issuer", &build_cert(&subject, &issuer));

    assert_success(&out, "CN=subject.example", "CN=Test CA,postalAddress=#30050C03616263");
}

#[test]
fn known_non_string_value_preserves_long_form_length() {
    // A legal long-form length in the value TLV must be carried byte for byte
    // after '#', not collapsed and not rebuilt from the parsed content.
    let value = tlv(0x04, &vec![0x41u8; 128]); // postalCode as OCTET STRING
    let subject = name(&[rdn(&[atv(POSTAL_CODE, &value)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("known-longlen", &build_cert(&subject, &issuer));

    let mut expected = String::from("postalCode=#048180");
    expected.push_str(&"41".repeat(128));
    assert_success(&out, &expected, "CN=Test CA");
}

#[test]
fn text_hex_and_unknown_values_mix_without_splitting_multivalued_rdn() {
    // One multi-valued RDN mixes a text value (CN), a known attribute shown as
    // hex (postalAddress SEQUENCE) and an unknown attribute shown as hex
    // (1.2.3.4). The group stays joined with '+'; a preceding text RDN and a
    // repeated text CN in another RDN keep their order after reversal. The
    // members are encoded in DER SET OF order, which is decided here by the
    // outer SEQUENCE length and content bytes, not by attribute name: the
    // shortest member (unknown 1.2.3.4) sorts first, postalAddress next and
    // CN last — display preserves exactly that encoding order.
    let subject = name(&[
        rdn(&[atv_utf8(C, "CN")]),
        rdn(&[atv_utf8(CN, "one"), atv_utf8(CN, "two")]),
        rdn(&[
            atv_utf8(&[1, 2, 3, 4], "abc"),
            atv(POSTAL_ADDRESS, &seq(&tlv(0x0C, b"abc"))),
            atv_utf8(CN, "example.com"),
        ]),
    ]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("known-mixed", &build_cert(&subject, &issuer));

    assert_success(
        &out,
        &format!(
            "1.2.3.4=#0C03616263+postalAddress=#{POSTAL_ADDRESS_ABC_HEX}+CN=example.com,\
             CN=one+CN=two,C=CN"
        ),
        "CN=Test CA",
    );
}

#[test]
fn known_attribute_with_bad_string_is_still_rejected_not_hexified() {
    // A supported string type that fails to decode must never be salvaged by
    // the hex fallback: invalid UTF-8 in a known attribute is still corrupt.
    let bad_value = tlv(0x0C, &[0xFF, 0xFE]);
    let subject = name(&[rdn(&[atv(POSTAL_ADDRESS, &bad_value)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("known-bad-string", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
}

#[test]
fn truncated_known_non_string_value_is_rejected() {
    // The non-text value's SEQUENCE header announces five content bytes but
    // only four follow; hex display must not mask the truncation.
    let mut inner = oid(POSTAL_ADDRESS);
    inner.extend_from_slice(&[0x30, 0x05, 0x0C, 0x02, 0xAB]); // len 5, 4 present
    let subject = name(&[rdn(&[seq(&inner)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("known-truncated", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
}

#[test]
fn known_non_string_value_with_extra_field_is_rejected() {
    // Two value TLVs inside one AttributeTypeAndValue stay illegal even when
    // the first value would render as hex.
    let two_values = concat(&[&seq(&tlv(0x0C, b"abc")), &tlv(0x05, &[])]);
    let subject = name(&[rdn(&[atv(POSTAL_ADDRESS, &two_values)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("known-extra-field", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
}

#[test]
fn empty_subject_keeps_empty_field_while_issuer_shows() {
    // An empty SEQUENCE is a valid empty name: the Subject line stays empty
    // (just "Subject: ") while Issuer renders normally. All five fields keep
    // their documented order, and the time fields are UTC regardless of the
    // machine zone.
    let subject = tlv(0x30, &[]); // Name ::= SEQUENCE {}
    let issuer = simple_cn_name("Example CA");
    let (out, _cert) = run_inspect("empty-subject", &build_cert(&subject, &issuer));

    assert_eq!(out.status.code(), Some(0), "stderr={}", lossy(&out.stderr));
    assert!(out.stderr.is_empty());
    assert_eq!(
        lossy(&out.stdout),
        format!(
            "Subject: \n\
             Issuer: CN=Example CA\n\
             Serial Number: {FIXED_SERIAL}\n\
             Not Before: {FIXED_NOT_BEFORE}\n\
             Not After: {FIXED_NOT_AFTER}\n"
        )
    );
}

// ----- Failure cases -------------------------------------------------------

#[test]
fn invalid_utf8_in_unknown_attribute_value_is_rejected() {
    // Tagged UTF8String but carrying 0xFF 0xFE: an input error even though the
    // value would have been shown as hex.
    let bad_value = tlv(0x0C, &[0xFF, 0xFE]);
    let subject = name(&[rdn(&[atv(&[1, 2, 3, 4], &bad_value)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("bad-utf8-unknown", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
}

#[test]
fn invalid_utf8_in_known_attribute_value_is_rejected() {
    // Same rule for a known short-name attribute: the value cannot be decoded.
    let bad_value = tlv(0x0C, &[0xFF]);
    let subject = name(&[rdn(&[atv(CN, &bad_value)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("bad-utf8-known", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
}

// ----- PrintableString (tag 0x13) character rules --------------------------

#[test]
fn printable_string_with_legal_characters_displays_as_text() {
    // Every character of the RFC 5280 PrintableString alphabet is accepted:
    // letters, digits, space and ' ( ) + , - . / : = ?. The RFC 4514 text
    // escaping is unchanged: '+' and ',' are escaped anywhere, and leading and
    // trailing spaces keep their backslashes; the other punctuation passes
    // through verbatim.
    let subject = name(&[rdn(&[atv_printable(
        CN,
        b" John+Doe, Jr.'s (Section-1/2: A=B?) ",
    )])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("printable-legal", &build_cert(&subject, &issuer));

    assert_success(
        &out,
        "CN=\\ John\\+Doe\\, Jr.'s (Section-1/2: A=B?)\\ ",
        "CN=Test CA",
    );
}

#[test]
fn printable_string_for_unknown_oid_still_uses_full_der_hex() {
    // A legal PrintableString at an unknown OID is still shown by encoding
    // (never decoded to text): tag 0x13, length and content in uppercase hex.
    let subject = name(&[rdn(&[atv_printable(&[1, 2, 3, 4], b"abc")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("printable-unknown", &build_cert(&subject, &issuer));

    assert_success(&out, "1.2.3.4=#1303616263", "CN=Test CA");
}

#[test]
fn empty_printable_string_is_accepted() {
    // Empty content keeps the pre-existing acceptance: the character check
    // adds no extra failure for a zero-length value.
    let subject = name(&[rdn(&[atv_printable(CN, b"")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("printable-empty", &build_cert(&subject, &issuer));

    assert_success(&out, "CN=", "CN=Test CA");
}

#[test]
fn illegal_bytes_in_subject_printable_string_are_rejected() {
    // One illegal byte anywhere in a known attribute's PrintableString fails
    // the whole certificate; the byte must never be printed, replaced or
    // stripped. @, _, tab, LF, NUL and the whole 0x80..=0xFF range are all
    // outside the alphabet even though many are otherwise printable bytes.
    for bad in [
        b'@', b'_', b'%', b'&', b'*', b'!', b'"', b';', b'<', b'>', b'\\', b'#', b'[',
        b']', b'{', b'}', b'|', b'^', b'~', b'`', b'$', b'\t', b'\n', b'\r', 0x00, 0x01,
        0x1F, 0x7F, 0x80, 0xA9, 0xC3, 0xFF,
    ] {
        let value = vec![b'a', bad, b'b'];
        let subject = name(&[rdn(&[atv_printable(CN, &value)])]);
        let issuer = simple_cn_name("Test CA");
        let (out, _cert) = run_inspect("printable-bad-subject", &build_cert(&subject, &issuer));

        assert_invalid_certificate(&out);
        let stderr = lossy(&out.stderr);
        assert!(
            stderr.contains("subject") && stderr.contains("PrintableString"),
            "byte 0x{bad:02X} must fail as a subject PrintableString error: {stderr}"
        );
    }
}

#[test]
fn illegal_byte_in_issuer_printable_string_is_rejected_and_named() {
    // The same rule applies to the Issuer name, and the reason names it.
    let subject = simple_cn_name("subject.example");
    let issuer = name(&[rdn(&[atv_printable(CN, b"bad_ca")])]);
    let (out, _cert) = run_inspect("printable-bad-issuer", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.contains("issuer") && stderr.contains("PrintableString"),
        "reason must name the issuer name and PrintableString: {stderr}"
    );
}

#[test]
fn illegal_printable_string_at_unknown_oid_cannot_be_accepted_via_hex() {
    // Unknown attributes render as '#'-hex, but hex display is not an escape
    // hatch: a PrintableString containing '@' must fail even though the value
    // would otherwise appear only as hex bytes.
    let subject = name(&[rdn(&[atv_printable(&[1, 2, 3, 4], b"a@b")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("printable-bad-unknown", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    assert!(lossy(&out.stderr).contains("PrintableString"));
}

#[test]
fn non_ascii_and_at_still_display_in_utf8_string() {
    // The restriction is type-specific: a UTF8String may freely contain '@'
    // and CJK text, which keeps displaying as raw UTF-8.
    let subject = name(&[rdn(&[atv_utf8(CN, "user@示例.test")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("utf8-unaffected", &build_cert(&subject, &issuer));

    assert_success(&out, "CN=user@示例.test", "CN=Test CA");
}

// ----- IA5String (tag 0x16) ASCII byte-range rules --------------------------

#[test]
fn ia5_string_with_legal_bytes_displays_as_text() {
    // IA5String is ASN.1's ASCII type: the whole 0x00..=0x7F range is legal,
    // not just printable characters. Ordinary ASCII symbols that a
    // PrintableString would reject ('@', '_', '#') pass, and so do NUL, tab,
    // LF and DEL — the existing RFC 4514 control-character escaping applies to
    // them (\00, \09, \0A, \7F), adding no extra output line. Leading tab and
    // the trailing NUL are control characters, not spaces, so they take the
    // hex escape rather than the edge-space rule; embedded spaces and commas
    // keep their existing escaping and are otherwise untouched.
    let subject = name(&[rdn(&[atv_ia5(
        CN,
        b"\tJohn\nDoe, Jr.@_\x7F#1\x00",
    )])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("ia5-legal", &build_cert(&subject, &issuer));

    assert_success(
        &out,
        "CN=\\09John\\0ADoe\\, Jr.@_\\7F#1\\00",
        "CN=Test CA",
    );
}

#[test]
fn ia5_string_accepts_every_control_byte_and_del() {
    // No legal byte of 0x00..=0x7F may be rejected: probe the control ranges
    // (0x00..=0x1F and 0x7F) one by one; each renders through the existing
    // two-digit control escape without failing.
    for legal in (0x00u8..=0x1F).chain([0x7F]) {
        let subject = name(&[rdn(&[atv_ia5(CN, &[b'a', legal, b'b'])])]);
        let issuer = simple_cn_name("Test CA");
        let (out, _cert) = run_inspect("ia5-legal-controls", &build_cert(&subject, &issuer));

        assert_eq!(out.status.code(), Some(0), "stderr={}", lossy(&out.stderr));
        assert_eq!(
            lossy(&out.stdout).lines().next().unwrap(),
            &format!("Subject: CN=a\\{legal:02X}b"),
            "byte 0x{legal:02X} must stay legal in an IA5String"
        );
    }
}

#[test]
fn ia5_string_for_unknown_oid_still_uses_full_der_hex() {
    // A legal IA5String at an unknown OID is shown by encoding (never decoded
    // to text): tag 0x16, length and content in uppercase hex.
    let subject = name(&[rdn(&[atv_ia5(&[1, 2, 3, 4], b"abc")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("ia5-unknown", &build_cert(&subject, &issuer));

    assert_success(&out, "1.2.3.4=#1603616263", "CN=Test CA");
}

#[test]
fn empty_ia5_string_is_accepted_in_both_display_forms() {
    // Empty content keeps the pre-existing acceptance: a known attribute shows
    // empty text, an unknown attribute shows tag and zero length as #1600.
    let issuer = simple_cn_name("Test CA");

    let subject = name(&[rdn(&[atv_ia5(CN, b"")])]);
    let (out, _cert) = run_inspect("ia5-empty-known", &build_cert(&subject, &issuer));
    assert_success(&out, "CN=", "CN=Test CA");

    let subject = name(&[rdn(&[atv_ia5(&[1, 2, 3, 4], b"")])]);
    let (out, _cert) = run_inspect("ia5-empty-unknown", &build_cert(&subject, &issuer));
    assert_success(&out, "1.2.3.4=#1600", "CN=Test CA");
}

#[test]
fn non_ascii_bytes_in_subject_ia5_string_are_rejected() {
    // Any byte in 0x80..=0xFF fails the whole certificate wherever it stands:
    // alone, after a legal DEL, embedded between ASCII bytes, and even when the
    // surrounding bytes are perfectly valid UTF-8 (the two-byte UTF-8 of 'é'
    // and the three-byte UTF-8 of '例'). The IA5String byte range is what
    // counts; the bytes are never printed, replaced or reinterpreted as
    // UTF-8.
    for bad in [
        vec![0x80],
        vec![0xFF],
        vec![0x7F, 0x80],
        vec![b'a', 0xA9, b'b'],
        vec![b'a', 0xC2, 0xA9, b'b'], // valid UTF-8 'é' between ASCII
        vec![0xE4, 0xBE, 0x8B],       // valid UTF-8 '例'
        vec![b'x', 0xC3, 0xBF],       // valid UTF-8 'ÿ' after ASCII
    ] {
        let subject = name(&[rdn(&[atv_ia5(CN, &bad)])]);
        let issuer = simple_cn_name("Test CA");
        let (out, _cert) = run_inspect("ia5-bad-subject", &build_cert(&subject, &issuer));

        assert_invalid_certificate(&out);
        let stderr = lossy(&out.stderr);
        assert!(
            stderr.contains("subject") && stderr.contains("IA5String"),
            "{bad:02X?} must fail as a subject IA5String error: {stderr}"
        );
    }
}

#[test]
fn non_ascii_byte_in_issuer_ia5_string_is_rejected_and_named() {
    // The same rule independently governs the Issuer name, and the reason
    // names it — even though the subject is well formed.
    let subject = simple_cn_name("subject.example");
    let issuer = name(&[rdn(&[atv_ia5(CN, b"bad\xCAca")])]);
    let (out, _cert) = run_inspect("ia5-bad-issuer", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.contains("issuer") && stderr.contains("IA5String"),
        "reason must name the issuer name and IA5String: {stderr}"
    );
}

#[test]
fn illegal_ia5_string_at_unknown_oid_cannot_be_accepted_via_hex() {
    // Unknown attributes render as '#'-hex, but hex display is not an escape
    // hatch: an IA5String carrying 0x80..=0xFF must fail even though the value
    // would otherwise appear only as hex bytes, and even when those bytes form
    // valid UTF-8.
    for bad in [
        vec![b'a', 0x80, b'b'],
        vec![0xE4, 0xBE, 0x8B], // valid UTF-8 '例'
    ] {
        let subject = name(&[rdn(&[atv_ia5(&[1, 2, 3, 4], &bad)])]);
        let issuer = simple_cn_name("Test CA");
        let (out, _cert) = run_inspect("ia5-bad-unknown", &build_cert(&subject, &issuer));

        assert_invalid_certificate(&out);
        assert!(lossy(&out.stderr).contains("IA5String"));
    }
}

#[test]
fn illegal_ia5_string_between_legal_attributes_is_rejected() {
    // One bad IA5String value must not be ignored because legal attributes
    // stand on both sides of it — neither in its own RDN between two other
    // RDNs, nor as a member inside an otherwise legal multi-valued RDN.
    let subject = name(&[
        rdn(&[atv_utf8(C, "US")]),
        rdn(&[atv_ia5(CN, b"bad\xffca")]),
        rdn(&[atv_utf8(O, "org")]),
    ]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("ia5-bad-among-rdns", &build_cert(&subject, &issuer));
    assert_invalid_certificate(&out);
    assert!(lossy(&out.stderr).contains("subject"));
    assert!(lossy(&out.stderr).contains("IA5String"));

    // Multi-valued RDN: the unknown IA5 member's complete encoding is longer
    // than the CN UTF8String member, so it sorts second and the SET OF order
    // is legal; the illegal byte still rejects.
    let subject = name(&[rdn(&[
        atv_utf8(CN, "ok"),
        atv_ia5(&[1, 2, 3, 4], b"bad\xff"),
    ])]);
    let (out, _cert) = run_inspect("ia5-bad-among-members", &build_cert(&subject, &issuer));
    assert_invalid_certificate(&out);
    assert!(lossy(&out.stderr).contains("IA5String"));
}

#[test]
fn ia5_rule_depends_on_the_carried_type_not_the_attribute_name() {
    // A known short name cannot relax the rule: DC (2nd most common IA5
    // carrier after CN in real certificates) with bytes forming valid UTF-8
    // ('é') is still corrupt under an IA5String tag.
    let dc = &[0, 9, 2342, 19200300, 100, 1, 25];
    let subject = name(&[rdn(&[atv_ia5(dc, b"caf\xC3\xA9")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("ia5-bad-dc", &build_cert(&subject, &issuer));
    assert_invalid_certificate(&out);
    assert!(lossy(&out.stderr).contains("IA5String"));

    // The same DC carrying plain ASCII keeps displaying as text.
    let subject = name(&[rdn(&[atv_ia5(dc, b"example.com")])]);
    let (out, _cert) = run_inspect("ia5-legal-dc", &build_cert(&subject, &issuer));
    assert_success(&out, "DC=example.com", "CN=Test CA");
}

#[test]
fn same_bytes_are_legal_utf8_but_illegal_ia5() {
    // The bytes E4 BE 8B mean '例' in a UTF8String (accepted, shown as text)
    // but are three illegal bytes in an IA5String (rejected): the decision is
    // made from the value's actual tag, never from the content looking like
    // text.
    let cjk = [0xE4u8, 0xBE, 0x8B];

    let subject = name(&[rdn(&[atv_utf8(CN, "例")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("ia5-vs-utf8-utf8", &build_cert(&subject, &issuer));
    assert_success(&out, "CN=例", "CN=Test CA");

    let subject = name(&[rdn(&[atv_ia5(CN, &cjk)])]);
    let (out, _cert) = run_inspect("ia5-vs-utf8-ia5", &build_cert(&subject, &issuer));
    assert_invalid_certificate(&out);
    assert!(lossy(&out.stderr).contains("IA5String"));
}

#[test]
fn other_ascii_family_string_tags_keep_their_existing_behavior() {
    // The new range rule belongs to IA5String (0x16) alone. NumericString
    // (0x12) and VisibleString (0x1A) keep their pre-existing byte passthrough
    // for a non-ASCII byte instead of being tightened alongside it.
    for tag in [0x12u8, 0x1A] {
        let subject = name(&[rdn(&[atv(CN, &tlv(tag, &[b'A', 0xE9]))])]);
        let issuer = simple_cn_name("Test CA");
        let (out, _cert) = run_inspect("ia5-other-tags", &build_cert(&subject, &issuer));
        assert_eq!(out.status.code(), Some(0), "stderr={}", lossy(&out.stderr));
        assert_eq!(
            lossy(&out.stdout).lines().next().unwrap(),
            "Subject: CN=Aé",
            "tag 0x{tag:02X} must keep its existing passthrough behavior"
        );
    }
}

// ----- BMPString (tag 0x1E) character rules --------------------------------

#[test]
fn bmp_string_with_bmp_characters_displays_as_text() {
    // Big-endian 16-bit BMP characters, including CJK, decode to text; the
    // usual RFC 4514 escaping still applies to ','.
    let content = bmp_units(&[0x793A, 0x4F8B, 0x002C, 0x0041]); // "示例,A"
    let subject = name(&[rdn(&[atv_bmp(CN, &content)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("bmp-legal", &build_cert(&subject, &issuer));

    assert_success(&out, "CN=示例\\,A", "CN=Test CA");
}

#[test]
fn bmp_string_for_unknown_oid_still_uses_full_der_hex() {
    // A legal BMPString at an unknown OID is shown by encoding (never decoded
    // to text): tag 0x1E, length and content in uppercase hex.
    let subject = name(&[rdn(&[atv_bmp(&[1, 2, 3, 4], &bmp_units(&[0x0061, 0x0062]))])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("bmp-unknown", &build_cert(&subject, &issuer));

    assert_success(&out, "1.2.3.4=#1E0400610062", "CN=Test CA");
}

#[test]
fn empty_bmp_string_is_accepted() {
    // Empty content keeps the pre-existing acceptance: zero bytes is zero
    // whole 16-bit characters.
    let subject = name(&[rdn(&[atv_bmp(CN, &[])])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("bmp-empty", &build_cert(&subject, &issuer));

    assert_success(&out, "CN=", "CN=Test CA");
}

#[test]
fn odd_length_bmp_string_in_subject_is_rejected() {
    // Three content bytes cannot be whole 16-bit characters.
    let subject = name(&[rdn(&[atv_bmp(CN, &[0x00, 0x61, 0x00])])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("bmp-odd-subject", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.contains("subject") && stderr.contains("BMPString"),
        "reason must name the subject name and BMPString: {stderr}"
    );
}

#[test]
fn odd_length_bmp_string_in_issuer_is_rejected() {
    // The same rule applies to the Issuer name, and the reason names it.
    let subject = simple_cn_name("subject.example");
    let issuer = name(&[rdn(&[atv_bmp(CN, &[0x41])])]);
    let (out, _cert) = run_inspect("bmp-odd-issuer", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.contains("issuer") && stderr.contains("BMPString"),
        "reason must name the issuer name and BMPString: {stderr}"
    );
}

#[test]
fn lone_surrogates_in_bmp_string_are_rejected() {
    // A lone high surrogate and a lone low surrogate are both illegal, in
    // either name; nothing may be printed, replaced or stripped.
    for unit in [0xD800u16, 0xDBFF, 0xDC00, 0xDFFF] {
        let content = bmp_units(&[0x0041, unit, 0x0042]);
        let subject = name(&[rdn(&[atv_bmp(CN, &content)])]);
        let issuer = simple_cn_name("Test CA");
        let (out, _cert) = run_inspect("bmp-lone-surr", &build_cert(&subject, &issuer));

        assert_invalid_certificate(&out);
        let stderr = lossy(&out.stderr);
        assert!(
            stderr.contains("subject") && stderr.contains("BMPString"),
            "unit U+{unit:04X} must fail as a subject BMPString error: {stderr}"
        );
    }
}

#[test]
fn surrogate_pair_in_bmp_string_is_rejected_not_merged() {
    // U+D83D U+DE00 would combine into U+1F600 under UTF-16 rules, but a
    // BMPString holds BMP characters only: the pair must fail the whole
    // certificate, never be merged into an astral character and displayed.
    let content = bmp_units(&[0xD83D, 0xDE00]);
    let subject = name(&[rdn(&[atv_bmp(CN, &content)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("bmp-surr-pair", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    assert!(lossy(&out.stderr).contains("BMPString"));
}

#[test]
fn illegal_bmp_string_at_unknown_oid_cannot_be_accepted_via_hex() {
    // Unknown attributes render as '#'-hex, but hex display is not an escape
    // hatch: odd length and surrogate code points must fail even though the
    // value would otherwise appear only as hex bytes.
    for content in [
        vec![0x00, 0x61, 0x00],          // odd length
        bmp_units(&[0xD800, 0xDC00]),    // surrogate pair
    ] {
        let subject = name(&[rdn(&[atv_bmp(&[1, 2, 3, 4], &content)])]);
        let issuer = simple_cn_name("Test CA");
        let (out, _cert) = run_inspect("bmp-bad-unknown", &build_cert(&subject, &issuer));

        assert_invalid_certificate(&out);
        assert!(lossy(&out.stderr).contains("BMPString"));
    }
}

#[test]
fn utf8_and_universal_string_keep_legal_astral_characters() {
    // The BMPString restriction is type-specific: a UTF8String and a
    // UniversalString may legally carry characters outside the BMP
    // (U+1F600 here), which keep displaying as raw UTF-8.
    let subject = name(&[rdn(&[atv_utf8(CN, "a\u{1F600}")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("utf8-astral", &build_cert(&subject, &issuer));
    assert_success(&out, "CN=a\u{1F600}", "CN=Test CA");

    let universal = atv(CN, &tlv(0x1C, &[0x00, 0x01, 0xF6, 0x00])); // U+1F600
    let subject = name(&[rdn(&[universal])]);
    let (out, _cert) = run_inspect("universal-astral", &build_cert(&subject, &issuer));
    assert_success(&out, "CN=\u{1F600}", "CN=Test CA");
}

// ----- UniversalString (tag 0x1C) code point rules -------------------------

#[test]
fn universal_string_with_code_points_displays_as_text() {
    // Big-endian 32-bit code points, including BMP characters, a comma and an
    // astral character (U+1F600), decode to text; the usual RFC 4514 escaping
    // still applies to ','.
    let content = universal_units(&[0x0061, 0x1F600, 0x002C, 0x0041]); // "a😀,A"
    let subject = name(&[rdn(&[atv_universal(CN, &content)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("universal-legal", &build_cert(&subject, &issuer));

    assert_success(&out, "CN=a\u{1F600}\\,A", "CN=Test CA");
}

#[test]
fn universal_string_for_unknown_oid_still_uses_full_der_hex() {
    // The exact spec example: a UniversalString U+1F600 at the unknown OID
    // 1.2.3.4 renders as 1.2.3.4=#1C040001F600 — the original tag, length and
    // all content in uppercase hex, never decoded to text.
    let subject = name(&[rdn(&[atv_universal(
        &[1, 2, 3, 4],
        &universal_units(&[0x1F600]),
    )])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("universal-unknown", &build_cert(&subject, &issuer));

    assert_success(&out, "1.2.3.4=#1C040001F600", "CN=Test CA");
}

#[test]
fn empty_universal_string_is_accepted() {
    // Empty content keeps the pre-existing acceptance: a known attribute shows
    // empty text, an unknown attribute shows tag and zero length as #1C00.
    let issuer = simple_cn_name("Test CA");

    let subject = name(&[rdn(&[atv_universal(CN, &[])])]);
    let (out, _cert) = run_inspect("universal-empty-known", &build_cert(&subject, &issuer));
    assert_success(&out, "CN=", "CN=Test CA");

    let subject = name(&[rdn(&[atv_universal(&[1, 2, 3, 4], &[])])]);
    let (out, _cert) = run_inspect("universal-empty-unknown", &build_cert(&subject, &issuer));
    assert_success(&out, "1.2.3.4=#1C00", "CN=Test CA");
}

#[test]
fn boundary_code_points_in_universal_string_are_accepted() {
    // U+0000 and U+10FFFF are the inclusive legal boundaries: both decode. The
    // NUL keeps the existing RFC 4514 control-character escaping (\00) in text.
    let subject = name(&[rdn(&[atv_universal(
        CN,
        &universal_units(&[0x0000, 0x10FFFF]),
    )])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("universal-boundaries", &build_cert(&subject, &issuer));

    assert_success(&out, "CN=\\00\u{10FFFF}", "CN=Test CA");
}

#[test]
fn unknown_universal_string_preserves_long_form_length() {
    // 32 code points are 128 content bytes, requiring the long-form length
    // 0x81 0x80; the hex must carry the original TLV header verbatim.
    let content = universal_units(&vec![0x00000041u32; 32]);
    let subject = name(&[rdn(&[atv_universal(&[1, 2, 3, 4], &content)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("universal-longlen", &build_cert(&subject, &issuer));

    let mut expected = String::from("1.2.3.4=#1C8180");
    expected.push_str(&"00000041".repeat(32));
    assert_success(&out, &expected, "CN=Test CA");
}

#[test]
fn universal_string_mixes_with_other_attributes_and_string_types() {
    // A legal mixed multi-valued RDN (unknown UniversalString and CN
    // UniversalString, both U+1F600 — the unknown member sorts first on its
    // smaller OID byte) alongside a UTF8String CN RDN and a PrintableString C
    // RDN: the other string types, group reversal and '+' joining stay intact.
    let subject = name(&[
        rdn(&[atv_printable(C, b"US")]),
        rdn(&[
            atv_universal(&[1, 2, 3, 4], &universal_units(&[0x1F600])),
            atv_universal(CN, &universal_units(&[0x1F600])),
        ]),
        rdn(&[atv_utf8(CN, "host")]),
    ]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("universal-mixed-legal", &build_cert(&subject, &issuer));

    assert_success(
        &out,
        "CN=host,1.2.3.4=#1C040001F600+CN=\u{1F600},C=US",
        "CN=Test CA",
    );
}

#[test]
fn bad_length_universal_string_in_subject_is_rejected() {
    // One to three trailing bytes cannot form whole 32-bit code points, even
    // when every complete leading code point is legal; 5/6/7 bytes pair one
    // legal code point with an incomplete tail.
    let legal = universal_units(&[0x0061]);
    for bad in [
        vec![0x00],
        vec![0x00, 0x61],
        vec![0x00, 0x00, 0x61],
        [&legal[..], &[0x00]].concat(),
        [&legal[..], &[0x00, 0x00]].concat(),
        [&legal[..], &[0x00, 0x00, 0x00]].concat(),
    ] {
        let len = bad.len();
        let subject = name(&[rdn(&[atv_universal(CN, &bad)])]);
        let issuer = simple_cn_name("Test CA");
        let (out, _cert) = run_inspect("universal-badlen-subject", &build_cert(&subject, &issuer));

        assert_invalid_certificate(&out);
        let stderr = lossy(&out.stderr);
        assert!(
            stderr.contains("subject") && stderr.contains("UniversalString"),
            "{len}-byte value must fail as a subject UniversalString error: {stderr}"
        );
    }
}

#[test]
fn bad_length_universal_string_in_issuer_is_rejected() {
    // The same length rule applies to the Issuer name, and the reason names it.
    let subject = simple_cn_name("subject.example");
    let issuer = name(&[rdn(&[atv_universal(CN, &[0x00, 0x00, 0x00])])]);
    let (out, _cert) = run_inspect("universal-badlen-issuer", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.contains("issuer") && stderr.contains("UniversalString"),
        "reason must name the issuer name and UniversalString: {stderr}"
    );
}

#[test]
fn surrogate_code_points_in_universal_string_are_rejected() {
    // A lone high surrogate and a lone low surrogate are both illegal, in
    // either name and even between legal code points; nothing may be printed,
    // replaced or stripped.
    for cp in [0xD800u32, 0xDBFF, 0xDC00, 0xDFFF] {
        let content = universal_units(&[0x0041, cp, 0x0042]);
        let subject = name(&[rdn(&[atv_universal(CN, &content)])]);
        let issuer = simple_cn_name("Test CA");
        let (out, _cert) = run_inspect("universal-lone-surr", &build_cert(&subject, &issuer));

        assert_invalid_certificate(&out);
        let stderr = lossy(&out.stderr);
        assert!(
            stderr.contains("subject") && stderr.contains("UniversalString"),
            "U+{cp:04X} must fail as a subject UniversalString error: {stderr}"
        );
    }
}

#[test]
fn surrogate_pair_in_universal_string_is_rejected_not_merged() {
    // U+D83D U+DE00 would combine into U+1F600 under UTF-16 rules, but
    // UniversalString units are Unicode code points, not UTF-16 units: the
    // pair must fail the whole certificate, never be merged and displayed.
    let subject = name(&[rdn(&[atv_universal(
        CN,
        &universal_units(&[0xD83D, 0xDE00]),
    )])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("universal-surr-pair", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    assert!(lossy(&out.stderr).contains("UniversalString"));
}

#[test]
fn code_points_above_maximum_in_universal_string_are_rejected() {
    // U+110000 is the first value past the Unicode ceiling; larger values
    // including all-ones must fail even next to legal code points.
    for cp in [0x00110000u32, 0x001FFFFF, 0x7FFFFFFF, 0xFFFFFFFF] {
        let content = universal_units(&[0x0041, cp]);
        let subject = name(&[rdn(&[atv_universal(CN, &content)])]);
        let issuer = simple_cn_name("Test CA");
        let (out, _cert) = run_inspect("universal-too-large", &build_cert(&subject, &issuer));

        assert_invalid_certificate(&out);
        let stderr = lossy(&out.stderr);
        assert!(
            stderr.contains("subject") && stderr.contains("UniversalString"),
            "U+{cp:04X} must fail as a subject UniversalString error: {stderr}"
        );
    }
}

#[test]
fn too_large_universal_string_in_issuer_is_rejected_and_named() {
    // The same range rule applies to the Issuer name, and the reason names it.
    let subject = simple_cn_name("subject.example");
    let issuer = name(&[rdn(&[atv_universal(
        CN,
        &universal_units(&[0x00110000]),
    )])]);
    let (out, _cert) = run_inspect("universal-too-large-issuer", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.contains("issuer") && stderr.contains("UniversalString"),
        "reason must name the issuer name and UniversalString: {stderr}"
    );
}

#[test]
fn illegal_universal_string_at_unknown_oid_cannot_be_accepted_via_hex() {
    // Unknown attributes render as '#'-hex, but hex display is not an escape
    // hatch: incomplete length, surrogate code points and values past U+10FFFF
    // must fail even though the value would otherwise appear only as hex bytes.
    for content in [
        vec![0x00, 0x00, 0x00],                    // incomplete length
        universal_units(&[0xD800, 0xDC00]),        // surrogate pair
        universal_units(&[0x1F600, 0x00110000]),   // legal unit, then past-max
    ] {
        let subject = name(&[rdn(&[atv_universal(&[1, 2, 3, 4], &content)])]);
        let issuer = simple_cn_name("Test CA");
        let (out, _cert) = run_inspect("universal-bad-unknown", &build_cert(&subject, &issuer));

        assert_invalid_certificate(&out);
        assert!(lossy(&out.stderr).contains("UniversalString"));
    }
}

#[test]
fn illegal_universal_string_among_mixed_attributes_is_rejected() {
    // A legal mixed multi-valued RDN does not shield a later RDN whose CN
    // UniversalString carries a surrogate: mixed known/unknown attributes are
    // all checked.
    let subject = name(&[
        rdn(&[
            atv_universal(&[1, 2, 3, 4], &universal_units(&[0x1F600])),
            atv_universal(CN, &universal_units(&[0x1F600])),
        ]),
        rdn(&[atv_universal(CN, &universal_units(&[0xD800]))]),
    ]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("universal-bad-among-mixed", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.contains("subject") && stderr.contains("UniversalString"),
        "reason must name the subject name and UniversalString: {stderr}"
    );
}

#[test]
fn duplicate_illegal_universal_string_attributes_are_rejected() {
    // Repeating an attribute (allowed for equal SET OF members) repeats the
    // type check: two identical incomplete unknown UniversalString members
    // must still fail.
    let bad = atv_universal(&[1, 2, 3, 4], &[0x00, 0x00, 0x00]);
    let subject = name(&[rdn(&[bad.clone(), bad])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("universal-bad-duplicate", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    assert!(lossy(&out.stderr).contains("UniversalString"));
}


#[test]
fn truncated_attribute_value_is_rejected() {
    // The value header announces five content bytes but only three follow;
    // the enclosing SET/SEQUENCE lengths stay well-formed so truncation is
    // detected at the value itself.
    let mut inner = oid(&[1, 2, 3, 4]);
    inner.extend_from_slice(&[0x0C, 0x05, 0x61, 0x62, 0x63]); // len 5, 3 present
    let subject = name(&[rdn(&[seq(&inner)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("truncated-value", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
}

#[test]
fn non_minimal_value_length_encoding_is_rejected() {
    // A three-byte value encoded as 0x81 0x03 is not valid DER.
    let mut inner = oid(&[1, 2, 3, 4]);
    inner.extend_from_slice(&[0x0C, 0x81, 0x03, 0x61, 0x62, 0x63]);
    let subject = name(&[rdn(&[seq(&inner)])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("nonminimal-len", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
}

#[test]
fn truncated_certificate_file_is_rejected() {
    // Bytes cut off the tail of an otherwise valid certificate: malformed
    // input, same failure contract.
    let subject = simple_cn_name("subject.example");
    let issuer = simple_cn_name("Test CA");
    let mut bytes = build_cert(&subject, &issuer);
    bytes.truncate(bytes.len() - 5);
    let (out, _cert) = run_inspect("truncated-cert", &bytes);

    assert_invalid_certificate(&out);
}

// ----- DER SET OF ordering inside multi-valued RDNs ------------------------

#[test]
fn unsorted_subject_multivalued_rdn_is_rejected() {
    // O and CN in one RDN, encoded O-first: both members have the same outer
    // tag and length, and O's OID arc byte 0x0A is greater than CN's 0x03, so
    // the SET OF is not byte-sorted. This must fail as a corrupt certificate
    // and name the subject, never be printed or repaired by reordering the
    // members (alphabetically "CN" would sort first, which is exactly the
    // kind of display-name comparison that must not be used).
    let subject = name(&[rdn(&[atv_utf8(O, "y"), atv_utf8(CN, "x")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("bad-order-subject", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.contains("subject") && stderr.contains("order"),
        "reason must name the subject name and the ordering problem: {stderr}"
    );
}

#[test]
fn unsorted_issuer_multivalued_rdn_is_rejected() {
    // Same violation in the issuer must be reported against the issuer name,
    // even though the subject is well-formed.
    let subject = simple_cn_name("subject.example");
    let issuer = name(&[rdn(&[atv_utf8(O, "y"), atv_utf8(CN, "x")])]);
    let (out, _cert) = run_inspect("bad-order-issuer", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.contains("issuer") && stderr.contains("order"),
        "reason must name the issuer name and the ordering problem: {stderr}"
    );
}

#[test]
fn ordering_compares_encoding_content_not_attribute_text() {
    // The same unknown OID twice (identical dotted text) with values "b" and
    // "a": both complete encodings have equal tag and length, and the first
    // value byte 0x62 > 0x61, so "b" before "a" is out of DER order. Sorting
    // by attribute name or value text would call these identical or sort the
    // same way only by accident; this pins the comparison to encoding bytes.
    let subject = name(&[rdn(&[
        atv_utf8(&[1, 2, 3, 4], "b"),
        atv_utf8(&[1, 2, 3, 4], "a"),
    ])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("bad-order-content", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
}

#[test]
fn ordering_compares_outer_length_before_value_text() {
    // CN="z" (complete ATV encoding 10 bytes) and 1.2.3.4="aaa" (12 bytes,
    // because its value is longer): DER compares tag, then the outer length
    // byte, so the shorter CN member sorts first even though the decoded
    // value text "aaa" would come well before "z". A comparison based on
    // decoded values would reject this legal certificate.
    let subject = name(&[rdn(&[
        atv_utf8(CN, "z"),
        atv_utf8(&[1, 2, 3, 4], "aaa"),
    ])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("order-by-length", &build_cert(&subject, &issuer));

    assert_success(&out, "CN=z+1.2.3.4=#0C03616161", "CN=Test CA");

    // The reverse encoding is corrupt: the longer outer encoding must not
    // come first, regardless of the '#' text representation.
    let subject = name(&[rdn(&[
        atv_utf8(&[1, 2, 3, 4], "aaa"),
        atv_utf8(CN, "z"),
    ])]);
    let (out, _cert) = run_inspect("order-by-length-bad", &build_cert(&subject, &issuer));
    assert_invalid_certificate(&out);
}

#[test]
fn each_rdn_is_checked_independently() {
    // Two RDNs in one name: the first (outermost) group is fine, a later
    // group carries O-before-CN out of order (OID arc 0x0A > 0x03). The
    // whole name must be rejected rather than treated as one set or checked
    // only on a group of the checker's choosing.
    let subject = name(&[
        rdn(&[atv_utf8(C, "CN")]),
        rdn(&[atv_utf8(O, "y"), atv_utf8(CN, "x")]),
    ]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("bad-order-second-rdn", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    assert!(lossy(&out.stderr).contains("subject"));
}

#[test]
fn identical_adjacent_members_are_allowed_and_not_deduplicated() {
    // Two members with byte-for-byte identical complete encodings compare
    // equal and therefore satisfy the sorted SET OF rule; both must still
    // appear in the output (no merging, no false ordering failure).
    let subject = name(&[rdn(&[
        atv_utf8(CN, "dup"),
        atv_utf8(CN, "dup"),
    ])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("dup-members", &build_cert(&subject, &issuer));

    assert_success(&out, "CN=dup+CN=dup", "CN=Test CA");
}

#[test]
fn single_attribute_rdn_always_satisfies_set_ordering() {
    // A SET OF with one member is trivially sorted, whatever the attribute
    // and value encodings are.
    let subject = name(&[
        rdn(&[atv_utf8(C, "CN")]),
        rdn(&[atv_utf8(O, "org")]),
        rdn(&[atv_utf8(CN, "host")]),
    ]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("single-member-rdns", &build_cert(&subject, &issuer));

    assert_success(&out, "CN=host,O=org,C=CN", "CN=Test CA");
}

// ----- CLI contract: usage, version and unreadable files ------------------

#[test]
fn version_flag_keeps_its_output() {
    let out = Command::new(env!("CARGO_BIN_EXE_chainview"))
        .arg("--version")
        .output()
        .expect("running chainview");
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty());
    assert_eq!(lossy(&out.stdout), "chainview 0.1.0\n");
}

#[test]
fn missing_path_is_a_usage_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_chainview"))
        .arg("inspect")
        .output()
        .expect("running chainview");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert_eq!(
        lossy(&out.stderr),
        "Usage: chainview inspect <DER-FILE>\n       chainview --version\n"
    );
}

#[test]
fn unreadable_file_keeps_its_error_contract() {
    let missing = std::env::temp_dir().join("chainview-name-test-definitely-missing.der");
    let _ = fs::remove_file(&missing);
    let out = Command::new(env!("CARGO_BIN_EXE_chainview"))
        .arg("inspect")
        .arg(&missing)
        .output()
        .expect("running chainview");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.starts_with(&format!(
            "chainview: cannot read certificate file '{}': ",
            missing.display()
        )),
        "unexpected stderr: {stderr}"
    );
}

// ----- Assertions / process plumbing --------------------------------------

fn assert_success(out: &Output, subject: &str, issuer: &str) {
    assert_eq!(out.status.code(), Some(0), "stderr={}", lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "stderr={}", lossy(&out.stderr));
    assert_eq!(
        lossy(&out.stdout),
        format!(
            "Subject: {subject}\n\
             Issuer: {issuer}\n\
             Serial Number: {FIXED_SERIAL}\n\
             Not Before: {FIXED_NOT_BEFORE}\n\
             Not After: {FIXED_NOT_AFTER}\n"
        )
    );
}

/// Every malformed-input case shares the documented contract: exit code 2,
/// nothing on stdout (so no name field can be printed before the error is
/// noticed), and the fixed invalid-certificate message prefix on stderr.
fn assert_invalid_certificate(out: &Output) {
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout={} stderr={}",
        lossy(&out.stdout),
        lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "stdout must be empty on failure, got: {}",
        lossy(&out.stdout)
    );
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.starts_with("chainview: invalid DER certificate: "),
        "unexpected stderr: {stderr}"
    );
    // A non-empty reason after the fixed prefix.
    assert!(
        stderr["chainview: invalid DER certificate: ".len()..].trim().len() > 1,
        "missing error reason in stderr: {stderr}"
    );
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn run_inspect(tag: &str, data: &[u8]) -> (Output, TempCert) {
    let cert = TempCert::new(tag, data);
    let output = Command::new(env!("CARGO_BIN_EXE_chainview"))
        .arg("inspect")
        .arg(&cert.path)
        .output()
        .expect("running chainview");
    (output, cert)
}

struct TempCert {
    path: PathBuf,
}

impl TempCert {
    fn new(tag: &str, data: &[u8]) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "chainview-name-test-{}-{tag}-{unique}.der",
            std::process::id()
        ));
        fs::write(&path, data).expect("writing temp certificate");
        TempCert { path }
    }
}

impl Drop for TempCert {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

// ----- Certificate / DER construction -------------------------------------
//
// Only the structural pieces needed here. Lengths are always emitted in the
// minimal DER form, except where a test injects raw bytes to exercise
// rejection of non-minimal or truncated encodings.

fn simple_cn_name(cn: &str) -> Vec<u8> {
    name(&[rdn(&[atv_utf8(CN, cn)])])
}

/// Assemble a complete, parseable v1 DER certificate with fixed serial,
/// validity and key material around the supplied Subject/Issuer names.
fn build_cert(subject: &[u8], issuer: &[u8]) -> Vec<u8> {
    let serial = tlv(0x02, &[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]);
    let validity = seq(&concat(&[
        &tlv(0x17, b"260115093000Z"), // UTCTime 2026-01-15T09:30:00Z
        &tlv(0x17, b"270115093000Z"), // UTCTime 2027-01-15T09:30:00Z
    ]));
    let spki = seq(&concat(&[
        &seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 1]), &tlv(0x05, &[])])),
        &tlv(0x03, &[0x00, 0x01, 0x00]), // BIT STRING, zero unused bits
    ]));

    let tbs = seq(&concat(&[
        &serial,
        &signature_algorithm(),
        issuer,
        &validity,
        subject,
        &spki,
    ]));
    seq(&concat(&[&tbs, &signature_algorithm(), &tlv(0x03, &[0x00])]))
}

fn signature_algorithm() -> Vec<u8> {
    // sha256WithRSAEncryption with explicit NULL parameters.
    seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 11]), &tlv(0x05, &[])]))
}

fn name(rdns: &[Vec<u8>]) -> Vec<u8> {
    tlv(
        0x30,
        &concat(&rdns.iter().map(Vec::as_slice).collect::<Vec<_>>()),
    )
}

fn rdn(atvs: &[Vec<u8>]) -> Vec<u8> {
    tlv(
        0x31,
        &concat(&atvs.iter().map(Vec::as_slice).collect::<Vec<_>>()),
    )
}

/// AttributeTypeAndValue ::= SEQUENCE { type OID, value ANY } — `value` is the
/// complete DER TLV of the value.
fn atv(arcs: &[u64], value: &[u8]) -> Vec<u8> {
    seq(&concat(&[&oid(arcs), value]))
}

fn atv_utf8(arcs: &[u64], text: &str) -> Vec<u8> {
    atv(arcs, &tlv(0x0C, text.as_bytes()))
}

fn atv_printable(arcs: &[u64], bytes: &[u8]) -> Vec<u8> {
    atv(arcs, &tlv(0x13, bytes))
}

fn atv_ia5(arcs: &[u64], bytes: &[u8]) -> Vec<u8> {
    atv(arcs, &tlv(0x16, bytes))
}

fn atv_bmp(arcs: &[u64], content: &[u8]) -> Vec<u8> {
    atv(arcs, &tlv(0x1E, content))
}

fn atv_universal(arcs: &[u64], content: &[u8]) -> Vec<u8> {
    atv(arcs, &tlv(0x1C, content))
}

/// Big-endian 16-bit units, the wire form of a BMPString's characters.
fn bmp_units(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|u| u.to_be_bytes()).collect()
}

/// Big-endian 32-bit code points, the wire form of a UniversalString's units.
fn universal_units(units: &[u32]) -> Vec<u8> {
    units.iter().flat_map(|u| u.to_be_bytes()).collect()
}

fn oid(arcs: &[u64]) -> Vec<u8> {
    assert!(arcs.len() >= 2);
    let mut content = oid_arc(arcs[0] * 40 + arcs[1]);
    for &arc in &arcs[2..] {
        content.extend_from_slice(&oid_arc(arc));
    }
    tlv(0x06, &content)
}

fn oid_arc(mut value: u64) -> Vec<u8> {
    let mut out = vec![(value % 128) as u8];
    value /= 128;
    while value > 0 {
        out.push(0x80 | (value % 128) as u8);
        value /= 128;
    }
    out.reverse();
    out
}

fn seq(content: &[u8]) -> Vec<u8> {
    tlv(0x30, content)
}

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend_from_slice(&der_len(content.len()));
    out.extend_from_slice(content);
    out
}

fn der_len(n: usize) -> Vec<u8> {
    if n < 0x80 {
        vec![n as u8]
    } else {
        let mut body = Vec::new();
        let mut v = n;
        while v > 0 {
            body.push((v & 0xFF) as u8);
            v >>= 8;
        }
        body.reverse();
        let mut out = vec![0x80 | body.len() as u8];
        out.extend_from_slice(&body);
        out
    }
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(part);
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        write!(s, "{b:02X}").unwrap();
    }
    s
}
