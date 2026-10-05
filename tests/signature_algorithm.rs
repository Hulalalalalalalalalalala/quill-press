//! End-to-end regression tests for checking the two signature algorithm
//! descriptions when running `chainview inspect <DER-FILE>`.
//!
//! A certificate carries the signature algorithm twice:
//!
//! ```text
//! Certificate ::= SEQUENCE {
//!     tbsCertificate      TBSCertificate,
//!     signatureAlgorithm  AlgorithmIdentifier,   -- outer copy
//!     signatureValue      BIT STRING }
//! TBSCertificate ::= SEQUENCE {
//!     ...,
//!     signature           AlgorithmIdentifier,   -- copy to be signed
//!     ... }
//! ```
//!
//! Both copies must be complete, well-formed AlgorithmIdentifiers and must
//! agree exactly (same algorithm OID and the same parameter representation;
//! an absent parameter and an explicit NULL differ). Damage to either copy,
//! or any disagreement between the two healthy copies, must reject the whole
//! certificate with the usual malformed-input contract even though Subject,
//! Issuer, serial and validity all remain readable. The check is purely
//! about encoding and internal consistency: the signature itself is never
//! verified and trust is never judged, unknown but decodable OIDs are
//! accepted, and the signature OID need not match the subjectPublicKey
//! algorithm OID.
//!
//! Every case drives the real `chainview` binary: success prints exactly the
//! five fixed fields with exit code 0 and empty stderr; failure yields exit
//! code 2, completely empty stdout and a
//! `chainview: invalid DER certificate: <reason>` stderr whose reason names
//! whether the outer copy, the copy inside tbsCertificate, or their
//! disagreement caused the rejection.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXED_SERIAL: &str = "0E8A4C2F9B17D603";
const FIXED_SUBJECT: &str = "CN=example.com";
const FIXED_ISSUER: &str = "CN=Test CA";
const FIXED_NOT_BEFORE: &str = "2026-01-15T09:30:00Z";
const FIXED_NOT_AFTER: &str = "2027-01-15T09:30:00Z";

// ----- Algorithm OID arcs --------------------------------------------------

const SHA256_WITH_RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 11];
const RSA_ENCRYPTION: &[u64] = &[1, 2, 840, 113549, 1, 1, 1];
const ECDSA_WITH_SHA256: &[u64] = &[1, 2, 840, 10045, 4, 3, 2];

// ----- Success: both copies agree ------------------------------------------

#[test]
fn matching_algorithms_with_null_parameters_are_accepted() {
    // The standard sha256WithRSAEncryption form: OID plus an explicit NULL
    // in both copies.
    let alg = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("match-null", &build_cert(&alg, &alg));
    assert_success(&out);
}

#[test]
fn matching_algorithms_without_parameters_are_accepted() {
    // Parameters are OPTIONAL; both copies omitting them entirely is fine
    // (ECDSA algorithm identifiers are commonly encoded this way).
    let alg = alg_with_oid_only(ECDSA_WITH_SHA256);
    let (out, _cert) = run_inspect("match-absent", &build_cert(&alg, &alg));
    assert_success(&out);
}

#[test]
fn matching_algorithms_with_identical_non_null_parameters_are_accepted() {
    // The parameter need not be NULL: any single complete DER value is
    // accepted without interpretation, as long as both copies carry the very
    // same TLV. An INTEGER parameter and an OID parameter both qualify.
    let integer = tlv(0x02, &[0x01, 0x2A]);
    let alg = alg_with(SHA256_WITH_RSA, &integer);
    let (out, _cert) = run_inspect("match-int", &build_cert(&alg, &alg));
    assert_success(&out);

    let oid_param = oid(&[1, 2, 840, 10045, 3, 1, 7]);
    let alg = alg_with(&[1, 2, 3, 4], &oid_param);
    let (out, _cert) = run_inspect("match-oid-param", &build_cert(&alg, &alg));
    assert_success(&out);
}

