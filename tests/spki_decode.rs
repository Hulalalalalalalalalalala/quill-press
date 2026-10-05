//! End-to-end regression tests for SubjectPublicKeyInfo decoding during
//! `chainview inspect <DER-FILE>`, fixing the behavior already implemented in
//! `src/main.rs`.
//!
//! The public key never appears in the five displayed fields, but its DER
//! encoding is still read in full: a malformed SubjectPublicKeyInfo rejects the
//! whole certificate even though Subject/Issuer/Serial/validity are all
//! readable. These tests drive the real `chainview` binary so that contract is
//! locked as a whole — success keeps the fixed five-line output (key bytes do
//! not influence it), exit code 0 and empty stderr; malformed input means exit
//! code 2, completely empty stdout and the
//! `chainview: invalid DER certificate: ` stderr prefix with a non-empty
//! reason. Signature verification, trust and whether the key bytes form a
//! usable public key are intentionally out of scope: only the encoding is
//! checked.
//!
//! Coverage:
//! - AlgorithmIdentifier parameters omitted, an explicit NULL, or any other
//!   single complete parameter TLV; an algorithm OID the tool does not know is
//!   accepted as long as it decodes;
//! - subjectPublicKey BIT STRING with zero or nonzero unused-bits counts
//!   (nonzero counts require the corresponding low bits of the final data byte
//!   to be zero), including the accepted empty zero-count bit string;
//! - structural rejection: missing algorithm identifier or bit string, the
//!   wrong element tag, an extra trailing element in the SPKI;
//! - AlgorithmIdentifier rejection: missing OID, truncated or non-minimal OID
//!   encoding, truncated parameters, multiple parameter values;
//! - BIT STRING rejection: no unused-bits byte, count above seven, nonzero
//!   count with no data, set bits among the declared-unused trailing bits.
//!
//! Certificates are built with the same tiny zero-dependency DER constructor
//! used by the other test files so individual encodings can be forged byte for
//! byte.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXED_SERIAL: &str = "0E8A4C2F9B17D603";
const FIXED_SUBJECT: &str = "CN=example.com";
const FIXED_ISSUER: &str = "CN=Test CA";
const FIXED_NOT_BEFORE: &str = "2026-01-15T09:30:00Z";
const FIXED_NOT_AFTER: &str = "2027-01-15T09:30:00Z";

const CN: &[u64] = &[2, 5, 4, 3];

// ----- Success: AlgorithmIdentifier parameter forms ------------------------

#[test]
fn algorithm_parameters_may_be_omitted() {
    // AlgorithmIdentifier { algorithm OID } with no parameters element at all:
    // the common rsaEncryption-with-absent-NULL form must parse.
    let alg = seq(&oid(&[1, 2, 840, 113549, 1, 1, 1]));
    let spki = spki_with(&alg, &bit_string(&[0x00, 0x30, 0x03, 0x01, 0x00, 0x05]));
    let (out, _cert) = run_inspect("params-omitted", &build_cert_spki(&spki));
    assert_success(&out);
}

#[test]
fn algorithm_parameters_may_be_explicit_null() {
    // The other common form: the same OID plus an explicit NULL parameters.
    let alg = seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 1]), &null()]));
    let spki = spki_with(&alg, &bit_string(&[0x00, 0x01, 0x00]));
    let (out, _cert) = run_inspect("params-null", &build_cert_spki(&spki));
    assert_success(&out);
}

#[test]
fn other_single_complete_parameter_values_are_accepted() {
    // Parameters are ANY and their content is not interpreted: one complete
    // TLV of any decodable shape is enough. An INTEGER (as e.g. DSA uses), a
    // nested SEQUENCE (as EC/DSA use) and an OID (namedCurve) must all parse.
    let alg_int = seq(&concat(&[
        &oid(&[1, 2, 840, 10040, 4, 1]),
        &tlv(0x02, &[0x40]),
    ]));
    let (out, _cert) = run_inspect(
        "params-integer",
        &build_cert_spki(&spki_with(&alg_int, &bit_string(&[0x00, 0x02, 0x01, 0x00]))),
    );
    assert_success(&out);

    let inner = seq(&concat(&[&oid(&[1, 3, 101, 102]), &tlv(0x02, &[0x01, 0x00])]));
    let alg_seq = seq(&concat(&[&oid(&[1, 2, 840, 10040, 4, 3]), &inner]));
    let (out, _cert) = run_inspect(
        "params-sequence",
        &build_cert_spki(&spki_with(&alg_seq, &bit_string(&[0x00, 0x02, 0x01, 0x00]))),
    );
    assert_success(&out);

    let alg_oid = seq(&concat(&[
        &oid(&[1, 2, 840, 10045, 2, 1]),
        &oid(&[1, 2, 840, 10045, 3, 1, 7]), // prime256v1-style named curve
    ]));
    let (out, _cert) = run_inspect(
        "params-oid",
        &build_cert_spki(&spki_with(&alg_oid, &bit_string(&[0x00, 0x04]))),
    );
    assert_success(&out);
}

