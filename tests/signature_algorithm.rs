//! End-to-end regression tests for the two signature algorithm fields when
//! running `chainview inspect <DER-FILE>`.
//!
//! A certificate carries two copies of the signature AlgorithmIdentifier:
//! the outer `signatureAlgorithm` sibling of the signature BIT STRING and
//! the `signature` field inside tbsCertificate. Both must be parsed in full
//! and must agree exactly; readable Subject/Issuer/validity fields must not
//! let a certificate through when either copy is damaged or the copies
//! disagree. Like every other structural check this is about DER encoding
//! and internal consistency only — the signature itself is never verified
//! and trust and expiry are out of scope.
//!
//! Coverage:
//! - accepted: both copies absent parameters, both with an explicit NULL,
//!   both carrying the same non-NULL parameter TLV, an unknown but decodable
//!   algorithm OID, and a signature OID that differs from the subject public
//!   key algorithm OID (e.g. sha256WithRSA over an rsaEncryption key);
//! - damaged outer identifier: not a SEQUENCE, empty SEQUENCE, first element
//!   not an OID, empty/truncated/non-minimal OID, truncated parameter, an
//!   extra element after the parameter;
//! - damaged tbs identifier: the same set of malformed forms with the outer
//!   copy kept valid, proving each location is checked independently;
//! - disagreement: different OIDs, a parameter on only one side (absent vs
//!   explicit NULL in both directions), NULL vs another parameter type, and
//!   differing parameter encodings even though the OIDs agree.
//!
//! Every case drives the real `chainview` binary: success means exit code 0,
//! empty stderr and exactly the five fixed fields in order; failure means
//! exit code 2, completely empty stdout and a
//! `chainview: invalid DER certificate: <reason>` stderr whose reason names
//! the outer field, the tbs field, or the mismatch.

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
const SHA1_WITH_RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 5];
const RSA_ENCRYPTION: &[u64] = &[1, 2, 840, 113549, 1, 1, 1];
const P256_CURVE: &[u64] = &[1, 2, 840, 10045, 3, 1, 7];
const P384_CURVE: &[u64] = &[1, 2, 840, 10045, 3, 1, 34];

// ----- Success: both identifiers well formed and equal ---------------------

#[test]
fn matching_identifiers_with_explicit_null_are_accepted() {
    // The common PKCS#1 form: OID plus an explicit NULL in both slots.
    let cert = build_cert(
        &alg_with_null_params(SHA256_WITH_RSA),
        &alg_with_null_params(SHA256_WITH_RSA),
    );
    let (out, _cert) = run_inspect("both-null", &cert);
    assert_success(&out);
}

#[test]
fn matching_identifiers_without_parameters_are_accepted() {
    // Parameters are OPTIONAL; both copies omitting them is equally valid.
    let cert = build_cert(
        &alg_with_oid_only(SHA256_WITH_RSA),
        &alg_with_oid_only(SHA256_WITH_RSA),
    );
    let (out, _cert) = run_inspect("both-absent", &cert);
    assert_success(&out);
}

#[test]
fn matching_identifiers_with_same_non_null_parameter_are_accepted() {
    // The parameter need not be NULL; any single complete TLV is accepted as
    // long as the two copies carry the same encoding.
    let int_param = tlv(0x02, &[0x01, 0x2A]); // INTEGER 42
    let cert = build_cert(
        &alg_with(SHA256_WITH_RSA, &int_param),
        &alg_with(SHA256_WITH_RSA, &int_param),
    );
    let (out, _cert) = run_inspect("both-int", &cert);
    assert_success(&out);

    let oid_param = oid(P256_CURVE); // e.g. an EC named-curve style value
    let cert = build_cert(
        &alg_with(SHA256_WITH_RSA, &oid_param),
        &alg_with(SHA256_WITH_RSA, &oid_param),
    );
    let (out, _cert) = run_inspect("both-oid", &cert);
    assert_success(&out);
}

#[test]
fn matching_identifiers_with_unknown_but_decodable_oid_are_accepted() {
    // 1.2.3.4 is not a recognized signature algorithm, but recognition is
    // irrelevant: the OID decodes by the base-128 rules and the copies match.
    let cert = build_cert(
        &alg_with_null_params(&[1, 2, 3, 4]),
        &alg_with_null_params(&[1, 2, 3, 4]),
    );
    let (out, _cert) = run_inspect("unknown-oid", &cert);
    assert_success(&out);
}