#[test]
fn unknown_but_decodable_algorithm_oid_is_accepted() {
    // 1.2.840.99999.7 is not a recognized signature algorithm, but its OID
    // decodes by the normal base-128 rules (99999 spans several arc bytes);
    // recognition is irrelevant, in both parameter forms.
    let alg = alg_with_null_params(&[1, 2, 840, 99999, 7]);
    let (out, _cert) = run_inspect("unknown-null", &build_cert(&alg, &alg));
    assert_success(&out);

    let alg = alg_with_oid_only(&[1, 2, 840, 99999, 7]);
    let (out, _cert) = run_inspect("unknown-absent", &build_cert(&alg, &alg));
    assert_success(&out);
}

#[test]
fn signature_oid_may_differ_from_subject_public_key_oid() {
    // The comparison is only between the certificate's own two signature
    // algorithm copies. An RSA key (rsaEncryption 1.2.840.113549.1.1.1) used
    // with a hashed RSA signature OID (sha256WithRSAEncryption
    // 1.2.840.113549.1.1.11) is the completely normal case.
    let sig = alg_with_null_params(SHA256_WITH_RSA);
    let spki_alg = alg_with_null_params(RSA_ENCRYPTION);
    let (out, _cert) = run_inspect("sig-ne-key", &build_cert_spki(&sig, &sig, &spki_alg));
    assert_success(&out);
}

// ----- Failure: outer signatureAlgorithm is damaged -------------------------

#[test]
fn empty_outer_algorithm_sequence_is_rejected() {
    // An outer SEQUENCE with no OID at all cannot be salvaged; its location
    // must be named in the error even though every displayed field parses.
    let broken = seq(&[]);
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("outer-empty", &build_cert(&broken, &good));
    assert_invalid_at(&out, "outer signatureAlgorithm");
}

#[test]
fn outer_algorithm_with_wrong_first_tag_is_rejected() {
    // A NULL where the algorithm OID must stand is a tag error, not a
    // parameter.
    let broken = seq(&tlv(0x05, &[]));
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("outer-tag", &build_cert(&broken, &good));
    assert_invalid_at(&out, "outer signatureAlgorithm");
}

#[test]
fn outer_algorithm_with_empty_oid_is_rejected() {
    // 06 00: the OID TLV exists but names no arcs.
    let broken = seq(&raw_tlv(0x06, &[0x00], &[]));
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("outer-oid-empty", &build_cert(&broken, &good));
    assert_invalid_at(&out, "outer signatureAlgorithm");
}

#[test]
fn outer_algorithm_with_truncated_oid_arcs_is_rejected() {
    // 06 01 88: a continuation byte promises another base-128 byte that the
    // (correctly sized) enclosing SEQUENCE never provides.
    let broken = seq(&raw_tlv(0x06, &[0x01], &[0x88]));
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("outer-oid-trunc", &build_cert(&broken, &good));
    assert_invalid_at(&out, "outer signatureAlgorithm");
}

#[test]
fn outer_algorithm_with_truncated_oid_tlv_is_rejected() {
    // The OID TLV header announces three content bytes but the SEQUENCE only
    // contains two: a half-read OID must fail inside the outer identifier.
    let broken = seq(&concat(&[&[0x06, 0x03, 0x2A, 0x03]]));
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("outer-oid-tlv-trunc", &build_cert(&broken, &good));
    assert_invalid_at(&out, "outer signatureAlgorithm");
}

#[test]
fn outer_algorithm_with_nonminimal_oid_encoding_is_rejected() {
    // Arc value 1 encoded as 0x80 0x01 (leading 0x80 continuation byte) is
    // not the shortest base-128 encoding and is invalid DER.
    let broken = seq(&raw_tlv(0x06, &[0x02], &[0x80, 0x01]));
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("outer-oid-nonmin", &build_cert(&broken, &good));
    assert_invalid_at(&out, "outer signatureAlgorithm");
}

#[test]
fn outer_algorithm_with_truncated_parameter_is_rejected() {
    // A parameter TLV announcing five content bytes with only three present
    // cannot be skipped over.
    let truncated_params = raw_tlv(0x04, &[0x05], b"abc");
    let broken = seq(&concat(&[&oid(SHA256_WITH_RSA), &truncated_params]));
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("outer-param-trunc", &build_cert(&broken, &good));
    assert_invalid_at(&out, "outer signatureAlgorithm");
}

