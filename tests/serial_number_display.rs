//! End-to-end regression tests for the serial-number decoding and display
//! behavior documented in README.md.
//!
//! The other suites build every certificate around one fixed serial
//! (`0E8A4C2F9B17D603`), which leaves the DER INTEGER sign-padding boundary
//! unprotected: a positive INTEGER whose top value bit is set carries one
//! leading `0x00` purely to keep the number positive, and that padding byte is
//! not part of the value. These tests drive the real `chainview` binary with
//! hand-built serial contents so the distinction is locked byte for byte:
//!
//! Success (exit code 0, empty stderr, five fields in their fixed order with
//! the fixed names and UTC times unchanged):
//! - the mandatory sign padding is stripped: `00 80` -> `80`,
//!   `00 FF 01` -> `FF01`, while `7F` (no padding) stays `7F`;
//! - a single `00` is zero and displays `00`;
//! - zero bytes in the middle or at the end of the value are preserved, and
//!   every value byte keeps two uppercase hex digits with no prefix, space or
//!   separator;
//! - positive serials longer than eight value bytes render in full — nothing
//!   is parsed into a machine-width integer or truncated;
//! - expiry / not-yet-valid windows never gate serial display.
//!
//! Failure (exit code 2, empty stdout, stderr starting with
//! `chainview: invalid DER certificate:` followed by a non-empty reason),
//! even though the name and validity fields in the same certificate are
//! perfectly readable:
//! - an empty INTEGER (`02 00`);
//! - negative integers (`80`, `FF`, ...);
//! - superfluous leading zero bytes on a positive value (`00 7F`, `00 00`,
//!   `00 01`, ...): the encoder may not strip the zeros and accept it.
//!
//! Signature verification and trust are out of scope, as everywhere else.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXED_SUBJECT: &str = "CN=example.com";
const FIXED_ISSUER: &str = "CN=Test CA";
const FIXED_NOT_BEFORE: &str = "2030-01-01T00:00:00Z";
const FIXED_NOT_AFTER: &str = "2031-01-01T00:00:00Z";

// ----- Known X.520 attribute OID arcs -------------------------------------

const CN: &[u64] = &[2, 5, 4, 3];

const TAG_INTEGER: u8 = 0x02;
const TAG_UTC_TIME: u8 = 0x17;
const TAG_GENERALIZED_TIME: u8 = 0x18;

// ----- Success: sign padding vs. value bytes -------------------------------

#[test]
fn sign_padding_zero_is_stripped_but_value_bytes_are_kept() {
    // The leading 00 exists only so the INTEGER content stays positive; it is
    // not a value byte. The bytes after it — including trailing zeros — are
    // the number and render verbatim.
    let cases: &[(&[u8], &str)] = &[
        (&[0x00, 0x80], "80"),             // +128, padding mandatory
        (&[0x00, 0xFF], "FF"),             // +255
        (&[0x00, 0xFF, 0x01], "FF01"),     // +65281
        (&[0x00, 0x80, 0x00], "8000"),     // trailing value zero kept
        (&[0x00, 0xAB, 0xCD, 0xEF], "ABCDEF"),
        (&[0x00, 0x9A, 0x00, 0x7F], "9A007F"), // interior value zero kept
    ];
    for (i, &(content, expected)) in cases.iter().enumerate() {
        let (out, _cert) = run_inspect(&format!("padding-{i}"), &cert(content));
        assert_serial_success(&out, expected);
    }
}

#[test]
fn unpadded_positive_serial_renders_verbatim() {
    // With the top bit clear no padding is allowed, and none is added in the
    // output: the value bytes map 1:1 to hex pairs, small values keeping their
    // own leading-zero nibble (`01`, `0A` are two digits, never `1`/`A`).
    let cases: &[(&[u8], &str)] = &[
        (&[0x7F], "7F"),
        (&[0x01], "01"),
        (&[0x0A], "0A"),
        (&[0x00], "00"), // zero is its own case, pinned here too
        (&[0x01, 0x7F], "017F"),
        (&[0x7F, 0xFF, 0x01], "7FFF01"),
    ];
    for (i, &(content, expected)) in cases.iter().enumerate() {
        let (out, _cert) = run_inspect(&format!("unpadded-{i}"), &cert(content));
        assert_serial_success(&out, expected);
    }
}

#[test]
fn single_zero_byte_is_the_zero_serial_and_displays_00() {
    // DER encodes INTEGER zero as one content byte 00; the display keeps one
    // two-digit byte, not an empty string and not a decimal `0`.
    let (out, _cert) = run_inspect("zero", &cert(&[0x00]));
    assert_serial_success(&out, "00");
}