#[test]
fn signature_oid_may_differ_from_subject_public_key_oid() {
    // Only the certificate's own two copies must match. The signature OID
    // (sha256WithRSAEncryption) and the public key OID (rsaEncryption) are
    // different OIDs naming different things, which is normal.
    let spki = spki_of(
        &alg_with_null_params(RSA_ENCRYPTION),
        &default_key(),
    );
    let cert = build_cert_with_spki(
        &alg_with_null_params(SHA256_WITH_RSA),
        &alg_with_null_params(SHA256_WITH_RSA),
        &spki,
    );
    let (out, _cert) = run_inspect("sig-ne-spki", &cert);
    assert_success(&out);
}

// ----- Failure: damaged outer signatureAlgorithm ---------------------------

#[test]
fn outer_identifier_that_is_not_a_sequence_is_rejected() {
    // A SET where the outer AlgorithmIdentifier SEQUENCE must stand.
    let wrong = tlv(0x31, &concat(&[&oid(SHA256_WITH_RSA), &null_param()]));
    let cert = build_cert(&alg_with_null_params(SHA256_WITH_RSA), &wrong);
    let (out, _cert) = run_inspect("outer-tag", &cert);
    assert_invalid_certificate_about(&out, "outer signature");
}

#[test]
fn outer_identifier_empty_sequence_is_rejected() {
    let cert = build_cert(
        &alg_with_null_params(SHA256_WITH_RSA),
        &seq(&[]),
    );
    let (out, _cert) = run_inspect("outer-empty", &cert);
    assert_invalid_certificate_about(&out, "outer signature");
}

#[test]
fn outer_identifier_without_oid_is_rejected() {
    // First (and only) element is a NULL, not an OID.
    let only_null = seq(&null_param());
    let cert = build_cert(&alg_with_null_params(SHA256_WITH_RSA), &only_null);
    let (out, _cert) = run_inspect("outer-no-oid", &cert);
    assert_invalid_certificate_about(&out, "outer signature");
}

#[test]
fn outer_identifier_with_empty_oid_is_rejected() {
    let bad = seq(&tlv(0x06, &[]));
    let cert = build_cert(&alg_with_null_params(SHA256_WITH_RSA), &bad);
    let (out, _cert) = run_inspect("outer-empty-oid", &cert);
    assert_invalid_certificate_about(&out, "outer signature");
}

#[test]
fn outer_identifier_with_truncated_oid_is_rejected() {
    // 0x88 promises another base-128 byte that never arrives; wrappers are
    // correctly sized so the failure is inside this OID.
    let truncated_oid = raw_tlv(0x06, &[0x01], &[0x88]);
    let bad = seq(&truncated_oid);
    let cert = build_cert(&alg_with_null_params(SHA256_WITH_RSA), &bad);
    let (out, _cert) = run_inspect("outer-trunc-oid", &cert);
    assert_invalid_certificate_about(&out, "outer signature");
}

#[test]
fn outer_identifier_with_nonminimal_oid_is_rejected() {
    let nonminimal_oid = raw_tlv(0x06, &[0x02], &[0x80, 0x01]);
    let bad = seq(&nonminimal_oid);
    let cert = build_cert(&alg_with_null_params(SHA256_WITH_RSA), &bad);
    let (out, _cert) = run_inspect("outer-nonmin-oid", &cert);
    assert_invalid_certificate_about(&out, "outer signature");
}

#[test]
fn outer_identifier_with_truncated_parameter_is_rejected() {
    // OCTET STRING announcing five bytes but providing three, after a good OID.
    let truncated = raw_tlv(0x04, &[0x05], b"abc");
    let bad = seq(&concat(&[&oid(SHA256_WITH_RSA), &truncated]));
    let cert = build_cert(&alg_with_null_params(SHA256_WITH_RSA), &bad);
    let (out, _cert) = run_inspect("outer-trunc-param", &cert);
    assert_invalid_certificate_about(&out, "outer signature");
}

#[test]
fn outer_identifier_with_extra_element_is_rejected() {
    // OID, NULL, then a second TLV: parameters are at most one element.
    let bad = seq(&concat(&[
        &oid(SHA256_WITH_RSA),
        &null_param(),
        &tlv(0x02, &[0x01, 0x00]),
    ]));
    let cert = build_cert(&alg_with_null_params(SHA256_WITH_RSA), &bad);
    let (out, _cert) = run_inspect("outer-extra", &cert);
    assert_invalid_certificate_about(&out, "outer signature");
}

// ----- Failure: damaged tbs signature algorithm ----------------------------

