//! End-to-end regression tests for the outer `signatureValue` BIT STRING
//! decoding when running `chainview inspect <DER-FILE>`.
//!
//! ```text
//! Certificate ::= SEQUENCE {
//!     tbsCertificate      TBSCertificate,
//!     signatureAlgorithm  AlgorithmIdentifier,
//!     signatureValue      BIT STRING }
//! ```
//!
//! The signature itself is never cryptographically verified, but its BIT
//! STRING must still be strict DER: one leading byte counting the unused bits
//! in the final data byte (0..=7), data present whenever the count is
//! nonzero, and all declared unused low bits of the final data byte zero. A
//! lone zero count with no data keeps being accepted, and a zero count makes
//! every data bit significant (a final byte of 0xFF is fine). Damage to the
//! signatureValue encoding must reject the whole certificate — exit code 2,
//! empty stdout, a `chainview: invalid DER certificate:` reason that names
//! the signatureValue BIT STRING and distinguishes "declares unused bits but
//! has no data" from "set bits among the unused trailing bits" — even though
//! Subject, Issuer, serial and validity all remain readable.
//!
//! Every case drives the real `chainview` binary. Certificates are built with
//! the tiny zero-dependency DER constructor at the bottom of this file, so
//! individual signature encodings (including truncated ones) can be crafted
//! byte for byte while every enclosing wrapper keeps a correct length.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXED_SERIAL: &str = "0E8A4C2F9B17D603";
const FIXED_SUBJECT: &str = "CN=example.com";
const FIXED_ISSUER: &str = "CN=Test CA";
const FIXED_NOT_BEFORE: &str = "2026-01-15T09:30:00Z";
const FIXED_NOT_AFTER: &str = "2027-01-15T09:30:00Z";

// ----- Success: unused-bits encoding ----------------------------------------

#[test]
fn signature_with_zero_unused_bits_and_data_is_accepted() {
    // Zero unused bits over several data bytes, including a final 0xFF byte
    // whose low bits are all significant: with count 0 every data bit means
    // something, so low ones are accepted and the payload is not interpreted.
    let (out, _cert) =
        run_inspect("sig-bits-zero", &build_cert(&bit_string(&[0x00, 0xAA, 0x55, 0xFF])));
    assert_success(&out);
}

#[test]
fn signature_with_nonzero_unused_bits_and_clear_low_bits_is_accepted() {
    // Counts 1, 3 and 7 all succeed as long as the declared low bits of the
    // final data byte are zero (3 unused bits: F8 is fine, F9 is not — the F9
    // case lives among the rejection tests).
    let cases: &[&[u8]] = &[
        &[0x01, 0x02], // 1 unused bit;  last byte low 1 bit  = 0
        &[0x03, 0xF8], // 3 unused bits; last byte low 3 bits = 000
        &[0x07, 0x80], // 7 unused bits; last byte low 7 bits = 0000000
    ];
    for (i, content) in cases.iter().enumerate() {
        let (out, _cert) =
            run_inspect(&format!("sig-bits-nonzero-{i}"), &build_cert(&bit_string(content)));
        assert_success(&out);
    }
}

#[test]
fn signature_padding_rule_only_touches_the_final_data_byte() {
    // With multiple data bytes the padding requirement acts on the final
    // byte alone: earlier bytes are unconstrained and the used high bits of
    // the final byte are unconstrained too, so neither may cause rejection.
    let cases: &[&[u8]] = &[
        &[0x03, 0xFF, 0xAB, 0xF8], // prior bytes arbitrary; last low 3 zero
        &[0x03, 0x00, 0x00, 0x38], // prior bytes arbitrary; last low 3 zero
        &[0x01, 0x7E, 0xFE],       // prior byte arbitrary; last low 1 zero
    ];
    for (i, content) in cases.iter().enumerate() {
        let (out, _cert) =
            run_inspect(&format!("sig-bits-multi-{i}"), &build_cert(&bit_string(content)));
        assert_success(&out);
    }
}

#[test]
fn signature_empty_bit_string_with_zero_count_is_accepted() {
    // Existing behavior, pinned deliberately: a signature BIT STRING holding
    // only the zero unused-bits count (no data at all) stays accepted. The
    // check judges encoding well-formedness, not whether a real signature
    // exists or verifies.
    let (out, _cert) = run_inspect("sig-bits-empty", &build_cert(&bit_string(&[0x00])));
    assert_success(&out);
}

// ----- Failure: signatureValue BIT STRING contents --------------------------

#[test]
fn signature_bit_string_without_unused_bits_byte_is_rejected() {
    // A zero-length BIT STRING content has no leading unused-bits count at
    // all (contrast with [0x00], the empty-but-counted form that is accepted).
    let (out, _cert) = run_inspect("sig-bits-no-count", &build_cert(&bit_string(&[])));
    assert_invalid_signature(&out);
}

#[test]
fn signature_bit_string_with_unused_count_above_seven_is_rejected() {
    // The count names bits within one byte; 8 cannot be unused bits.
    let (out, _cert) =
        run_inspect("sig-bits-count-8", &build_cert(&bit_string(&[0x08, 0x00])));
    assert_invalid_signature(&out);
}

