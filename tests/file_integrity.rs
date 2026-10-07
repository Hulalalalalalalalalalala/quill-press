//! End-to-end regression tests for the whole-file contract of
//! `chainview inspect <DER-FILE>`: every byte of the file must be exactly one
//! complete, validly encoded DER certificate.
//!
//! The field decoders have their own regression files; this one guards the
//! boundary around the certificate as a whole:
//!
//! - a complete, legal certificate still inspects successfully (exit 0, empty
//!   stderr, the five fixed fields in order under the existing display
//!   rules), including a certificate whose outer or inner lengths use the DER
//!   long form and one whose interior legitimately contains zero bytes or
//!   newline bytes (e.g. inside the serial or a UTF8String value);
//! - bytes appended after an otherwise viewable certificate reject the whole
//!   input, whether the append is one zero byte, whitespace, a newline or a
//!   second complete certificate: inspect neither trims whitespace nor shows
//!   only the first certificate nor merges two concatenated certificates (two
//!   identical certificates back to back are still two certificates);
//! - the opposite boundary holds too: while the outermost declared length is
//!   not satisfied the input is truncated, not partially displayed — no field
//!   is printed just because the subject, serial or validity already happened
//!   to be readable; an input that ends inside the long-form length bytes
//!   fails the same way;
//! - these guarantees concern input completeness only: expired or not-yet-
//!   valid certificates remain viewable, and inspect still never verifies
//!   signatures or trust.
//!
//! Every case drives the real binary. Success is exit 0 with empty stderr and
//! exactly the five fixed output lines; failure is exit 2, completely empty
//! stdout and a `chainview: invalid DER certificate: <reason>` stderr whose
//! reason says explicitly that bytes follow the certificate (trailing) or that
//! the data ends before the certificate does (truncated).

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXED_SERIAL: &str = "0E8A4C2F9B17D603";
const FIXED_SUBJECT: &str = "CN=example.com";
const FIXED_ISSUER: &str = "CN=Test CA";
const FIXED_NOT_BEFORE: &str = "2026-01-15T09:30:00Z";
const FIXED_NOT_AFTER: &str = "2027-01-15T09:30:00Z";

const UNKNOWN_OID: &[u64] = &[1, 2, 3, 4];

const CERT_PREFIX: &str = "chainview: invalid DER certificate: ";

// ----- Success: one complete certificate ------------------------------------

#[test]
fn complete_certificate_file_shows_the_five_fields_in_order() {
    // The baseline contract: a whole, legal file prints exactly Subject,
    // Issuer, Serial Number, Not Before, Not After — in that order — exits 0
    // and writes nothing to stderr.
    let cert = build_cert();
    let out = run_inspect("complete", &cert);
    assert_success(&out);
}

#[test]
fn certificate_using_long_form_lengths_is_accepted() {
    // The ordinary fixed fixture already exceeds 127 bytes of content, so its
    // outer Certificate SEQUENCE necessarily uses a long-form length; that
    // must not be mistaken for extra data.
    let cert = build_cert();
    assert_eq!(
        &cert[..2],
        &[0x30, 0x81],
        "the fixture must genuinely carry a long-form outer length"
    );
    assert_success(&run_inspect("long-form-outer", &cert));

    // A v3 extension with a 200-byte OCTET STRING forces an inner long-form
    // length (04 81 C8 ...) and pushes the certificate into a two-byte outer
    // length (30 82 ...): both are ordinary DER, not trailing or truncation.
    let value = vec![0xAAu8; 200];
    let ext = seq(&concat(&[&oid(UNKNOWN_OID), &tlv(0x04, &value)]));
    let tail = tlv(0xA3, &seq(&ext));
    let big = build_cert_with(Some(2), &tail, &fixed_serial(), FIXED_CN, b"260115093000Z", b"270115093000Z");
    assert_eq!(&big[..2], &[0x30, 0x82], "outer length should need two bytes");
    assert!(
        big.windows(3).any(|w| w == [0x04, 0x81, 0xC8]),
        "extension OCTET STRING should carry an inner 0x81 long-form length"
    );
    assert_success(&run_inspect("long-form-inner", &big));
}