#[test]
fn tbs_identifier_that_is_not_a_sequence_is_rejected() {
    let wrong = tlv(0x31, &concat(&[&oid(SHA256_WITH_RSA), &null_param()]));
    let cert = build_cert(&wrong, &alg_with_null_params(SHA256_WITH_RSA));
    let (out, _cert) = run_inspect("tbs-tag", &cert);
    assert_invalid_certificate_about(&out, "tbs signature");
}

#[test]
fn tbs_identifier_empty_sequence_is_rejected() {
    let cert = build_cert(
        &seq(&[]),
        &alg_with_null_params(SHA256_WITH_RSA),
    );
    let (out, _cert) = run_inspect("tbs-empty", &cert);
    assert_invalid_certificate_about(&out, "tbs signature");
}

#[test]
fn tbs_identifier_without_oid_is_rejected() {
    let only_null = seq(&null_param());
    let cert = build_cert(&only_null, &alg_with_null_params(SHA256_WITH_RSA));
    let (out, _cert) = run_inspect("tbs-no-oid", &cert);
    assert_invalid_certificate_about(&out, "tbs signature");
}

#[test]
fn tbs_identifier_with_empty_oid_is_rejected() {
    let bad = seq(&tlv(0x06, &[]));
    let cert = build_cert(&bad, &alg_with_null_params(SHA256_WITH_RSA));
    let (out, _cert) = run_inspect("tbs-empty-oid", &cert);
    assert_invalid_certificate_about(&out, "tbs signature");
}

#[test]
fn tbs_identifier_with_truncated_oid_is_rejected() {
    let truncated_oid = raw_tlv(0x06, &[0x01], &[0x88]);
    let bad = seq(&truncated_oid);
    let cert = build_cert(&bad, &alg_with_null_params(SHA256_WITH_RSA));
    let (out, _cert) = run_inspect("tbs-trunc-oid", &cert);
    assert_invalid_certificate_about(&out, "tbs signature");
}

#[test]
fn tbs_identifier_with_nonminimal_oid_is_rejected() {
    let nonminimal_oid = raw_tlv(0x06, &[0x02], &[0x80, 0x01]);
    let bad = seq(&nonminimal_oid);
    let cert = build_cert(&bad, &alg_with_null_params(SHA256_WITH_RSA));
    let (out, _cert) = run_inspect("tbs-nonmin-oid", &cert);
    assert_invalid_certificate_about(&out, "tbs signature");
}

#[test]
fn tbs_identifier_with_truncated_parameter_is_rejected() {
    let truncated = raw_tlv(0x04, &[0x05], b"abc");
    let bad = seq(&concat(&[&oid(SHA256_WITH_RSA), &truncated]));
    let cert = build_cert(&bad, &alg_with_null_params(SHA256_WITH_RSA));
    let (out, _cert) = run_inspect("tbs-trunc-param", &cert);
    assert_invalid_certificate_about(&out, "tbs signature");
}

#[test]
fn tbs_identifier_with_extra_element_is_rejected() {
    let bad = seq(&concat(&[
        &oid(SHA256_WITH_RSA),
        &null_param(),
        &tlv(0x02, &[0x01, 0x00]),
    ]));
    let cert = build_cert(&bad, &alg_with_null_params(SHA256_WITH_RSA));
    let (out, _cert) = run_inspect("tbs-extra", &cert);
    assert_invalid_certificate_about(&out, "tbs signature");
}

// ----- Failure: the two structurally valid identifiers disagree ------------

#[test]
fn identifiers_with_different_oids_are_rejected() {
    // Both carry NULL, but sha256WithRSA != sha1WithRSA.
    let cert = build_cert(
        &alg_with_null_params(SHA256_WITH_RSA),
        &alg_with_null_params(SHA1_WITH_RSA),
    );
    let (out, _cert) = run_inspect("mismatch-oid", &cert);
    assert_invalid_certificate_about(&out, "do not match");
}

#[test]
fn absent_parameter_vs_explicit_null_is_rejected_in_both_directions() {
    let cert = build_cert(
        &alg_with_oid_only(SHA256_WITH_RSA),
        &alg_with_null_params(SHA256_WITH_RSA),
    );
    let (out, _cert) = run_inspect("mismatch-absent-null", &cert);
    assert_invalid_certificate_about(&out, "do not match");

    let cert = build_cert(
        &alg_with_null_params(SHA256_WITH_RSA),
        &alg_with_oid_only(SHA256_WITH_RSA),
    );
    let (out, _cert) = run_inspect("mismatch-null-absent", &cert);
    assert_invalid_certificate_about(&out, "do not match");
}

