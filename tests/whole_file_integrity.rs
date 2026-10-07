//! End-to-end regression tests for the whole-file integrity rule documented
//! in README.md: the file handed to `chainview inspect` must contain exactly
//! one complete DER certificate — every byte of it, and nothing more.
//!
//! Like `tests/validity_display.rs`, these drive the real `chainview` binary
//! so the public contract is locked as a whole: on success the five fields
//! keep their fixed order with stderr empty; on failure the exit code is 2,
//! stdout stays completely empty and stderr carries the
//! `chainview: invalid DER certificate:` prefix with a reason. Only input
//! completeness is exercised here — field decoding has its own test files,
//! and signature verification, trust and expiry stay out of scope exactly as
//! before: `inspect` displays fields, it never judges them.
//!
//! Coverage:
//! - a complete, correctly encoded certificate displays all five fields;
//! - legal DER long-form lengths (0x81, 0x82 and inner long forms) are
//!   accepted, never mistaken for trailing data;
//! - zero bytes and newline bytes *inside* the certificate are data, not
//!   end-of-file markers;
//! - any byte after the certificate — a zero byte, a space, a newline, or a
//!   whole second certificate (even an identical one) — rejects the input
//!   with a "trailing byte(s) after certificate" reason; nothing is trimmed,
//!   only the first certificate is never shown, two certificates are never
//!   merged;
//! - a file that ends before the outer SEQUENCE's declared length is
//!   truncated input: exit 2, empty stdout (no partial fields), reason
//!   naming incomplete data — never success and never "trailing data";
//! - a length encoding whose long-form length bytes are themselves cut off
//!   is rejected the same way.
//!
//! Certificates are built with the tiny zero-dependency DER constructor
//! copied from the other test files so file boundaries can be crafted byte
//! for byte.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXED_SERIAL: &str = "0E8A4C2F9B17D603";
const FIXED_SUBJECT: &str = "CN=example.com";
const FIXED_ISSUER: &str = "CN=Test CA";

const CN: &[u64] = &[2, 5, 4, 3];

const STDERR_PREFIX: &str = "chainview: invalid DER certificate: ";

// ----- Success: exactly one complete certificate ----------------------------

#[test]
fn complete_certificate_displays_five_fields_in_order() {
    // The baseline the integrity rule must never break: a file holding one
    // complete, correctly encoded certificate succeeds with exit code 0,
    // empty stderr and the five documented lines in their fixed order.
    let cert = default_cert();
    let (out, _cert) = run_inspect("complete", &cert);
    assert_success(&out, &expected_output(FIXED_SUBJECT));
}

#[test]
fn long_form_lengths_are_accepted_as_part_of_the_certificate() {
    // The default certificate's outer SEQUENCE already needs a 0x81 long-form
    // length (its content exceeds 127 bytes); those length bytes are part of
    // the certificate, never "extra data" after it.
    let cert = default_cert();
    assert_eq!(cert[0], 0x30, "test premise: outer SEQUENCE");
    assert_eq!(cert[1], 0x81, "test premise: one-byte long-form length");
    let (out, _cert) = run_inspect("long-form-81", &cert);
    assert_success(&out, &expected_output(FIXED_SUBJECT));

    // A larger certificate pushes the outer length (and several inner ones)
    // past 255 bytes, so the two-byte long form 0x82 is exercised as well.
    let long_cn = "a".repeat(200);
    let cert = build_cert(
        &simple_cn_name(&long_cn),
        &simple_cn_name("Test CA"),
        &default_validity(),
    );
    assert_eq!(cert[1], 0x82, "test premise: two-byte long-form length");
    let (out, _cert) = run_inspect("long-form-82", &cert);
    assert_success(&out, &expected_output(&format!("CN={long_cn}")));
}

