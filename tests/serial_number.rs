//! End-to-end regression tests for the serial-number decoding and display
//! behavior documented in README.md.
//!
//! Like `tests/name_display.rs` and `tests/validity_display.rs`, these drive
//! the real `chainview` binary so the public contract is locked as a whole:
//! on success the five fields keep their fixed order with stderr empty, and a
//! malformed serial number fails the whole inspect with exit code 2, empty
//! stdout and the `chainview: invalid DER certificate:` stderr prefix with a
//! non-empty reason — even when the names and validity decode fine.
//!
//! What is pinned here is the difference between the integer *value* and its
//! DER *encoding*: a leading 0x00 sign-padding byte is not part of the number,
//! while 0x00 bytes inside or at the end of the value are. The hex rendering
//! is the value itself — never decimal, never truncated to a machine integer
//! width. Signature verification, trust and expiry stay out of scope: serial
//! validity is judged by the encoding rules alone.
//!
//! Coverage:
//! - sign padding stripped (`00 80` -> `80`, `00 FF 01` -> `FF01`);
//! - unpadded values shown verbatim (`7F` -> `7F`);
//! - interior and trailing 0x00 value bytes preserved;
//! - values wider than eight bytes displayed in full;
//! - a single 0x00 content byte (the value zero) shown as `00`;
//! - rejection of empty integers, negative integers and unnecessary leading
//!   zero bytes (`00 7F`, `00 00`), without stripping the zero and accepting.
//!
//! Certificates are built with the tiny zero-dependency DER constructor at the
//! bottom of this file so individual serial encodings can be crafted byte for
//! byte.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXED_SUBJECT: &str = "CN=example.com";
const FIXED_ISSUER: &str = "CN=Test CA";
const FIXED_NOT_BEFORE: &str = "2026-01-15T09:30:00Z";
const FIXED_NOT_AFTER: &str = "2027-01-15T09:30:00Z";

// ----- Success: sign padding is not part of the value -----------------------

#[test]
fn sign_padding_zero_byte_is_not_part_of_the_value() {
    // A leading 0x00 exists only to keep the integer positive when the value's
    // own top bit is set; it must not appear in the output.
    let (out, _cert) = run_inspect("pad-0080", &cert_with_serial(&[0x00, 0x80]));
    assert_serial_success(&out, "80");

    // Padding plus a multi-byte value: only the first byte is padding.
    let (out, _cert) = run_inspect("pad-00ff01", &cert_with_serial(&[0x00, 0xFF, 0x01]));
    assert_serial_success(&out, "FF01");
}

#[test]
fn high_bit_clear_values_need_no_padding_and_display_verbatim() {
    // 0x7F is positive on its own: no padding in the encoding, none stripped.
    let (out, _cert) = run_inspect("nopad-7f", &cert_with_serial(&[0x7F]));
    assert_serial_success(&out, "7F");

    let (out, _cert) = run_inspect("nopad-01", &cert_with_serial(&[0x01]));
    assert_serial_success(&out, "01");

    // The serial used across the sibling test files: eight value bytes, top
    // bit of the first byte clear, shown as-is in upper-case hex.
    let (out, _cert) = run_inspect(
        "nopad-fixed",
        &cert_with_serial(&[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]),
    );
    assert_serial_success(&out, "0E8A4C2F9B17D603");
}

#[test]
fn zero_bytes_inside_and_at_the_end_are_value_bytes() {
    // Only a single leading 0x00 can be sign padding. Every 0x00 after the
    // first value byte belongs to the number and must survive as "00" pairs.
    let (out, _cert) = run_inspect("inner-zero", &cert_with_serial(&[0x01, 0x00, 0x01]));
    assert_serial_success(&out, "010001");

    let (out, _cert) = run_inspect("trailing-zero", &cert_with_serial(&[0x10, 0x00]));
    assert_serial_success(&out, "1000");

    // Padding followed by a value that itself contains 0x00 bytes.
    let (out, _cert) = run_inspect(
        "pad-inner-zeros",
        &cert_with_serial(&[0x00, 0x80, 0x00, 0x80]),
    );
    assert_serial_success(&out, "800080");
}