#[test]
fn interior_zero_bytes_are_not_mistaken_for_end_of_file() {
    // Zero bytes occur inside every normal certificate (the BIT STRING
    // unused-bits count, integer padding); here the serial itself carries 0x00
    // in its middle. Such a byte is part of the certificate and must never be
    // read as the end of the file.
    let cert = build_cert_with(
        Some(2),
        &[],
        &[0x10, 0x00, 0x01],
        FIXED_CN,
        b"260115093000Z",
        b"270115093000Z",
    );
    let out = run_inspect("inner-zero", &cert);
    assert_ok_fields(&out, FIXED_SUBJECT, "100001", FIXED_NOT_BEFORE, FIXED_NOT_AFTER);
}

#[test]
fn interior_newline_byte_inside_a_value_is_not_mistaken_for_end_of_file() {
    // A UTF8String may legally contain a newline (and NUL) byte; inside the
    // file that byte is value content, shown under the existing escaping
    // rules, not a file terminator.
    let cert = build_cert_with(
        Some(2),
        &[],
        &fixed_serial(),
        b"a\nb\x00c",
        b"260115093000Z",
        b"270115093000Z",
    );
    let out = run_inspect("inner-newline", &cert);
    assert_ok_fields(
        &out,
        "CN=a\\0Ab\\00c",
        FIXED_SERIAL,
        FIXED_NOT_BEFORE,
        FIXED_NOT_AFTER,
    );
}

#[test]
fn expired_and_not_yet_valid_certificates_are_still_displayed() {
    // Input completeness is independent of validity dates: an already expired
    // certificate and one not yet valid both remain whole, legal files.
    let expired = build_cert_times(b"200115093000Z", b"210115093000Z");
    assert_ok_fields(
        &run_inspect("expired", &expired),
        FIXED_SUBJECT,
        FIXED_SERIAL,
        "2020-01-15T09:30:00Z",
        "2021-01-15T09:30:00Z",
    );

    let future = build_cert_times(b"280115093000Z", b"290115093000Z");
    assert_ok_fields(
        &run_inspect("future", &future),
        FIXED_SUBJECT,
        FIXED_SERIAL,
        "2028-01-15T09:30:00Z",
        "2029-01-15T09:30:00Z",
    );
}

// ----- Failure: bytes after the certificate ---------------------------------

#[test]
fn appended_zero_byte_is_rejected_as_trailing_data() {
    // The same 0x00 value that is legal in the middle of a certificate (see
    // interior_zero_bytes_are_not_mistaken_for_end_of_file) rejects the whole
    // file once it stands beyond the complete certificate.
    let mut data = build_cert();
    data.push(0x00);
    assert_trailing(&run_inspect("trailing-zero", &data), 1);
}

#[test]
fn appended_whitespace_or_newline_is_rejected_without_trimming() {
    // inspect must not strip whitespace the way a text reader would: a space,
    // a line feed or a CRLF after the certificate are all surplus bytes.
    for (tag, extra) in [
        ("space", vec![b' ']),
        ("lf", vec![b'\n']),
        ("cr", vec![b'\r']),
        ("crlf", vec![b'\r', b'\n']),
        ("nul-space-lf", vec![0x00, b' ', b'\n']),
    ] {
        let mut data = build_cert();
        data.extend_from_slice(&extra);
        assert_trailing(&run_inspect(&format!("trailing-{tag}"), &data), extra.len());
    }
}

#[test]
fn appended_second_complete_certificate_is_rejected() {
    // A full, independently valid certificate behind the first one is still
    // "data after the certificate", not a certificate collection the tool may
    // merge into the first read.
    let first = build_cert();
    let second = build_cert_with(
        Some(2),
        &[],
        &[0x42],
        b"second.example",
        b"260115093000Z",
        b"270115093000Z",
    );
    let mut data = first.clone();
    data.extend_from_slice(&second);
    assert_trailing(&run_inspect("trailing-cert", &data), second.len());
}

#[test]
fn two_identical_certificates_concatenated_remain_two_certificates() {
    // Byte-for-byte identical content does not make the file one certificate:
    // the second copy is surplus bytes and must not be deduplicated, merged
    // away or silently shown as the first certificate.
    let cert = build_cert();
    let duplicated_len = cert.len();
    let mut data = cert.clone();
    data.extend_from_slice(&cert);
    assert_trailing(&run_inspect("trailing-duplicate", &data), duplicated_len);
}

#[test]
fn appended_garbage_fragment_is_rejected() {
    // Even a few non-whitespace bytes that cannot start a second certificate
    // are rejected the same way; nothing is guessed or skipped.
    let mut data = build_cert();
    data.extend_from_slice(&[0x02, 0x01, 0x01]);
    assert_trailing(&run_inspect("trailing-fragment", &data), 3);
}

// ----- Failure: truncated files ---------------------------------------------