#[test]
fn signature_bit_string_nonzero_count_without_data_is_rejected() {
    // Declaring unused bits requires the data byte those bits are unused in:
    // a lone nonzero count names bits that cannot exist. This reason must be
    // distinguishable from the "trailing unused bits are set" case.
    for (i, count) in [0x01u8, 0x03, 0x07].into_iter().enumerate() {
        let (out, _cert) = run_inspect(
            &format!("sig-bits-nodata-{i}"),
            &build_cert(&bit_string(&[count])),
        );
        assert_invalid_signature(&out);
        let stderr = lossy(&out.stderr);
        assert!(
            stderr.contains("declares") && stderr.contains("unused bits") && stderr.contains("no data"),
            "reason must name a nonzero count without data, got: {stderr}"
        );
        assert!(
            !stderr.contains("trailing bits"),
            "no-data case must not be reported as trailing-bit damage: {stderr}"
        );
    }
}

#[test]
fn signature_bit_string_with_set_unused_tail_bits_is_rejected() {
    // With a nonzero count the named low bits of the final data byte must be
    // zero. Each case below has at least one of them set, so the encoding
    // lies about its padding even though the rest of the certificate parses.
    // The reason must be distinguishable from "declares unused bits but has
    // no data".
    let cases: &[&[u8]] = &[
        &[0x01, 0x01], // 1 unused bit;  low 1 bit set
        &[0x02, 0x03], // 2 unused bits; both low bits set
        &[0x03, 0xF9], // 3 unused bits; lowest of them set (...001)
        &[0x07, 0x81], // 7 unused bits; lowest bit set
        &[0x03, 0x00, 0x01], // padding is judged on the FINAL byte only,
                             // and that final byte still has a set low bit
    ];
    for (i, content) in cases.iter().enumerate() {
        let (out, _cert) = run_inspect(
            &format!("sig-bits-tailset-{i}"),
            &build_cert(&bit_string(content)),
        );
        assert_invalid_signature(&out);
        let stderr = lossy(&out.stderr);
        assert!(
            stderr.contains("unused") && stderr.contains("trailing bits"),
            "reason must name set unused trailing bits, got: {stderr}"
        );
        assert!(
            !stderr.contains("no data"),
            "tail-bits case must not be reported as missing data: {stderr}"
        );
    }
}

#[test]
fn signature_value_with_wrong_tag_is_rejected() {
    // An OCTET STRING where the signatureValue BIT STRING must stand is a
    // type error, not a signature that merely cannot be verified.
    let wrong = tlv(0x04, &[0x00, 0x01, 0x00]);
    let (out, _cert) = run_inspect("sig-tag", &build_cert(&wrong));
    assert_invalid_certificate(&out);
}

#[test]
fn signature_value_with_truncated_encoding_is_rejected() {
    // The BIT STRING header announces ten content bytes but only two follow:
    // a half-read signature cannot be salvaged by ignoring its tail or
    // padding it with zeroes.
    let truncated = raw_tlv(0x03, &[0x0A], &[0x00, 0x01]);
    let (out, _cert) = run_inspect("sig-truncated", &build_cert(&truncated));
    assert_invalid_certificate(&out);
}

// ----- Assertions / process plumbing --------------------------------------

/// A successful inspect must print exactly the five fixed fields in their
/// fixed order; none of their content varies with the signature encoding, and
/// stderr stays empty.
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

/// The malformed-input contract: exit code 2, nothing on stdout, and the
/// fixed invalid-certificate prefix on stderr with a non-empty reason.
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

/// A signatureValue BIT STRING failure must additionally point at the
/// signature value, so it can never be confused with damage to some other
/// field.
fn assert_invalid_signature(out: &Output) {
    assert_invalid_certificate(out);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.contains("signatureValue"),
        "reason must identify the signatureValue BIT STRING, got: {stderr}"
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
            "chainview-sigval-test-{}-{tag}-{unique}.der",
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
// minimal DER form, except where a test injects hand-written length bytes (a
// truncated signature TLV); such bytes are always wrapped by correctly sized
// SEQUENCEs so rejection is proven to come from the signatureValue.

/// Assemble a complete, parseable v1 DER certificate with fixed names,
/// serial, validity and public-key material around the supplied
/// signatureValue element (normally a BIT STRING TLV).
fn build_cert(signature_value: &[u8]) -> Vec<u8> {
    let serial = tlv(0x02, &[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]);
    let validity = seq(&concat(&[
        &tlv(0x17, b"260115093000Z"), // UTCTime 2026-01-15T09:30:00Z
        &tlv(0x17, b"270115093000Z"), // UTCTime 2027-01-15T09:30:00Z
    ]));
    let subject = simple_cn_name("example.com");
    let issuer = simple_cn_name("Test CA");

    let tbs = seq(&concat(&[
        &serial,
        &signature_algorithm(),
        &issuer,
        &validity,
        &subject,
        &spki(),
    ]));
    seq(&concat(&[&tbs, &signature_algorithm(), signature_value]))
}

fn signature_algorithm() -> Vec<u8> {
    // sha256WithRSAEncryption with explicit NULL parameters.
    seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 11]), &tlv(0x05, &[])]))
}

fn spki() -> Vec<u8> {
    // rsaEncryption AlgorithmIdentifier plus a small well-formed key BIT
    // STRING (zero unused bits, two data bytes).
    seq(&concat(&[
        &seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 1]), &tlv(0x05, &[])])),
        &tlv(0x03, &[0x00, 0x01, 0x00]),
    ]))
}

/// BIT STRING carrying the supplied content (unused-bits count plus data).
fn bit_string(content: &[u8]) -> Vec<u8> {
    tlv(0x03, content)
}

fn simple_cn_name(cn: &str) -> Vec<u8> {
    let atv = seq(&concat(&[&oid(&[2, 5, 4, 3]), &tlv(0x0C, cn.as_bytes())]));
    let rdn = tlv(0x31, &atv); // SET { CN=... }
    tlv(0x30, &rdn) // SEQUENCE { RDN }
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
