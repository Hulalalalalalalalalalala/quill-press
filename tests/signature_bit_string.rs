//! End-to-end regression tests for strict decoding of the outer
//! `signatureValue` BIT STRING when running `chainview inspect <DER-FILE>`.
//!
//! ```text
//! Certificate ::= SEQUENCE {
//!     tbsCertificate      TBSCertificate,
//!     signatureAlgorithm  AlgorithmIdentifier,
//!     signatureValue      BIT STRING }
//! ```
//!
//! The signature bytes are never cryptographically verified, but their BIT
//! STRING encoding must be well formed: the first content byte counts the
//! unused bits in the final data byte (0..=7); a nonzero count requires data,
//! and with a nonzero count the declared low bits of the final data byte must
//! all be zero. A count of 0 leaves every data bit significant (low bits set
//! is fine), and a bit string holding only a zero count byte keeps the
//! existing acceptance. A missing count byte, a count above 7, a wrong field
//! tag or a truncated encoding all remain failures. Encoding damage to the
//! signatureValue rejects the whole certificate even though Subject, Issuer,
//! serial and validity all remain readable; the corrupt bytes are never
//! deleted, zero-padded or rewritten.
//!
//! Every case drives the real `chainview` binary: success prints exactly the
//! five fixed fields with exit code 0 and empty stderr; failure yields exit
//! code 2, completely empty stdout and a
//! `chainview: invalid DER certificate: <reason>` stderr whose reason names
//! the signatureValue BIT STRING and distinguishes "unused bits declared but
//! no data" from "set bits among the unused trailing bits".

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXED_SERIAL: &str = "0E8A4C2F9B17D603";
const FIXED_SUBJECT: &str = "CN=example.com";
const FIXED_ISSUER: &str = "CN=Test CA";
const FIXED_NOT_BEFORE: &str = "2026-01-15T09:30:00Z";
const FIXED_NOT_AFTER: &str = "2027-01-15T09:30:00Z";

const SHA256_WITH_RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 11];
const RSA_ENCRYPTION: &[u64] = &[1, 2, 840, 113549, 1, 1, 1];

// ----- Success: well-formed signature bit strings ---------------------------

#[test]
fn signature_zero_unused_bits_is_accepted() {
    // 0x00 count over several data bytes, including a final 0xFF whose low
    // bits are all significant: signature payload content is never judged.
    let sig = bit_string(&[0x00, 0xAA, 0x55, 0xFF, 0x01]);
    let (out, _cert) = run_inspect("sig-zero", &build_cert(&sig));
    assert_success(&out);
}

#[test]
fn signature_nonzero_unused_bits_with_clear_low_bits_are_accepted() {
    // Counts 1, 3 and 7 all succeed when the corresponding low bits of the
    // final data byte are zero. High (used) bits of the final byte may hold
    // any value.
    let cases: &[&[u8]] = &[
        &[0x01, 0xFE], // 1 unused bit;  last byte low 1 bit  = 0
        &[0x03, 0xF8], // 3 unused bits; last byte low 3 bits = 000
        &[0x07, 0x80], // 7 unused bits; last byte low 7 bits = 0000000
    ];
    for (i, content) in cases.iter().enumerate() {
        let (out, _cert) = run_inspect(&format!("sig-nonzero-ok-{i}"), &build_cert(&bit_string(content)));
        assert_success(&out);
    }
}

#[test]
fn signature_nonzero_unused_bits_check_only_final_byte() {
    // With several data bytes the padding rule applies to the FINAL byte
    // only: earlier bytes may have any low bits set, including the exact
    // pattern that is forbidden in the trailing byte.
    let sig = bit_string(&[0x03, 0x01, 0xF9, 0x12, 0xF8]);
    let (out, _cert) = run_inspect("sig-penultimate", &build_cert(&sig));
    assert_success(&out);
}