#[test]
fn file_shorter_than_its_outer_length_is_truncated() {
    // Physically cutting bytes off a good certificate leaves the outermost
    // declared length unsatisfied: the certificate is incomplete, and no
    // field is printed even though subject/serial/validity were already
    // readable. Truncation must not be reported as trailing data.
    for (tag, cut) in [("one", 1usize), ("five", 5), ("twenty", 20)] {
        let mut data = build_cert();
        data.truncate(data.len() - cut);
        assert_truncated(
            &run_inspect(&format!("truncated-{tag}"), &data),
            "Certificate SEQUENCE: truncated TLV content",
        );
    }
}

#[test]
fn outer_length_promising_more_bytes_than_present_is_truncated() {
    // The outer header honestly encodes 300 content bytes, but far fewer
    // follow: an incomplete certificate, with nothing displayable.
    let data = raw_tlv(0x30, &[0x82, 0x01, 0x2C], &build_cert()[..40]);
    assert_truncated(
        &run_inspect("truncated-declared-length", &data),
        "Certificate SEQUENCE: truncated TLV content",
    );
}

#[test]
fn file_ending_right_after_the_tag_is_truncated() {
    // One tag byte with no length byte at all.
    assert_truncated(
        &run_inspect("truncated-tag-only", &[0x30]),
        "Certificate SEQUENCE: truncated length",
    );
}

#[test]
fn file_ending_inside_the_long_form_length_bytes_is_truncated() {
    // 0x82 announces two more length bytes; the file ending before either (or
    // after only one) is the existing truncation failure, never a guess at
    // the length and never success.
    for (tag, data) in [
        ("announce-only", vec![0x30, 0x82]),
        ("one-of-two", vec![0x30, 0x82, 0x01]),
        ("announce-one-byte-form", vec![0x30, 0x81]),
    ] {
        assert_truncated(
            &run_inspect(&format!("truncated-long-len-{tag}"), &data),
            "Certificate SEQUENCE: truncated long length",
        );
    }
}

#[test]
fn empty_file_is_incomplete_not_trailing() {
    // No bytes at all: the data ends before the certificate begins.
    assert_truncated(
        &run_inspect("truncated-empty", &[]),
        "Certificate SEQUENCE: unexpected end of data",
    );
}

#[test]
fn truncation_never_prints_the_fields_that_were_already_readable() {
    // Cut inside the signature BIT STRING: subject, serial and both validity
    // times are all present and decodable, yet stdout must stay completely
    // empty and the reason must be incompleteness, not trailing data.
    let mut data = build_cert();
    data.truncate(data.len() - 3);
    let out = run_inspect("truncated-no-partial-output", &data);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.is_empty(),
        "no partial fields may be printed, got: {}",
        lossy(&out.stdout)
    );
    let reason = reason_of(&out);
    assert!(
        reason.contains("truncated"),
        "reason must describe incomplete data: {reason}"
    );
    assert!(
        !reason.contains("trailing"),
        "truncation must not be reported as trailing data: {reason}"
    );
}

// ----- Assertions / process plumbing ---------------------------------------

fn assert_success(out: &Output) {
    assert_ok_fields(
        out,
        FIXED_SUBJECT,
        FIXED_SERIAL,
        FIXED_NOT_BEFORE,
        FIXED_NOT_AFTER,
    );
}

fn assert_ok_fields(
    out: &Output,
    subject: &str,
    serial: &str,
    not_before: &str,
    not_after: &str,
) {
    assert_eq!(out.status.code(), Some(0), "stderr={}", lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "stderr={}", lossy(&out.stderr));
    assert_eq!(
        lossy(&out.stdout),
        format!(
            "Subject: {subject}\n\
             Issuer: {FIXED_ISSUER}\n\
             Serial Number: {serial}\n\
             Not Before: {not_before}\n\
             Not After: {not_after}\n"
        )
    );
}

/// Trailing-data failure: exit 2, empty stdout, and the reason must say
/// exactly how many bytes follow the certificate, under the established
/// invalid-DER prefix.
fn assert_trailing(out: &Output, count: usize) {
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
        stderr.starts_with(CERT_PREFIX),
        "unexpected stderr: {stderr}"
    );
    let reason = stderr.trim_end();
    assert_eq!(
        reason,
        format!("{CERT_PREFIX}{count} trailing byte(s) after certificate"),
        "reason must state explicitly that bytes follow the certificate"
    );
}