#[test]
fn interior_zero_and_newline_bytes_are_data_not_end_of_file() {
    // Every certificate already carries interior zero bytes (the signature
    // BIT STRING's unused-bits count byte is 0x00). This certificate also
    // carries a 0x0A byte inside the subject CN. Both are ordinary content:
    // the reader must follow the declared lengths, not stop at the first
    // zero or newline byte, and the certificate displays in full.
    let cert = build_cert(
        &simple_cn_name("line\nbreak"),
        &simple_cn_name("Test CA"),
        &default_validity(),
    );
    assert!(
        cert[..cert.len() - 1].contains(&0x00),
        "test premise: interior zero byte"
    );
    assert!(
        cert[..cert.len() - 1].contains(&0x0A),
        "test premise: interior newline byte"
    );
    let (out, _cert) = run_inspect("interior-bytes", &cert);
    // RFC 4514 escaping renders the newline as `\0A` (uppercase hex).
    assert_success(&out, &expected_output("CN=line\\0Abreak"));

    // And because the interior bytes did not end the read, a byte appended
    // after this same certificate is still detected as trailing data.
    let mut appended = cert.clone();
    appended.push(0x00);
    let (out, _cert) = run_inspect("interior-bytes-then-extra", &appended);
    assert_trailing_rejected(&out, 1);
}

// ----- Failure: data after the certificate ----------------------------------

#[test]
fn trailing_zero_space_or_newline_byte_is_rejected() {
    // Appending anything — even a single invisible byte — turns one
    // certificate into one certificate plus extra data. The tool must not
    // trim whitespace or ignore a trailing NUL: every variant is rejected
    // identically, with stdout completely empty.
    for (i, extra) in [0x00u8, b' ', b'\n', b'\r', b'\t'].iter().enumerate() {
        let mut data = default_cert();
        data.push(*extra);
        let (out, _cert) = run_inspect(&format!("trailing-byte-{i}"), &data);
        assert_trailing_rejected(&out, 1);
    }
}

#[test]
fn trailing_whitespace_run_is_rejected() {
    // Trimming is not allowed no matter how much whitespace follows.
    let mut data = default_cert();
    data.extend_from_slice(b" \t\r\n\n\n");
    let (out, _cert) = run_inspect("trailing-whitespace-run", &data);
    assert_trailing_rejected(&out, 6);
}

#[test]
fn trailing_second_certificate_is_rejected() {
    // A second complete, valid certificate after the first is still trailing
    // data: the tool shows neither "just the first one" nor a merged view.
    let first = default_cert();
    let second = build_cert(
        &simple_cn_name("other.example.com"),
        &simple_cn_name("Test CA"),
        &default_validity(),
    );
    let mut data = first.clone();
    data.extend_from_slice(&second);
    let (out, _cert) = run_inspect("trailing-second-cert", &data);
    assert_trailing_rejected(&out, second.len());
}

#[test]
fn two_identical_certificates_are_still_two_certificates() {
    // Concatenating a certificate with itself does not collapse into one:
    // the file contains two certificates and is rejected like any other
    // trailing data, regardless of the repeated content.
    let cert = default_cert();
    let mut data = cert.clone();
    data.extend_from_slice(&cert);
    let (out, _cert) = run_inspect("identical-twice", &data);
    assert_trailing_rejected(&out, cert.len());
}

// ----- Failure: truncated input ---------------------------------------------

#[test]
fn truncated_certificate_is_rejected_without_partial_output() {
    // Cutting the file anywhere before the outer SEQUENCE's declared end
    // leaves the announced certificate length unsatisfied. However much of
    // the subject, serial number or validity was already readable, nothing
    // may be printed: exit code 2, empty stdout, and a stderr reason that
    // names incomplete data — never a success and never "trailing data".
    let cert = default_cert();
    for (i, cut) in [cert.len() - 1, cert.len() - 20, cert.len() / 2, 10, 3]
        .iter()
        .enumerate()
    {
        let (out, _cert) = run_inspect(&format!("truncated-{i}"), &cert[..*cut]);
        assert_truncated_rejected(&out);
    }
}