#[test]
fn outer_algorithm_with_extra_element_is_rejected() {
    // OID, NULL and then another TLV: at most one parameter element exists.
    let extra = tlv(0x02, &[0x01, 0x00]);
    let broken = seq(&concat(&[&oid(SHA256_WITH_RSA), &tlv(0x05, &[]), &extra]));
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("outer-extra", &build_cert(&broken, &good));
    assert_invalid_at(&out, "outer signatureAlgorithm");
}

// ----- Failure: signatureAlgorithm inside tbsCertificate is damaged ---------

#[test]
fn empty_tbs_algorithm_sequence_is_rejected() {
    let broken = seq(&[]);
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("tbs-empty", &build_cert(&good, &broken));
    assert_invalid_at(&out, "tbs signatureAlgorithm");
}

#[test]
fn tbs_algorithm_with_wrong_first_tag_is_rejected() {
    let broken = seq(&tlv(0x02, &[0x01, 0x01]));
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("tbs-tag", &build_cert(&good, &broken));
    assert_invalid_at(&out, "tbs signatureAlgorithm");
}

#[test]
fn tbs_algorithm_with_empty_oid_is_rejected() {
    let broken = seq(&raw_tlv(0x06, &[0x00], &[]));
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("tbs-oid-empty", &build_cert(&good, &broken));
    assert_invalid_at(&out, "tbs signatureAlgorithm");
}

#[test]
fn tbs_algorithm_with_truncated_oid_arcs_is_rejected() {
    let broken = seq(&raw_tlv(0x06, &[0x01], &[0x88]));
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("tbs-oid-trunc", &build_cert(&good, &broken));
    assert_invalid_at(&out, "tbs signatureAlgorithm");
}

#[test]
fn tbs_algorithm_with_truncated_oid_tlv_is_rejected() {
    let broken = seq(&concat(&[&[0x06, 0x03, 0x2A, 0x03]]));
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("tbs-oid-tlv-trunc", &build_cert(&good, &broken));
    assert_invalid_at(&out, "tbs signatureAlgorithm");
}

#[test]
fn tbs_algorithm_with_nonminimal_oid_encoding_is_rejected() {
    let broken = seq(&raw_tlv(0x06, &[0x02], &[0x80, 0x01]));
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("tbs-oid-nonmin", &build_cert(&good, &broken));
    assert_invalid_at(&out, "tbs signatureAlgorithm");
}

#[test]
fn tbs_algorithm_with_truncated_parameter_is_rejected() {
    let truncated_params = raw_tlv(0x04, &[0x05], b"abc");
    let broken = seq(&concat(&[&oid(SHA256_WITH_RSA), &truncated_params]));
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("tbs-param-trunc", &build_cert(&good, &broken));
    assert_invalid_at(&out, "tbs signatureAlgorithm");
}

#[test]
fn tbs_algorithm_with_extra_element_is_rejected() {
    // Two NULL parameters after the OID: the second is an extra element.
    let broken = seq(&concat(&[
        &oid(SHA256_WITH_RSA),
        &tlv(0x05, &[]),
        &tlv(0x05, &[]),
    ]));
    let good = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("tbs-extra", &build_cert(&good, &broken));
    assert_invalid_at(&out, "tbs signatureAlgorithm");
}

// ----- Failure: the two well-formed copies disagree -------------------------

#[test]
fn copies_with_different_algorithm_oids_are_rejected() {
    // Both identifiers are perfectly formed; they simply name different
    // algorithms. The error must report the disagreement rather than damage.
    let outer = alg_with_null_params(SHA256_WITH_RSA);
    let inner = alg_with_null_params(ECDSA_WITH_SHA256);
    let (out, _cert) = run_inspect("mismatch-oid", &build_cert(&outer, &inner));
    assert_invalid_at(&out, "mismatch");
}

