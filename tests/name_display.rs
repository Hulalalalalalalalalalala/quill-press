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
    // groups; its members are encoded in DER order (ascending complete
    // encodings — here the shorter 1.2.3.4 attribute TLVs sort before CN);
    // the repeated unknown attribute appears twice (never merged); the
    // adjacent CN stays text; CJK content passes through as raw UTF-8; groups
    // are shown in reverse order; '#' is the encoding marker, unescaped.
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
    // (1.2.3.4), encoded in DER order (the 1.2.3.4 TLV is shortest, the CN
    // TLV longest). The group stays joined with '+'; a preceding text RDN and
    // a repeated text CN in another RDN keep their order after reversal.
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

// ----- DER SET OF ordering within one RDN ----------------------------------

#[test]
fn identical_attributes_may_repeat_within_an_rdn() {
    // Two byte-identical members compare equal, so DER order allows them;
    // both are displayed, never deduplicated and never flagged as unsorted.
    let subject = name(&[rdn(&[atv_utf8(CN, "dup"), atv_utf8(CN, "dup")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("identical-atvs", &build_cert(&subject, &issuer));

    assert_success(&out, "CN=dup+CN=dup", "CN=Test CA");
}

#[test]
fn der_order_compares_full_encodings_not_decoded_values() {
    // CN "a" versus unknown 1.2.3.4 "z": as text 'a' < 'z', but the complete
    // DER encodings compare 06 03 2A... < 06 03 55..., so the unknown
    // attribute must be encoded first. That order is legal and is displayed
    // exactly as encoded.
    let subject = name(&[rdn(&[atv_utf8(&[1, 2, 3, 4], "z"), atv_utf8(CN, "a")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("der-order-bytes", &build_cert(&subject, &issuer));

    assert_success(&out, "1.2.3.4=#0C017A+CN=a", "CN=Test CA");
}

#[test]
fn unsorted_rdn_in_subject_is_rejected() {
    // The same pair as above in the opposite — text-alphabetical — order:
    // the CN TLV sorts after the 1.2.3.4 TLV, so this SET OF violates DER
    // ordering even though every attribute is individually well-formed.
    let subject = name(&[rdn(&[atv_utf8(CN, "a"), atv_utf8(&[1, 2, 3, 4], "z")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("unsorted-subject", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.contains("subject"),
        "stderr should name the subject name: {stderr}"
    );
    assert!(
        stderr.contains("DER order"),
        "stderr should cite the DER ordering violation: {stderr}"
    );
}

#[test]
fn unsorted_rdn_in_issuer_is_rejected() {
    // The rule applies to the Issuer name independently: a valid subject
    // cannot salvage a certificate whose issuer RDN is unsorted, and nothing
    // may be printed before the failure.
    let subject = simple_cn_name("subject.example");
    let issuer = name(&[rdn(&[
        atv_utf8(CN, "Example CA"),
        atv_utf8(&[1, 2, 3, 4], "abc"),
    ])]);
    let (out, _cert) = run_inspect("unsorted-issuer", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.contains("issuer"),
        "stderr should name the issuer name: {stderr}"
    );
    assert!(
        stderr.contains("DER order"),
        "stderr should cite the DER ordering violation: {stderr}"
    );
}

#[test]
fn one_unsorted_rdn_among_sorted_ones_is_rejected() {
    // Each RDN is checked as its own SET OF: the second group is unsorted
    // even though the first is fine and the RDN sequence itself is legal.
    let subject = name(&[
        rdn(&[atv_utf8(C, "CN")]),
        rdn(&[atv_utf8(CN, "b"), atv_utf8(CN, "a")]),
    ]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("one-unsorted-rdn", &build_cert(&subject, &issuer));

    assert_invalid_certificate(&out);
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