#[test]
fn interior_and_trailing_zero_bytes_are_never_dropped() {
    // Only the one sign-padding byte may ever disappear. A 00 anywhere else
    // is a real digit: losing it would change the number (e.g. 0x0100 -> 0x01).
    let cases: &[(&[u8], &str)] = &[
        (&[0x12, 0x00, 0x34], "120034"),
        (&[0x01, 0x00], "0100"),
        (&[0x10, 0x00, 0x00, 0x00], "10000000"),
        (&[0x00, 0xFF, 0x00], "FF00"), // padding stripped, final 00 kept
        (&[0x00, 0x80, 0x00, 0x01], "800001"),
    ];
    for (i, &(content, expected)) in cases.iter().enumerate() {
        let (out, _cert) = run_inspect(&format!("inner-zero-{i}"), &cert(content));
        assert_serial_success(&out, expected);
    }
}

#[test]
fn serial_longer_than_eight_bytes_is_not_truncated_to_machine_width() {
    // The serial is rendered from its bytes, never converted to u64/u128, so
    // a positive value wider than any machine integer survives whole. Nine
    // value bytes (with and without the padding byte) and a full twenty.
    let nine: [u8; 9] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09];
    let (out, _cert) = run_inspect("nine-value-bytes", &cert(&nine));
    assert_serial_success(&out, "010203040506070809");

    // Nine value bytes that need a sign-padding byte first.
    let nine_padded: [u8; 10] = [0x00, 0x91, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09];
    let (out, _cert) = run_inspect("nine-padded", &cert(&nine_padded));
    assert_serial_success(&out, "910203040506070809");

    // Twenty value bytes (40 hex digits): first byte 01 needs no padding.
    let twenty: Vec<u8> = (1..=20).collect();
    let (out, _cert) = run_inspect("twenty-value-bytes", &cert(&twenty));
    let expected = "0102030405060708090A0B0C0D0E0F1011121314";
    assert_eq!(expected.len(), 40);
    assert_serial_success(&out, expected);
    let stdout = lossy(&out.stdout);
    let shown = serial_hex_field(&stdout);
    assert_eq!(shown.len(), 40, "serial must not be cut to a machine-width window");
    assert!(shown.ends_with("1314"), "tail bytes lost: {shown}");
}

#[test]
fn serial_hex_is_uppercase_without_prefix_space_or_separator() {
    // Formatting contract pinned explicitly: no 0x, no whitespace, no `:` or
    // other grouping, lowercase hex letters never appear, and every byte is
    // exactly two digits.
    let (out, _cert) = run_inspect(
        "format",
        &cert(&[0x00, 0xAB, 0xCD, 0xEF, 0x0A, 0x0F]),
    );
    assert_serial_success(&out, "ABCDEF0A0F");
    let stdout = lossy(&out.stdout);
    let shown = serial_hex_field(&stdout);
    assert_eq!(shown, "ABCDEF0A0F");
    assert!(!shown.starts_with("0x") && !shown.starts_with("0X"));
    assert!(!shown.contains(' '));
    assert!(!shown.contains(':'));
    assert!(!shown.contains('-'));
    assert!(!shown.bytes().any(|b| b.is_ascii_lowercase()));
}

// ----- Success: validity windows never gate serial display -----------------

#[test]
fn padded_serial_displays_on_expired_and_not_yet_valid_certificates() {
    // Serial legality is purely an encoding question. A correctly padded
    // serial (+128) is shown on a certificate that expired decades ago and on
    // one valid only far in the future; "today" is never consulted.
    let expired = build_cert(
        &[0x00, 0x80],
        &tlv(TAG_UTC_TIME, b"800101000000Z"),
        &tlv(TAG_UTC_TIME, b"990101000000Z"),
    );
    let (out, _cert) = run_inspect("expired-padded", &expired);
    assert_serial_success_at(&out, "80", "1980-01-01T00:00:00Z", "1999-01-01T00:00:00Z");

    let future = build_cert(
        &[0x00, 0xFF, 0x01],
        &tlv(TAG_GENERALIZED_TIME, b"20990101000000Z"),
        &tlv(TAG_GENERALIZED_TIME, b"21000101000000Z"),
    );
    let (out, _cert) = run_inspect("future-padded", &future);
    assert_serial_success_at(&out, "FF01", "2099-01-01T00:00:00Z", "2100-01-01T00:00:00Z");
}

// ----- Failure: empty / negative / non-minimal INTEGER ---------------------

#[test]
fn empty_serial_integer_fails_the_whole_inspection() {
    // `02 00` is a well-formed TLV but an INTEGER has no content; the name and
    // validity are fine, yet no partial output may appear.
    let (out, _cert) = run_inspect("empty-integer", &cert(&[]));
    assert_invalid_certificate(&out);
}

#[test]
fn negative_serial_integers_are_rejected() {
    // A high bit set with no padding 00 makes the content negative, whether
    // the value is a one-byte -128/-1 or a longer negative number. None may
    // be salvaged by displaying the raw bytes as a positive hex string.
    let negatives: &[&[u8]] = &[
        &[0x80],             // -128
        &[0xFF],             // -1
        &[0x80, 0x00],       // -32768 (minimal negative encoding)
        &[0xFE, 0x01],       // negative, two bytes
        &[0xFF, 0xFF, 0xFF], // -1 over three bytes
    ];
    for (i, content) in negatives.iter().enumerate() {
        let (out, _cert) = run_inspect(&format!("negative-{i}"), &cert(content));
        assert_invalid_certificate(&out);

        // The same magnitude with its mandatory positive padding is legal and
        // must keep displaying: rejection is about the encoding, never the
        // byte pattern after the sign bit.
        let mut padded = Vec::with_capacity(content.len() + 1);
        padded.push(0x00);
        padded.extend_from_slice(content);
        // Only well-defined positives use this cross-check: content[0] must
        // have its high bit set (true for every case above) and the resulting
        // padded form is minimal by construction.
        let (ok, _cert) = run_inspect(&format!("negative-{i}-padded"), &cert(&padded));
        let expected = padded[1..]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<String>();
        assert_serial_success(&ok, &expected);
    }
}