#[test]
fn copies_where_only_outer_has_parameters_are_rejected() {
    // Same OID, explicit NULL outside versus absent inside: these are
    // different representations and must not be treated as equal.
    let outer = alg_with_null_params(SHA256_WITH_RSA);
    let inner = alg_with_oid_only(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("mismatch-outer-param", &build_cert(&outer, &inner));
    assert_invalid_at(&out, "mismatch");
}

#[test]
fn copies_where_only_inner_has_parameters_are_rejected() {
    let outer = alg_with_oid_only(SHA256_WITH_RSA);
    let inner = alg_with_null_params(SHA256_WITH_RSA);
    let (out, _cert) = run_inspect("mismatch-inner-param", &build_cert(&outer, &inner));
    assert_invalid_at(&out, "mismatch");
}

#[test]
fn copies_with_different_parameter_values_are_rejected() {
    // Both sides carry a parameter, but NULL versus INTEGER 0 have different
    // complete DER representations; the matching OID does not save them.
    let outer = alg_with_null_params(SHA256_WITH_RSA);
    let inner = alg_with(SHA256_WITH_RSA, &tlv(0x02, &[0x01, 0x00]));
    let (out, _cert) = run_inspect("mismatch-param-value", &build_cert(&outer, &inner));
    assert_invalid_at(&out, "mismatch");
}

#[test]
fn copies_with_different_non_null_parameters_are_rejected() {
    // Two distinct non-NULL parameter TLVs must compare unequal as well.
    let outer = alg_with(SHA256_WITH_RSA, &tlv(0x04, b"aa"));
    let inner = alg_with(SHA256_WITH_RSA, &tlv(0x04, b"bb"));
    let (out, _cert) = run_inspect("mismatch-param-bytes", &build_cert(&outer, &inner));
    assert_invalid_at(&out, "mismatch");
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

/// The malformed-input contract: exit code 2, nothing on stdout, and the
/// fixed invalid-certificate prefix on stderr followed by a reason that names
/// the expected location (`needle`: the outer copy, the tbsCertificate copy,
/// or their mismatch).
fn assert_invalid_at(out: &Output, needle: &str) {
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
    let reason = &stderr[prefix.len()..];
    assert!(
        reason.contains(needle),
        "reason {reason:?} does not identify {needle:?}"
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
            "chainview-sigalg-test-{}-{tag}-{unique}.der",
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
// The outer signatureAlgorithm and the copy inside tbsCertificate are
// supplied independently (already-encoded TLVs) so any damage or disagreement
// can be crafted while every enclosing wrapper keeps a correct length.

fn build_cert(outer_alg: &[u8], tbs_alg: &[u8]) -> Vec<u8> {
    let spki_alg = alg_with_null_params(RSA_ENCRYPTION);
    build_cert_spki(outer_alg, tbs_alg, &spki_alg)
}

fn build_cert_spki(outer_alg: &[u8], tbs_alg: &[u8], spki_alg: &[u8]) -> Vec<u8> {
    let serial = tlv(0x02, &[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]);
    let validity = seq(&concat(&[
        &tlv(0x17, b"260115093000Z"),
        &tlv(0x17, b"270115093000Z"),
    ]));
    let subject = simple_cn_name("example.com");
    let issuer = simple_cn_name("Test CA");
    let spki = seq(&concat(&[spki_alg, &bit_string(&[0x00, 0x01, 0x00])]));

    let tbs = seq(&concat(&[
        &serial,
        tbs_alg,
        &issuer,
        &validity,
        &subject,
        &spki,
    ]));
    seq(&concat(&[&tbs, outer_alg, &tlv(0x03, &[0x00])]))
}

fn alg_with_oid_only(arcs: &[u64]) -> Vec<u8> {
    seq(&oid(arcs))
}

fn alg_with_null_params(arcs: &[u64]) -> Vec<u8> {
    alg_with(arcs, &tlv(0x05, &[]))
}

fn alg_with(arcs: &[u64], params: &[u8]) -> Vec<u8> {
    seq(&concat(&[&oid(arcs), params]))
}

fn bit_string(content: &[u8]) -> Vec<u8> {
    tlv(0x03, content)
}

fn simple_cn_name(cn: &str) -> Vec<u8> {
    let atv = seq(&concat(&[&oid(&[2, 5, 4, 3]), &tlv(0x0C, cn.as_bytes())]));
    let rdn = tlv(0x31, &atv);
    tlv(0x30, &rdn)
}

/// TLV with caller-supplied length bytes; used to forge truncated or
/// non-minimal encodings.
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