#[test]
fn signature_empty_bit_string_with_zero_count_is_accepted() {
    // Existing behavior, pinned deliberately: a BIT STRING holding only the
    // zero unused-bits count (no signature data at all) stays accepted.
    let sig = bit_string(&[0x00]);
    let (out, _cert) = run_inspect("sig-empty", &build_cert(&sig));
    assert_success(&out);
}

#[test]
fn signature_with_unknown_algorithm_still_only_checks_encoding() {
    // The encoding check never turns into cryptographic verification: an
    // unknown signature algorithm OID with a well-formed signature is shown.
    let alg = alg_with_null_params(&[1, 2, 840, 99999, 7]);
    let sig = bit_string(&[0x03, 0xF8]);
    let (out, _cert) = run_inspect("sig-unknown-alg", &build_cert_parts(&alg, &sig));
    assert_success(&out);
}

// ----- Failure: nonzero count without data ----------------------------------

#[test]
fn signature_nonzero_count_without_data_is_rejected() {
    // Declaring unused bits requires at least the byte they are unused in:
    // a lone nonzero count names bits that cannot exist.
    for (i, count) in [0x01u8, 0x03, 0x07].into_iter().enumerate() {
        let (out, _cert) = run_inspect(
            &format!("sig-nodata-{i}"),
            &build_cert(&bit_string(&[count])),
        );
        assert_unused_bits_without_data(&out);
    }
}

// ----- Failure: set bits among the declared unused trailing bits -------------

#[test]
fn signature_with_set_unused_tail_bits_is_rejected() {
    // With a nonzero count the named low bits of the final data byte must be
    // zero. Each case below has at least one of them set, so the encoding
    // lies about its padding even though the rest of the certificate parses.
    let cases: &[&[u8]] = &[
        &[0x01, 0x01], // 1 unused bit;  low 1 bit set
        &[0x02, 0x03], // 2 unused bits; both low bits set
        &[0x03, 0xF9], // 3 unused bits; lowest of them set (...001); F8 is fine
        &[0x07, 0x81], // 7 unused bits; lowest bit set
        &[0x03, 0x12, 0x34, 0x01], // only the FINAL byte is judged, and it fails
    ];
    for (i, content) in cases.iter().enumerate() {
        let (out, _cert) = run_inspect(
            &format!("sig-tailset-{i}"),
            &build_cert(&bit_string(content)),
        );
        assert_unused_tail_bits_set(&out);
    }
}

// ----- Failure: other signatureValue encoding damage ------------------------

#[test]
fn signature_bit_string_without_count_byte_is_rejected() {
    // A zero-length BIT STRING content has no leading unused-bits count at
    // all (contrast with [0x00], the empty-but-counted form that is accepted).
    let (out, _cert) = run_inspect("sig-no-count", &build_cert(&bit_string(&[])));
    assert_invalid_signature(&out);
}

#[test]
fn signature_bit_string_with_count_above_seven_is_rejected() {
    // The count is three bits wide; 8 cannot name unused bits in a byte.
    let (out, _cert) = run_inspect("sig-count-8", &build_cert(&bit_string(&[0x08, 0x00])));
    assert_invalid_signature(&out);
}

#[test]
fn signature_field_with_wrong_tag_is_rejected() {
    // An OCTET STRING where the signatureValue BIT STRING must stand is a
    // type error, not a signature that merely cannot be interpreted.
    let wrong = tlv(0x04, &[0x00, 0x01, 0x00]);
    let (out, _cert) = run_inspect("sig-tag", &build_cert(&wrong));
    assert_invalid_signature(&out);
}

#[test]
fn signature_bit_string_with_truncated_tlv_is_rejected() {
    // The BIT STRING header announces four content bytes but only three
    // follow inside the correctly sized Certificate SEQUENCE: the signature
    // cannot be salvaged by ignoring its tail.
    let truncated = raw_tlv(0x03, &[0x04], &[0x00, 0xAA, 0xBB]);
    let (out, _cert) = run_inspect("sig-trunc", &build_cert(&truncated));
    assert_invalid_signature(&out);
}