/// Truncation failure: exit 2, empty stdout, the invalid-DER prefix and the
/// exact established reason wording naming the incomplete Certificate
/// SEQUENCE — never a success or a trailing-data report.
fn assert_truncated(out: &Output, expected_reason: &str) {
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
    let reason = reason_of(out);
    assert_eq!(reason, expected_reason, "truncation reason mismatch");
    assert!(
        reason.contains("truncated") || reason.contains("end of data"),
        "reason must describe incomplete data: {reason}"
    );
    assert!(
        !reason.contains("trailing"),
        "truncation must not be reported as trailing data: {reason}"
    );
}

fn reason_of(out: &Output) -> String {
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.starts_with(CERT_PREFIX),
        "unexpected stderr: {stderr}"
    );
    stderr.trim_end()[CERT_PREFIX.len()..].to_string()
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn run_inspect(tag: &str, data: &[u8]) -> Output {
    let cert = TempCert::new(tag, data);
    Command::new(env!("CARGO_BIN_EXE_chainview"))
        .arg("inspect")
        .arg(&cert.path)
        .output()
        .expect("running chainview")
}

struct TempCert {
    path: PathBuf,
}

impl TempCert {
    fn new(tag: &str, data: &[u8]) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "chainview-integrity-test-{}-{tag}-{unique}.der",
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

// ----- Certificate / DER construction --------------------------------------

const FIXED_CN: &[u8] = b"example.com";

fn fixed_serial() -> Vec<u8> {
    vec![0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]
}

/// The standard complete v3 fixture with the fixed visible fields.
fn build_cert() -> Vec<u8> {
    build_cert_with(
        Some(2),
        &[],
        &fixed_serial(),
        FIXED_CN,
        b"260115093000Z",
        b"270115093000Z",
    )
}

fn build_cert_times(not_before: &[u8], not_after: &[u8]) -> Vec<u8> {
    build_cert_with(
        Some(2),
        &[],
        &fixed_serial(),
        FIXED_CN,
        not_before,
        not_after,
    )
}

/// Assemble one complete certificate around an explicit version (`None` = v1
/// with no version field), an arbitrary TBS tail, and caller-chosen visible
/// fields so individual encodings can be crafted byte for byte.
#[allow(clippy::too_many_arguments)]
fn build_cert_with(
    version: Option<u8>,
    tail: &[u8],
    serial_content: &[u8],
    subject_cn: &[u8],
    not_before: &[u8],
    not_after: &[u8],
) -> Vec<u8> {
    let serial = tlv(0x02, serial_content);
    let validity = seq(&concat(&[
        &tlv(0x17, not_before),
        &tlv(0x17, not_after),
    ]));
    let subject = simple_cn_name(subject_cn);
    let issuer = simple_cn_name(b"Test CA");
    let spki = spki();
    let alg = signature_algorithm();

    let mut tbs_content = Vec::new();
    if let Some(v) = version {
        tbs_content.extend_from_slice(&tlv(0xA0, &tlv(0x02, &[v])));
    }
    tbs_content.extend_from_slice(&serial);
    tbs_content.extend_from_slice(&alg);
    tbs_content.extend_from_slice(&issuer);
    tbs_content.extend_from_slice(&validity);
    tbs_content.extend_from_slice(&subject);
    tbs_content.extend_from_slice(&spki);
    tbs_content.extend_from_slice(tail);
    let tbs = seq(&tbs_content);

    seq(&concat(&[
        &tbs,
        &alg,
        &tlv(0x03, &[0x00]),
    ]))
}

fn signature_algorithm() -> Vec<u8> {
    seq(&concat(&[
        &oid(&[1, 2, 840, 113549, 1, 1, 11]),
        &tlv(0x05, &[]),
    ]))
}

fn spki() -> Vec<u8> {
    let alg = seq(&concat(&[
        &oid(&[1, 2, 840, 113549, 1, 1, 1]),
        &tlv(0x05, &[]),
    ]));
    seq(&concat(&[
        &alg,
        &tlv(0x03, &[0x00, 0x01, 0x00]),
    ]))
}

fn simple_cn_name(cn: &[u8]) -> Vec<u8> {
    let atv = seq(&concat(&[&oid(&[2, 5, 4, 3]), &tlv(0x0C, cn)]));
    let rdn = tlv(0x31, &atv);
    tlv(0x30, &rdn)
}

/// TLV with caller-supplied length bytes; used to forge truncated encodings.
fn raw_tlv(tag: u8, len_bytes: &[u8], content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend_from_slice(len_bytes);
    out.extend_from_slice(content);
    out
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