#[test]
fn values_wider_than_eight_bytes_display_in_full() {
    // The serial is rendered from its bytes, not through a machine integer:
    // nothing may be truncated to 64 bits or converted to decimal.
    let nine = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09];
    let (out, _cert) = run_inspect("wide-9", &cert_with_serial(&nine));
    assert_serial_success(&out, "010203040506070809");

    // Nine value bytes whose top bit is set: ten content bytes on the wire.
    let mut padded_nine = vec![0x00];
    padded_nine.extend_from_slice(&[0xFF; 9]);
    let (out, _cert) = run_inspect("wide-padded-9", &cert_with_serial(&padded_nine));
    assert_serial_success(&out, &"FF".repeat(9));

    // Sixteen value bytes, well beyond any machine word.
    let mut padded_sixteen = vec![0x00, 0x80];
    padded_sixteen.extend_from_slice(&[0x01; 15]);
    let (out, _cert) = run_inspect("wide-padded-16", &cert_with_serial(&padded_sixteen));
    assert_serial_success(&out, &format!("80{}", "01".repeat(15)));
}

#[test]
fn single_zero_content_byte_is_the_value_zero() {
    // INTEGER 0 encodes as one 0x00 content byte; that byte *is* the value,
    // not padding, and displays as "00".
    let (out, _cert) = run_inspect("zero", &cert_with_serial(&[0x00]));
    assert_serial_success(&out, "00");
}

#[test]
fn expired_or_not_yet_valid_certificate_still_displays_its_serial() {
    // Serial legality is judged by the encoding rules alone; `inspect` never
    // judges whether the certificate is currently valid. A long-expired and a
    // far-future certificate both display, so the assertion holds on any
    // execution date.
    let expired = cert_with_serial_and_validity(
        &[0x00, 0x80],
        &tlv(0x17, b"800101000000Z"), // 1980-01-01T00:00:00Z
        &tlv(0x17, b"990101000000Z"), // 1999-01-01T00:00:00Z
    );
    let (out, _cert) = run_inspect("expired", &expired);
    assert_success(
        &out,
        "80",
        "1980-01-01T00:00:00Z",
        "1999-01-01T00:00:00Z",
    );

    let future = cert_with_serial_and_validity(
        &[0x7F],
        &tlv(0x18, b"20990101000000Z"), // 2099-01-01T00:00:00Z
        &tlv(0x18, b"21000101000000Z"), // 2100-01-01T00:00:00Z
    );
    let (out, _cert) = run_inspect("not-yet-valid", &future);
    assert_success(
        &out,
        "7F",
        "2099-01-01T00:00:00Z",
        "2100-01-01T00:00:00Z",
    );
}

// ----- Failure: malformed serial numbers ------------------------------------

#[test]
fn empty_serial_number_is_rejected() {
    // An INTEGER with no content bytes has no value at all.
    let (out, _cert) = run_inspect("empty", &cert_with_serial(&[]));
    assert_invalid_certificate(&out);
}

#[test]
fn negative_serial_number_is_rejected() {
    // A set top bit in the first content byte makes the integer negative;
    // it must not be displayed as if it were a positive hex string.
    let cases: &[&[u8]] = &[
        &[0x80],             // -128, not "80"
        &[0xFF],             // -1, not "FF"
        &[0x80, 0x00],       // negative multi-byte
        &[0xFF, 0xFF, 0x01], // negative multi-byte
    ];
    for (i, content) in cases.iter().enumerate() {
        let (out, _cert) = run_inspect(&format!("negative-{i}"), &cert_with_serial(content));
        assert_invalid_certificate(&out);
    }
}

#[test]
fn unnecessary_leading_zero_byte_is_rejected() {
    // A leading 0x00 is only legal when the next byte's top bit is set.
    // Otherwise the encoding is non-minimal: the zero must not be stripped
    // and the remainder accepted.
    let cases: &[&[u8]] = &[
        &[0x00, 0x7F], // "7F" needs no padding
        &[0x00, 0x00], // zero needs no padding
        &[0x00, 0x01], // "01" needs no padding
        &[0x00, 0x00, 0x80], // one padding byte is enough
    ];
    for (i, content) in cases.iter().enumerate() {
        let (out, _cert) = run_inspect(&format!("overpadded-{i}"), &cert_with_serial(content));
        assert_invalid_certificate(&out);
    }
}