#[test]
fn unknown_but_decodable_algorithm_oid_is_accepted() {
    // The tool has no registry of key algorithms. 1.2.3.4.5.6 is not rsaEncryption
    // or anything else it knows, yet it is a perfectly valid OID and must not be
    // rejected — neither with absent parameters nor with an explicit NULL.
    let alg = seq(&oid(&[1, 2, 3, 4, 5, 6]));
    let spki = spki_with(&alg, &bit_string(&[0x00, 0xAA, 0xBB]));
    let (out, _cert) = run_inspect("unknown-oid-no-params", &build_cert_spki(&spki));
    assert_success(&out);

    let alg = seq(&concat(&[&oid(&[1, 2, 3, 4, 5, 6]), &null()]));
    let spki = spki_with(&alg, &bit_string(&[0x00, 0xAA, 0xBB]));
    let (out, _cert) = run_inspect("unknown-oid-null-params", &build_cert_spki(&spki));
    assert_success(&out);
}

// ----- Success: BIT STRING unused-bits forms --------------------------------

#[test]
fn zero_unused_bits_with_data_is_accepted() {
    // Every data byte is significant: the trailing byte is checked against a
    // zero mask, so any value (0xFF included) is fine when unused == 0.
    for (i, data) in [
        vec![0x01u8, 0x00u8],
        vec![0xAB, 0xCD, 0xEF],
        vec![0xFF, 0xFF],
    ]
    .into_iter()
    .enumerate()
    {
        let mut content = vec![0x00];
        content.extend_from_slice(&data);
        let spki = spki_with(&default_algorithm(), &bit_string(&content));
        let (out, _cert) = run_inspect(&format!("unused-zero-{i}"), &build_cert_spki(&spki));
        assert_success(&out);
    }
}

#[test]
fn nonzero_unused_bits_accepted_when_trailing_low_bits_are_zero() {
    // With k unused bits (1..=7) the low k bits of the final data byte must all
    // be zero. Boundary values: exactly one set bit *above* the unused region is
    // still legal, and each count in turn.
    for (i, (unused, last)) in [
        (1u8, 0xFE), // 1 unused bit, high 7 used
        (1, 0x80),
        (2, 0xFC),
        (3, 0xF8),
        (4, 0xF0),
        (5, 0xE0),
        (6, 0xC0),
        (7, 0x80), // only the sign/top bit used
    ]
    .into_iter()
    .enumerate()
    {
        let content = [unused, 0xFF, 0x10, last];
        let spki = spki_with(&default_algorithm(), &bit_string(&content));
        let (out, _cert) = run_inspect(&format!("unused-nonzero-ok-{i}"), &build_cert_spki(&spki));
        assert_success(&out);
    }
}

#[test]
fn empty_bit_string_with_zero_count_is_accepted() {
    // Existing behavior, pinned here: a BIT STRING holding only the count byte
    // (count 0, no data) parses. The code validates encodings only; it does not
    // judge whether these bytes are a usable public key.
    let spki = spki_with(&default_algorithm(), &bit_string(&[0x00]));
    let (out, _cert) = run_inspect("empty-bitstring", &build_cert_spki(&spki));
    assert_success(&out);
}

// ----- Failure: SubjectPublicKeyInfo structure ------------------------------