#[test]
fn null_parameter_vs_other_parameter_type_is_rejected() {
    // Same OID; one side NULL, the other side a complete INTEGER.
    let cert = build_cert(
        &alg_with_null_params(SHA256_WITH_RSA),
        &alg_with(SHA256_WITH_RSA, &tlv(0x02, &[0x01, 0x00])),
    );
    let (out, _cert) = run_inspect("mismatch-null-int", &cert);
    assert_invalid_certificate_about(&out, "do not match");
}

#[test]
fn differing_parameter_values_are_rejected_even_with_same_oid() {
    // INTEGER 0 vs INTEGER 1.
    let cert = build_cert(
        &alg_with(SHA256_WITH_RSA, &tlv(0x02, &[0x01, 0x00])),
        &alg_with(SHA256_WITH_RSA, &tlv(0x02, &[0x01, 0x01])),
    );
    let (out, _cert) = run_inspect("mismatch-int-values", &cert);
    assert_invalid_certificate_about(&out, "do not match");

    // Two different OID parameters (two named curves) compare unequal too:
    // comparison is over the full parameter DER, not just its tag.
    let cert = build_cert(
        &alg_with(SHA256_WITH_RSA, &oid(P256_CURVE)),
        &alg_with(SHA256_WITH_RSA, &oid(P384_CURVE)),
    );
    let (out, _cert) = run_inspect("mismatch-oid-values", &cert);
    assert_invalid_certificate_about(&out, "do not match");
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

/// Malformed-input contract: exit code 2, nothing on stdout, the fixed
/// invalid-certificate prefix, and a reason that names the expected location
/// (`phrase`: the outer field, the tbs field, or the mismatch).
fn assert_invalid_certificate_about(out: &Output, phrase: &str) {
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
        stderr.contains(phrase),
        "reason should mention {phrase:?}: {stderr}"
    );
    assert!(
        stderr["chainview: invalid DER certificate: ".len()..]
            .trim()
            .len()
            > 1,
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
            "chainview-sigalgs-test-{}-{tag}-{unique}.der",
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
// The two signature AlgorithmIdentifier slots (tbs interior and certificate
// exterior) are supplied independently so individual encodings can be crafted
// byte for byte; everything else is a fixed, well-formed v1 certificate.

fn build_cert(tbs_sig_alg: &[u8], outer_sig_alg: &[u8]) -> Vec<u8> {
    let spki = spki_of(
        &alg_with_null_params(RSA_ENCRYPTION),
        &default_key(),
    );
    build_cert_with_spki(tbs_sig_alg, outer_sig_alg, &spki)
}

fn build_cert_with_spki(
    tbs_sig_alg: &[u8],
    outer_sig_alg: &[u8],
    spki: &[u8],
) -> Vec<u8> {
    let serial = tlv(0x02, &[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]);
    let validity = seq(&concat(&[
        &tlv(0x17, b"260115093000Z"),
        &tlv(0x17, b"270115093000Z"),
    ]));
    let subject = simple_cn_name("example.com");
    let issuer = simple_cn_name("Test CA");

    let tbs = seq(&concat(&[
        &serial,
        tbs_sig_alg,
        &issuer,
        &validity,
        &subject,
        spki,
    ]));
    seq(&concat(&[&tbs, outer_sig_alg, &tlv(0x03, &[0x00])]))
}

fn spki_of(alg: &[u8], key: &[u8]) -> Vec<u8> {
    seq(&concat(&[alg, key]))
}

fn alg_with_oid_only(arcs: &[u64]) -> Vec<u8> {
    seq(&oid(arcs))
}

fn alg_with_null_params(arcs: &[u64]) -> Vec<u8> {
    alg_with(arcs, &null_param())
}

fn alg_with(arcs: &[u64], params: &[u8]) -> Vec<u8> {
    seq(&concat(&[&oid(arcs), params]))
}

fn null_param() -> Vec<u8> {
    tlv(0x05, &[])
}

fn default_key() -> Vec<u8> {
    tlv(0x03, &[0x00, 0x01, 0x00])
}

fn simple_cn_name(cn: &str) -> Vec<u8> {
    let atv = seq(&concat(&[&oid(&[2, 5, 4, 3]), &tlv(0x0C, cn.as_bytes())]));
    let rdn = tlv(0x31, &atv);
    tlv(0x30, &rdn)
}

/// TLV with caller-supplied length bytes; used to forge truncated or
/// non-minimal OID encodings while every enclosing wrapper stays correct.
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