#[test]
fn incomplete_length_encoding_is_rejected_as_truncated() {
    // The length itself can be cut off: a lone tag byte, or a long-form
    // length announcing more length bytes than the file still holds. Both
    // are incomplete input and follow the same truncation contract.
    let cases: &[&[u8]] = &[
        &[0x30],             // tag only, length byte missing
        &[0x30, 0x82],       // long form announces two length bytes, none read
        &[0x30, 0x82, 0x01], // one of the two announced length bytes read
    ];
    for (i, data) in cases.iter().enumerate() {
        let (out, _cert) = run_inspect(&format!("truncated-length-{i}"), data);
        assert_truncated_rejected(&out);
    }

    // An empty file is the extreme of the same boundary: no certificate
    // bytes at all, reported as incomplete input, not as trailing data.
    let (out, _cert) = run_inspect("empty-file", &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "stdout must be empty on failure");
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.starts_with(STDERR_PREFIX),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("unexpected end of data"),
        "empty input must be reported as incomplete data: {stderr}"
    );
}

// ----- Assertions / process plumbing --------------------------------------

/// The five documented output lines in their fixed order for a certificate
/// built with the fixed issuer, serial and validity values.
fn expected_output(subject: &str) -> String {
    format!(
        "Subject: {subject}\n\
         Issuer: {FIXED_ISSUER}\n\
         Serial Number: {FIXED_SERIAL}\n\
         Not Before: 2026-01-15T09:30:00Z\n\
         Not After: 2027-01-15T09:30:00Z\n"
    )
}

/// A successful inspect: exit code 0, empty stderr, and exactly the
/// expected five lines on stdout.
fn assert_success(out: &Output, expected_stdout: &str) {
    assert_eq!(out.status.code(), Some(0), "stderr={}", lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "stderr={}", lossy(&out.stderr));
    assert_eq!(lossy(&out.stdout), expected_stdout);
}

/// Trailing-data rejection: exit code 2, stdout completely empty (the first
/// certificate is never shown on its own), the invalid-certificate prefix,
/// and a reason that explicitly says extra bytes follow the certificate.
fn assert_trailing_rejected(out: &Output, extra_bytes: usize) {
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
        stderr.starts_with(STDERR_PREFIX),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("trailing byte(s) after certificate"),
        "reason must name the extra bytes after the certificate: {stderr}"
    );
    assert!(
        stderr.contains(&format!("{extra_bytes} trailing byte(s)")),
        "reason must count the extra bytes: {stderr}"
    );
}

/// Truncation rejection: exit code 2, stdout completely empty (no partial
/// field output), the invalid-certificate prefix, and a reason that names
/// incomplete data rather than reporting success or trailing bytes.
fn assert_truncated_rejected(out: &Output) {
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
        stderr.starts_with(STDERR_PREFIX),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("truncated"),
        "reason must name incomplete data: {stderr}"
    );
    assert!(
        !stderr.contains("trailing"),
        "truncated input must not be reported as trailing data: {stderr}"
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
            "chainview-wholefile-test-{}-{tag}-{unique}.der",
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
// Mirrors the constructor in tests/validity_display.rs: minimal DER lengths
// everywhere, so certificates larger than 127/255 content bytes naturally
// exercise the 0x81/0x82 long forms.

fn default_cert() -> Vec<u8> {
    build_cert(
        &simple_cn_name("example.com"),
        &simple_cn_name("Test CA"),
        &default_validity(),
    )
}

/// 2026-01-15T09:30:00Z / 2027-01-15T09:30:00Z in UTCTime.
fn default_validity() -> Vec<u8> {
    seq(&concat(&[
        &tlv(0x17, b"260115093000Z"),
        &tlv(0x17, b"270115093000Z"),
    ]))
}

/// Assemble a complete, parseable v1 DER certificate with fixed serial and
/// key material around the supplied Subject/Issuer names and validity TLV.
fn build_cert(subject: &[u8], issuer: &[u8], validity: &[u8]) -> Vec<u8> {
    let serial = tlv(0x02, &[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]);
    let spki = seq(&concat(&[
        &seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 1]), &tlv(0x05, &[])])),
        &tlv(0x03, &[0x00, 0x01, 0x00]), // BIT STRING, zero unused bits
    ]));

    let tbs = seq(&concat(&[
        &serial,
        &signature_algorithm(),
        issuer,
        validity,
        subject,
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