#[test]
fn missing_algorithm_identifier_is_rejected() {
    // The SPKI SEQUENCE contains only the BIT STRING; the name and validity
    // fields remain perfectly readable, yet the certificate must be refused.
    let spki = seq(&bit_string(&[0x00, 0x01, 0x00]));
    let (out, _cert) = run_inspect("spki-no-alg", &build_cert_spki(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn missing_subject_public_key_bit_string_is_rejected() {
    let spki = seq(&default_algorithm());
    let (out, _cert) = run_inspect("spki-no-key", &build_cert_spki(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn wrong_element_types_in_spki_are_rejected() {
    let key = bit_string(&[0x00, 0x01, 0x00]);

    // Algorithm identifier slot holds an OID TLV instead of a SEQUENCE.
    let spki = seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 1]), &key]));
    let (out, _cert) = run_inspect("spki-alg-not-sequence", &build_cert_spki(&spki));
    assert_invalid_certificate(&out);

    // Key slot holds an OCTET STRING rather than a BIT STRING.
    let spki = seq(&concat(&[&default_algorithm(), &tlv(0x04, &[0x01, 0x00])]));
    let (out, _cert) = run_inspect("spki-key-not-bitstring", &build_cert_spki(&spki));
    assert_invalid_certificate(&out);

    // Both present but swapped (BIT STRING first): the first slot must be a
    // SEQUENCE, so this is rejected rather than reordered.
    let spki = seq(&concat(&[&key, &default_algorithm()]));
    let (out, _cert) = run_inspect("spki-swapped", &build_cert_spki(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn extra_element_after_spki_pair_is_rejected() {
    // Exactly two elements belong in SubjectPublicKeyInfo; a third well-formed
    // element makes the structure invalid even though the first two parse.
    let spki = seq(&concat(&[
        &default_algorithm(),
        &bit_string(&[0x00, 0x01, 0x00]),
        &tlv(0x05, &[]),
    ]));
    let (out, _cert) = run_inspect("spki-extra-element", &build_cert_spki(&spki));
    assert_invalid_certificate(&out);
}

// ----- Failure: AlgorithmIdentifier -----------------------------------------

#[test]
fn algorithm_identifier_without_oid_is_rejected() {
    // Parameters present but no OBJECT IDENTIFIER: an OID in some other tag is
    // still the wrong type, and an empty algorithm SEQUENCE has no first
    // element at all.
    let alg = seq(&null());
    let spki = spki_with(&alg, &bit_string(&[0x00, 0x01, 0x00]));
    let (out, _cert) = run_inspect("alg-no-oid", &build_cert_spki(&spki));
    assert_invalid_certificate(&out);

    let alg = seq(&concat(&[&tlv(0x02, &[0x2A]), &null()]));
    let spki = spki_with(&alg, &bit_string(&[0x00, 0x01, 0x00]));
    let (out, _cert) = run_inspect("alg-oid-wrong-tag", &build_cert_spki(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn truncated_oid_encoding_is_rejected() {
    // A multi-byte base-128 arc whose final continuation byte is missing: the
    // OID TLV itself is bounded, so parsing the arc runs out of content.
    let alg = seq(&raw_tlv(0x06, &[0x02], &[0x88])); // 2.999 starts 0x88 0x37
    let spki = spki_with(&alg, &bit_string(&[0x00, 0x01, 0x00]));
    let (out, _cert) = run_inspect("alg-oid-truncated", &build_cert_spki(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn non_minimal_oid_encoding_is_rejected() {
    // The value 85 (single byte 0x55) encoded as 0x80 0x55 adds a forbidden
    // leading zero octet, which is not valid DER.
    let alg = seq(&tlv(0x06, &[0x55, 0x80, 0x55]));
    let spki = spki_with(&alg, &bit_string(&[0x00, 0x01, 0x00]));
    let (out, _cert) = run_inspect("alg-oid-nonminimal", &build_cert_spki(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn truncated_parameter_value_is_rejected() {
    // The parameter TLV announces four content bytes but only two follow; the
    // enclosing algorithm SEQUENCE length is deliberately forged large enough
    // to contain them, so the error is the truncation itself.
    let params = raw_tlv(0x02, &[0x04], &[0x01, 0x00]);
    let alg = raw_seq(&concat(&[&oid(&[1, 2, 840, 10040, 4, 1]), &params]));
    let spki = spki_with(&alg, &bit_string(&[0x00, 0x01, 0x00]));
    let (out, _cert) = run_inspect("alg-params-truncated", &build_cert_spki(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn multiple_parameter_values_are_rejected() {
    // parameters is OPTIONAL but singular: two NULLs (or any two complete
    // TLVs) after the OID must not be tolerated.
    let alg = seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 1]), &null(), &null()]));
    let spki = spki_with(&alg, &bit_string(&[0x00, 0x01, 0x00]));
    let (out, _cert) = run_inspect("alg-params-multiple", &build_cert_spki(&spki));
    assert_invalid_certificate(&out);

    let alg = seq(&concat(&[
        &oid(&[1, 2, 840, 113549, 1, 1, 1]),
        &tlv(0x02, &[0x01]),
        &oid(&[1, 2, 3]),
    ]));
    let spki = spki_with(&alg, &bit_string(&[0x00, 0x01, 0x00]));
    let (out, _cert) = run_inspect("alg-params-multiple-mixed", &build_cert_spki(&spki));
    assert_invalid_certificate(&out);
}

// ----- Failure: BIT STRING content ------------------------------------------

#[test]
fn bit_string_without_unused_bits_byte_is_rejected() {
    // Zero-length BIT STRING content: there is no count byte at all (distinct
    // from the accepted [0x00] empty form).
    let spki = spki_with(&default_algorithm(), &tlv(0x03, &[]));
    let (out, _cert) = run_inspect("bs-no-count", &build_cert_spki(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn unused_bits_count_above_seven_is_rejected() {
    // A byte has only eight bits; counts 8..=255 are nonsense. The trailing
    // data bytes are irrelevant — the count alone must fail.
    for (i, unused) in [8u8, 9, 16, 0x7F, 0xFF].into_iter().enumerate() {
        let spki = spki_with(&default_algorithm(), &bit_string(&[unused, 0x00]));
        let (out, _cert) = run_inspect(&format!("bs-count-{i}"), &build_cert_spki(&spki));
        assert_invalid_certificate(&out);
    }
}

#[test]
fn nonzero_count_without_data_is_rejected() {
    // Declaring unused bits requires at least one data byte to hold them; a
    // count of 1..=7 with no data following is malformed.
    for (i, unused) in [1u8, 4, 7].into_iter().enumerate() {
        let spki = spki_with(&default_algorithm(), &bit_string(&[unused]));
        let (out, _cert) = run_inspect(&format!("bs-nodata-{i}"), &build_cert_spki(&spki));
        assert_invalid_certificate(&out);
    }
}

#[test]
fn set_bits_in_unused_trailing_positions_are_rejected() {
    // For each k in 1..=7, a final data byte whose k-th low bit is set while
    // that bit is declared unused must fail — whether or not the higher used
    // bits are also set. The chosen last bytes pair with the accepted boundary
    // cases above (e.g. 0xFE is legal with 1 unused bit but illegal with 7),
    // pinning the exact mask instead of "nonzero trailing byte fails".
    for (i, (unused, last)) in [
        (1u8, 0x01),
        (1, 0xFF),
        (2, 0x02),
        (2, 0x03),
        (3, 0x0F),
        (4, 0x8F),
        (5, 0x1F),
        (6, 0xBF),
        (7, 0xFE),
    ]
    .into_iter()
    .enumerate()
    {
        let spki = spki_with(
            &default_algorithm(),
            &bit_string(&[unused, 0xAB, last]),
        );
        let (out, _cert) = run_inspect(&format!("bs-trailing-set-{i}"), &build_cert_spki(&spki));
        assert_invalid_certificate(&out);
    }
}

// ----- Assertions / process plumbing ---------------------------------------

/// A successful inspect must show exactly the five documented fields in their
/// fixed order and format; SubjectPublicKeyInfo is fully parsed but never
/// printed, so none of these values may move with the key encoding.
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

/// Every malformed-input case shares the documented contract: exit code 2,
/// nothing at all on stdout (so no readable field is printed before the key
/// error is noticed), and the fixed invalid-certificate prefix on stderr with
/// a non-empty reason after it. The reason's exact wording is intentionally not
/// pinned.
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
            "chainview-spki-test-{}-{tag}-{unique}.der",
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
// Same minimal-DER constructor as the other test files; `raw_tlv` /
// `raw_seq` inject hand-written length bytes so truncated encodings can be
// forged while the surrounding structures stay well-formed.

/// Standard key algorithm used when a test does not vary it:
/// rsaEncryption with an explicit NULL parameters.
fn default_algorithm() -> Vec<u8> {
    seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 1]), &null()]))
}

fn null() -> Vec<u8> {
    tlv(0x05, &[])
}

fn bit_string(content: &[u8]) -> Vec<u8> {
    tlv(0x03, content)
}

/// SubjectPublicKeyInfo { algorithm, subjectPublicKey } from the two complete
/// element encodings.
fn spki_with(algorithm: &[u8], subject_public_key: &[u8]) -> Vec<u8> {
    seq(&concat(&[algorithm, subject_public_key]))
}

/// Assemble a complete, parseable v1 DER certificate with fixed serial, names
/// and validity around the supplied SubjectPublicKeyInfo encoding.
fn build_cert_spki(spki: &[u8]) -> Vec<u8> {
    let serial = tlv(0x02, &[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]);
    let validity = seq(&concat(&[
        &tlv(0x17, b"260115093000Z"), // UTCTime 2026-01-15T09:30:00Z
        &tlv(0x17, b"270115093000Z"), // UTCTime 2027-01-15T09:30:00Z
    ]));

    let tbs = seq(&concat(&[
        &serial,
        &signature_algorithm(),
        &simple_cn_name("Test CA"),
        &validity,
        &simple_cn_name("example.com"),
        spki,
    ]));
    seq(&concat(&[&tbs, &signature_algorithm(), &tlv(0x03, &[0x00])]))
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

/// TLV with caller-supplied length bytes; used to forge truncated contents.
fn raw_tlv(tag: u8, len_bytes: &[u8], content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend_from_slice(len_bytes);
    out.extend_from_slice(content);
    out
}

/// SEQUENCE whose declared length is two bytes longer than its content so a
/// truncated inner TLV still sits inside the declared bounds.
fn raw_seq(content: &[u8]) -> Vec<u8> {
    raw_tlv(0x30, &der_len(content.len() + 2), content)
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