#[test]
fn unnecessary_leading_zero_bytes_are_rejected() {
    // DER INTEGER contents are minimal: a leading 00 is legal ONLY when the
    // next byte has its high bit set. `00 80` is therefore valid (covered
    // above), while each of these must be rejected — the parser may not strip
    // the redundant zero(s) and then accept the value.
    let redundant: &[&[u8]] = &[
        &[0x00, 0x7F],       // +127 needs no padding
        &[0x00, 0x00],       // zero with an extra zero
        &[0x00, 0x01],       // +1 with an extra zero
        &[0x00, 0x00, 0x80], // two leading zeros before a high-bit byte
        &[0x00, 0x00, 0xFF], // two leading zeros before 0xFF
        &[0x00, 0x7F, 0xFF], // padding present despite clear top bit
    ];
    for (i, content) in redundant.iter().enumerate() {
        let (out, _cert) = run_inspect(&format!("redundant-zero-{i}"), &cert(content));
        assert_invalid_certificate(&out);
    }
}

// ----- Assertions / process plumbing --------------------------------------

/// A successful inspect must show all five fields in the fixed order with the
/// fixed names and times — serial encodings never influence them — exit 0 and
/// leave stderr empty.
fn assert_serial_success(out: &Output, serial_hex: &str) {
    assert_serial_success_at(out, serial_hex, FIXED_NOT_BEFORE, FIXED_NOT_AFTER);
}

/// Same as `assert_serial_success`, for certificates whose validity window is
/// deliberately not the fixed one (expired / not-yet-valid cases).
fn assert_serial_success_at(out: &Output, serial_hex: &str, not_before: &str, not_after: &str) {
    assert_eq!(out.status.code(), Some(0), "stderr={}", lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "stderr={}", lossy(&out.stderr));
    assert_eq!(
        lossy(&out.stdout),
        format!(
            "Subject: {FIXED_SUBJECT}\n\
             Issuer: {FIXED_ISSUER}\n\
             Serial Number: {serial_hex}\n\
             Not Before: {not_before}\n\
             Not After: {not_after}\n"
        )
    );
}

/// Every malformed-serial case shares the documented contract: exit code 2,
/// nothing on stdout (the perfectly readable name/validity must not leak out
/// first), and the fixed invalid-certificate prefix on stderr carrying a
/// non-empty reason whose exact wording is not pinned.
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
        stderr["chainview: invalid DER certificate: ".len()..]
            .trim()
            .len()
            > 1,
        "missing error reason in stderr: {stderr}"
    );
}

fn serial_hex_field(stdout: &str) -> &str {
    let line = stdout
        .lines()
        .find(|line| line.starts_with("Serial Number: "))
        .expect("Serial Number line present");
    &line["Serial Number: ".len()..]
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
// Same minimal-DER constructor as the other suites; the only varying input is
// the raw serialNumber INTEGER content.

/// Certificate with the fixed names/times around a caller-chosen serial.
fn cert(serial_content: &[u8]) -> Vec<u8> {
    build_cert(
        serial_content,
        &tlv(TAG_UTC_TIME, b"300101000000Z"),
        &tlv(TAG_UTC_TIME, b"310101000000Z"),
    )
}

fn build_cert(serial_content: &[u8], not_before: &[u8], not_after: &[u8]) -> Vec<u8> {
    let serial = tlv(TAG_INTEGER, serial_content);
    let validity = seq(&concat(&[not_before, not_after]));
    let subject = simple_cn_name("example.com");
    let issuer = simple_cn_name("Test CA");
    let spki = seq(&concat(&[
        &seq(&concat(&[
            &oid(&[1, 2, 840, 113549, 1, 1, 1]),
            &tlv(0x05, &[]),
        ])),
        &tlv(0x03, &[0x00, 0x01, 0x00]), // BIT STRING, zero unused bits
    ]));

    let tbs = seq(&concat(&[
        &serial,
        &signature_algorithm(),
        &issuer,
        &validity,
        &subject,
        &spki,
    ]));
    seq(&concat(&[
        &tbs,
        &signature_algorithm(),
        &tlv(0x03, &[0x00]),
    ]))
}

fn signature_algorithm() -> Vec<u8> {
    // sha256WithRSAEncryption with explicit NULL parameters.
    seq(&concat(&[
        &oid(&[1, 2, 840, 113549, 1, 1, 11]),
        &tlv(0x05, &[]),
    ]))
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