#[test]
fn non_minimal_negative_padding_is_rejected() {
    // 0xFF is the sign-extension byte for negative integers; the same
    // minimality rule applies to it.
    let cases: &[&[u8]] = &[
        &[0xFF, 0x7F], // top bit of 0x7F clear: 0xFF is redundant
        &[0xFF, 0x00], // top bit of 0x00 clear: 0xFF is redundant
    ];
    for (i, content) in cases.iter().enumerate() {
        let (out, _cert) = run_inspect(&format!("ff-padded-{i}"), &cert_with_serial(content));
        assert_invalid_certificate(&out);
    }
}

// ----- Assertions / process plumbing --------------------------------------

/// A successful inspect must show all five fields in the fixed order with the
/// fixed name and time values — the serial encoding never influences them —
/// and must leave stderr empty.
fn assert_serial_success(out: &Output, serial: &str) {
    assert_success(out, serial, FIXED_NOT_BEFORE, FIXED_NOT_AFTER);
}

fn assert_success(out: &Output, serial: &str, not_before: &str, not_after: &str) {
    assert_eq!(out.status.code(), Some(0), "stderr={}", lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "stderr={}", lossy(&out.stderr));
    assert_eq!(
        lossy(&out.stdout),
        format!(
            "Subject: {FIXED_SUBJECT}\n\
             Issuer: {FIXED_ISSUER}\n\
             Serial Number: {serial}\n\
             Not Before: {not_before}\n\
             Not After: {not_after}\n"
        )
    );
}

/// Every malformed-serial case shares the documented contract: exit code 2,
/// nothing on stdout (so neither a name nor a time field is printed before
/// the error is noticed), and the fixed invalid-certificate prefix on stderr
/// with a non-empty reason after it.
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
            "chainview-serial-test-{}-{tag}-{unique}.der",
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
// Mirrors the constructor in tests/name_display.rs, except the serial number
// is supplied as raw INTEGER content bytes so each test controls the exact
// encoding under test. Lengths are always emitted in the minimal DER form.

const CN: &[u64] = &[2, 5, 4, 3];

/// Assemble a complete, parseable v1 DER certificate around the supplied
/// serial-number INTEGER content, with fixed names, validity and key
/// material — everything else decodes cleanly, so any failure comes from
/// the serial number itself.
fn cert_with_serial(serial_content: &[u8]) -> Vec<u8> {
    cert_with_serial_and_validity(
        serial_content,
        &tlv(0x17, b"260115093000Z"), // UTCTime 2026-01-15T09:30:00Z
        &tlv(0x17, b"270115093000Z"), // UTCTime 2027-01-15T09:30:00Z
    )
}

fn cert_with_serial_and_validity(
    serial_content: &[u8],
    not_before: &[u8],
    not_after: &[u8],
) -> Vec<u8> {
    let serial = tlv(0x02, serial_content);
    let validity = seq(&concat(&[not_before, not_after]));
    let spki = seq(&concat(&[
        &seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 1]), &tlv(0x05, &[])])),
        &tlv(0x03, &[0x00, 0x01, 0x00]), // BIT STRING, zero unused bits
    ]));

    let tbs = seq(&concat(&[
        &serial,
        &signature_algorithm(),
        &simple_cn_name("Test CA"),
        &validity,
        &simple_cn_name("example.com"),
        &spki,
    ]));
    seq(&concat(&[&tbs, &signature_algorithm(), &tlv(0x03, &[0x00])]))
}

fn signature_algorithm() -> Vec<u8> {
    // sha256WithRSAEncryption with explicit NULL parameters.
    seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 11]), &tlv(0x05, &[])]))
}

fn simple_cn_name(cn: &str) -> Vec<u8> {
    let atv = seq(&concat(&[&oid(CN), &tlv(0x0C, cn.as_bytes())]));
    let rdn = tlv(0x31, &atv); // SET { CN=... }
    tlv(0x30, &rdn) // SEQUENCE { RDN }
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