// ----- Assertions / process plumbing --------------------------------------

fn assert_success(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "stderr={}", lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "stderr={}", lossy(&out.stderr));
    assert_eq!(
        lossy(&out.stdout),
        format!(
            "Subject: {FIXED_SUBJECT}\n\
             Issuer: {FIXED_ISSUER}\n\
             Serial Number: {FIXED_SERIAL}\n\
             Not Before: {FIXED_NOT_BEFORE}\n\
             Not After: {FIXED_NOT_AFTER}\n"
        )
    );
}

/// The malformed-input contract: exit code 2, nothing on stdout, fixed
/// invalid-certificate prefix on stderr with a reason identifying the
/// signature value bit string.
fn assert_invalid_signature(out: &Output) {
    let reason = invalid_reason(out);
    assert!(
        reason.contains("signatureValue"),
        "reason {reason:?} does not identify the signatureValue BIT STRING"
    );
}

/// A nonzero unused-bits count with no data byte behind it must be reported
/// distinctly from a set unused trailing bit.
fn assert_unused_bits_without_data(out: &Output) {
    let reason = invalid_reason(out);
    assert!(
        reason.contains("signatureValue"),
        "reason {reason:?} does not identify the signatureValue BIT STRING"
    );
    assert!(
        reason.contains("no data"),
        "reason {reason:?} does not state that unused bits are declared without data"
    );
}

/// A set bit among the declared unused trailing bits of the final data byte
/// must be reported distinctly from a nonzero count without data.
fn assert_unused_tail_bits_set(out: &Output) {
    let reason = invalid_reason(out);
    assert!(
        reason.contains("signatureValue"),
        "reason {reason:?} does not identify the signatureValue BIT STRING"
    );
    assert!(
        reason.contains("unused trailing bits"),
        "reason {reason:?} does not state that unused trailing bits are set"
    );
}

fn invalid_reason(out: &Output) -> String {
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
    let prefix = "chainview: invalid DER certificate: ";
    assert!(
        stderr.starts_with(prefix),
        "unexpected stderr: {stderr}"
    );
    stderr[prefix.len()..].to_string()
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
            "chainview-sigvalue-test-{}-{tag}-{unique}.der",
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
// The outer signatureValue TLV is supplied already encoded (or deliberately
// forged, including truncated encodings) while every enclosing wrapper keeps a
// correct length, so rejection is proven to come from the signature value.

fn build_cert(signature: &[u8]) -> Vec<u8> {
    build_cert_parts(&signature_algorithm(), signature)
}

fn build_cert_parts(alg: &[u8], signature: &[u8]) -> Vec<u8> {
    let serial = tlv(0x02, &[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]);
    let validity = seq(&concat(&[
        &tlv(0x17, b"260115093000Z"),
        &tlv(0x17, b"270115093000Z"),
    ]));
    let subject = simple_cn_name("example.com");
    let issuer = simple_cn_name("Test CA");
    let spki_alg = alg_with_null_params(RSA_ENCRYPTION);
    let spki = seq(&concat(&[&spki_alg, &bit_string(&[0x00, 0x01, 0x00])]));

    let tbs = seq(&concat(&[&serial, alg, &issuer, &validity, &subject, &spki]));
    seq(&concat(&[&tbs, alg, signature]))
}

fn signature_algorithm() -> Vec<u8> {
    alg_with_null_params(SHA256_WITH_RSA)
}

fn alg_with_null_params(arcs: &[u64]) -> Vec<u8> {
    seq(&concat(&[&oid(arcs), &tlv(0x05, &[])]))
}

fn bit_string(content: &[u8]) -> Vec<u8> {
    tlv(0x03, content)
}

fn simple_cn_name(cn: &str) -> Vec<u8> {
    let atv = seq(&concat(&[&oid(&[2, 5, 4, 3]), &tlv(0x0C, cn.as_bytes())]));
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
