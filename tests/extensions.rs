//! End-to-end regression tests for X.509 extensions decoding when running
//! `chainview inspect <DER-FILE>`.
//!
//! Extensions never appear among the five displayed fields, but a v3
//! certificate's `extensions [3] EXPLICIT` field must be read in full:
//!
//! Extensions  ::=  SEQUENCE SIZE (1..MAX) OF Extension
//! Extension   ::=  SEQUENCE {
//!   extnID      OBJECT IDENTIFIER,
//!   critical    BOOLEAN DEFAULT FALSE,
//!   extnValue   OCTET STRING }
//!
//! Coverage:
//! - acceptance: extensions remain optional; when present the version must be
//!   v3 (an implicit-v1 certificate and explicit v1/v2 certificates may not
//!   carry them); one or several well-formed extensions, unknown OIDs,
//!   critical omitted (== FALSE) or encoded as the single DER byte FF, and an
//!   empty extnValue OCTET STRING are all accepted; expiry is irrelevant;
//! - structure: the [3] wrapper holds exactly one non-empty SEQUENCE and may
//!   appear once; every extension is a SEQUENCE of OID, optional BOOLEAN and
//!   mandatory OCTET STRING in that order with nothing missing or appended;
//! - encodings: the OID must be complete and in shortest (minimal-DER) form;
//!   critical must be absent or exactly FF (00, other non-zero bytes, empty
//!   and multi-byte BOOLEANs are all rejected);
//! - truncation/trailing data anywhere inside the field is rejected.
//!
//! Like the other suites these drive the real binary: success means exit 0,
//! empty stderr and exactly the five fixed fields in order (no extension
//! summary is ever added); any malformed extension means exit 2, completely
//! empty stdout and a `chainview: invalid DER certificate:` reason that names
//! the extensions field, reported before any field is printed.
//!
//! Certificates are built with the tiny zero-dependency DER constructor at
//! the bottom of this file; malformed bytes are wrapped by correct outer
//! lengths so rejection is proven to come from the element under test.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXED_SERIAL: &str = "0E8A4C2F9B17D603";
const FIXED_SUBJECT: &str = "CN=example.com";
const FIXED_ISSUER: &str = "CN=Test CA";
const FIXED_NOT_BEFORE: &str = "2026-01-15T09:30:00Z";
const FIXED_NOT_AFTER: &str = "2027-01-15T09:30:00Z";

const KEY_USAGE: &[u64] = &[2, 5, 29, 15];
const SAN: &[u64] = &[2, 5, 29, 17];

// ----- Success: legal encodings --------------------------------------------

#[test]
fn v3_certificate_with_one_extension_is_accepted() {
    // Unknown OID, critical omitted (DEFAULT FALSE), empty extnValue: all are
    // legal. The extension payload is never interpreted.
    let ext = extension_of(&oid(&[1, 2, 3, 4]), None, &[]);
    let cert = build_cert(Some(2), &extensions_field(&[ext]));
    let (out, _cert) = run_inspect("one-ext", &cert);
    assert_success(&out);
}

#[test]
fn v3_critical_true_and_nonempty_value_are_accepted() {
    // critical TRUE is exactly one FF byte and never causes failure; the
    // OCTET STRING carries arbitrary opaque bytes.
    let ext = extension_of(&oid(KEY_USAGE), Some(vec![0xFF]), &[0x03, 0x02, 0x01, 0xA0]);
    let cert = build_cert(Some(2), &extensions_field(&[ext]));
    let (out, _cert) = run_inspect("critical-true", &cert);
    assert_success(&out);
}

#[test]
fn v3_multiple_extensions_mixed_criticality_are_accepted() {
    let a = extension_of(&oid(KEY_USAGE), None, &[0x03, 0x02, 0x01, 0xA0]);
    let b = extension_of(&oid(SAN), Some(vec![0xFF]), &[0x30, 0x00]);
    let cert = build_cert(Some(2), &extensions_field(&[a, b]));
    let (out, _cert) = run_inspect("two-ext", &cert);
    assert_success(&out);
}

#[test]
fn certificates_without_extensions_remain_accepted_at_any_version() {
    // Implicit v1 (no version field), explicit v1 (0) and explicit v2 (1)
    // without extensions all inspect exactly as before.
    for (tag, version) in [("v1-implicit", None), ("v1", Some(0u8)), ("v2", Some(1))] {
        let cert = build_cert(version, &[]);
        let (out, _cert) = run_inspect(tag, &cert);
        assert_success(&out);
    }
}

#[test]
fn expired_or_not_yet_valid_certificate_with_extensions_still_displays() {
    // Validity is never judged: a window entirely in the past with a sound
    // extensions field still succeeds.
    let ext = extension_of(&oid(SAN), None, &[0x30, 0x00]);
    let cert = build_cert_with_validity(
        Some(2),
        &extensions_field(&[ext]),
        b"200101000000Z",
        b"010101000000Z",
    );
    let (out, _cert) = run_inspect("expired-ext", &cert);
    assert_success_times(&out, "2020-01-01T00:00:00Z", "2001-01-01T00:00:00Z");
}

// ----- Failure: version constraints ----------------------------------------

#[test]
fn extensions_rejected_without_explicit_v3_version() {
    let ext = extension_of(&oid(KEY_USAGE), None, &[0x30, 0x00]);
    let field = extensions_field(&[ext]);

    // No version field at all: an implicit v1 certificate.
    let cert = build_cert(None, &field);
    let (out, _cert) = run_inspect("ext-implicit-v1", &cert);
    assert_invalid_extensions(&out);

    // Explicitly declared v1 ...
    let cert = build_cert(Some(0), &field);
    let (out, _cert) = run_inspect("ext-v1", &cert);
    assert_invalid_extensions(&out);

    // ... and v2.
    let cert = build_cert(Some(1), &field);
    let (out, _cert) = run_inspect("ext-v2", &cert);
    assert_invalid_extensions(&out);
}

#[test]
fn duplicate_extensions_fields_are_rejected() {
    let ext = extension_of(&oid(KEY_USAGE), None, &[0x30, 0x00]);
    let field = extensions_field(&[ext]);
    let two = [field.as_slice(), field.as_slice()].concat();
    let cert = build_cert(Some(2), &two);
    let (out, _cert) = run_inspect("ext-dup", &cert);
    assert_invalid_extensions(&out);
}

// ----- Failure: [3] wrapper and extensions SEQUENCE ------------------------

#[test]
fn empty_or_misstructured_extensions_wrapper_is_rejected() {
    // [3] with no content at all.
    let cert = build_cert(Some(2), &tlv(0xA3, &[]));
    let (out, _cert) = run_inspect("wrap-empty", &cert);
    assert_invalid_extensions(&out);

    // [3] containing an empty SEQUENCE (SIZE (1..MAX) violated).
    let cert = build_cert(Some(2), &tlv(0xA3, &seq(&[])));
    let (out, _cert) = run_inspect("wrap-empty-seq", &cert);
    assert_invalid_extensions(&out);

    // [3] containing something that is not a SEQUENCE.
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x02, &[0x01, 0x00])));
    let (out, _cert) = run_inspect("wrap-nonseq", &cert);
    assert_invalid_extensions(&out);

    // [3] containing two SEQUENCEs instead of exactly one.
    let inner = [seq(&[]).as_slice(), seq(&[]).as_slice()].concat();
    let cert = build_cert(Some(2), &tlv(0xA3, &inner));
    let (out, _cert) = run_inspect("wrap-two-seq", &cert);
    assert_invalid_extensions(&out);

    // A correct extensions SEQUENCE followed by a stray element in the [3].
    let ext = extension_of(&oid(KEY_USAGE), None, &[0x30, 0x00]);
    let body = [
        seq(&ext).as_slice(), // the single SEQUENCE expected ...
        tlv(0x02, &[0x01, 0x00]).as_slice(), // ... plus a forbidden element
    ]
    .concat();
    let cert = build_cert(Some(2), &tlv(0xA3, &body));
    let (out, _cert) = run_inspect("wrap-extra", &cert);
    assert_invalid_extensions(&out);
}

#[test]
fn truncated_extensions_wrapper_is_rejected() {
    // The wrapper announces more content bytes than actually follow inside
    // the correctly-sized tbsCertificate; the outer length is intact.
    let cert = build_cert(Some(2), &raw_tlv(0xA3, &[0x20], &[0x30, 0x00]));
    let (out, _cert) = run_inspect("wrap-trunc", &cert);
    assert_invalid_extensions(&out);
}

// ----- Failure: individual Extension structure -----------------------------

#[test]
fn extension_that_is_not_a_sequence_is_rejected() {
    // A bare (OID, OCTET STRING) pair instead of a SEQUENCE-wrapped
    // Extension.
    let item = [oid(KEY_USAGE).as_slice(), tlv(0x04, &[]).as_slice()].concat();
    let cert = build_cert(Some(2), &extensions_field_raw(&item));
    let (out, _cert) = run_inspect("item-noseq", &cert);
    assert_invalid_extensions(&out);
}

#[test]
fn extension_missing_required_parts_is_rejected() {
    // No extnID at all.
    let cert = build_cert(Some(2), &extensions_field_raw(&seq(&tlv(0x04, &[]))));
    let (out, _cert) = run_inspect("item-no-oid", &cert);
    assert_invalid_extensions(&out);

    // extnID present, extnValue missing.
    let cert = build_cert(Some(2), &extensions_field_raw(&seq(&oid(KEY_USAGE))));
    let (out, _cert) = run_inspect("item-no-value", &cert);
    assert_invalid_extensions(&out);

    // Only critical + value, still no OID.
    let body = [tlv(0x01, &[0xFF]).as_slice(), tlv(0x04, &[]).as_slice()].concat();
    let cert = build_cert(Some(2), &extensions_field_raw(&seq(&body)));
    let (out, _cert) = run_inspect("item-no-oid-crit", &cert);
    assert_invalid_extensions(&out);
}

#[test]
fn extension_wrong_order_and_appended_elements_are_rejected() {
    // OID, OCTET STRING, BOOLEAN: critical must precede the value.
    let body = [
        oid(KEY_USAGE).as_slice(),
        tlv(0x04, &[]).as_slice(),
        tlv(0x01, &[0xFF]).as_slice(),
    ]
    .concat();
    let cert = build_cert(Some(2), &extensions_field_raw(&seq(&body)));
    let (out, _cert) = run_inspect("item-order", &cert);
    assert_invalid_extensions(&out);

    // A fourth element appended after a complete extension.
    let body = [
        oid(KEY_USAGE).as_slice(),
        tlv(0x04, &[]).as_slice(),
        tlv(0x02, &[0x01, 0x2A]).as_slice(),
    ]
    .concat();
    let cert = build_cert(Some(2), &extensions_field_raw(&seq(&body)));
    let (out, _cert) = run_inspect("item-extra", &cert);
    assert_invalid_extensions(&out);
}

#[test]
fn extension_value_must_be_octet_string() {
    // A BIT STRING in the extnValue position is a type error, not opaque
    // content the decoder may skip.
    let body = [oid(KEY_USAGE).as_slice(), tlv(0x03, &[0x00]).as_slice()].concat();
    let cert = build_cert(Some(2), &extensions_field_raw(&seq(&body)));
    let (out, _cert) = run_inspect("item-value-tag", &cert);
    assert_invalid_extensions(&out);
}

// ----- Failure: OID and BOOLEAN encodings ----------------------------------

#[test]
fn extension_oid_must_be_complete_and_minimal() {
    // Dangling continuation byte: the arc never terminates.
    let body = [raw_tlv(0x06, &[], &[0x88]).as_slice(), tlv(0x04, &[]).as_slice()].concat();
    let cert = build_cert(Some(2), &extensions_field_raw(&seq(&body)));
    let (out, _cert) = run_inspect("oid-trunc", &cert);
    assert_invalid_extensions(&out);

    // Arc value 1 as 80 01 is not the shortest base-128 encoding.
    let body = [
        raw_tlv(0x06, &[], &[0x55, 0x80, 0x01]).as_slice(),
        tlv(0x04, &[]).as_slice(),
    ]
    .concat();
    let cert = build_cert(Some(2), &extensions_field_raw(&seq(&body)));
    let (out, _cert) = run_inspect("oid-nonmin", &cert);
    assert_invalid_extensions(&out);

    // An OID TLV with no content at all.
    let body = [tlv(0x06, &[]).as_slice(), tlv(0x04, &[]).as_slice()].concat();
    let cert = build_cert(Some(2), &extensions_field_raw(&seq(&body)));
    let (out, _cert) = run_inspect("oid-empty", &cert);
    assert_invalid_extensions(&out);
}

#[test]
fn critical_boolean_only_accepts_single_ff_byte() {
    for (tag, content) in [
        ("crit-00", vec![0x00u8]),       // explicit DEFAULT FALSE
        ("crit-01", vec![0x01]),         // non-FF non-zero "true"
        ("crit-7f", vec![0x7F]),
        ("crit-fe", vec![0xFE]),
        ("crit-empty", vec![]),          // no content byte
        ("crit-multi", vec![0xFF, 0xFF]), // two content bytes
    ] {
        let body = [
            oid(KEY_USAGE).as_slice(),
            tlv(0x01, &content).as_slice(),
            tlv(0x04, &[]).as_slice(),
        ]
        .concat();
        let cert = build_cert(Some(2), &extensions_field_raw(&seq(&body)));
        let (out, _cert) = run_inspect(tag, &cert);
        assert_invalid_extensions(&out);
    }
}

#[test]
fn truncated_or_garbage_extension_content_is_rejected() {
    // The extension SEQUENCE announces its full length but loses its last
    // byte; all enclosing wrappers keep correct lengths.
    let good = extension_of(&oid(KEY_USAGE), None, &[0x30, 0x00]);
    let truncated = raw_tlv(0x30, &der_len(good.len()), &good[..good.len() - 1]);
    let cert = build_cert(Some(2), &extensions_field_raw(&truncated));
    let (out, _cert) = run_inspect("item-trunc", &cert);
    assert_invalid_extensions(&out);

    // A stray byte that is not the start of a complete TLV after the last
    // well-formed extension.
    let ext = extension_of(&oid(KEY_USAGE), None, &[0x30, 0x00]);
    let item = [ext.as_slice(), &[0x42]].concat();
    let cert = build_cert(Some(2), &extensions_field_raw(&item));
    let (out, _cert) = run_inspect("seq-garbage", &cert);
    assert_invalid_extensions(&out);
}

// ----- Assertions / process plumbing --------------------------------------

/// Success still prints exactly the five fixed fields in their fixed order
/// and adds no extension summary; stderr stays empty.
fn assert_success(out: &Output) {
    assert_success_times(out, FIXED_NOT_BEFORE, FIXED_NOT_AFTER);
}

/// As `assert_success`, but for certificates built with explicit validity.
fn assert_success_times(out: &Output, not_before: &str, not_after: &str) {
    assert_eq!(out.status.code(), Some(0), "stderr={}", lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "stderr={}", lossy(&out.stderr));
    assert_eq!(
        lossy(&out.stdout),
        format!(
            "Subject: {FIXED_SUBJECT}\n\
             Issuer: {FIXED_ISSUER}\n\
             Serial Number: {FIXED_SERIAL}\n\
             Not Before: {not_before}\n\
             Not After: {not_after}\n"
        )
    );
}

/// Malformed extensions share the usual invalid-certificate contract and the
/// reason must locate the failure in the extensions field; exact wording is
/// not pinned.
fn assert_invalid_extensions(out: &Output) {
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
        stderr
            .get("chainview: invalid DER certificate: ".len()..)
            .is_some_and(|reason| reason.to_ascii_lowercase().contains("extension")),
        "error reason must point at the extensions field: {stderr}"
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
            "chainview-ext-test-{}-{tag}-{unique}.der",
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

/// Assemble a complete certificate around fixed fields. `version` is the
/// encoded version value (0 = v1, 1 = v2, 2 = v3); `None` omits the explicit
/// version field, meaning v1. `trailing` is appended after the
/// SubjectPublicKeyInfo inside tbsCertificate (extensions, unique IDs, ...).
fn build_cert(version: Option<u8>, trailing: &[u8]) -> Vec<u8> {
    build_cert_with_validity(
        version,
        trailing,
        b"260115093000Z",
        b"270115093000Z",
    )
}

fn build_cert_with_validity(
    version: Option<u8>,
    trailing: &[u8],
    not_before: &[u8],
    not_after: &[u8],
) -> Vec<u8> {
    let serial = tlv(0x02, &[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]);
    let validity = seq(&concat(&[
        &tlv(0x17, not_before),
        &tlv(0x17, not_after),
    ]));
    let subject = simple_cn_name("example.com");
    let issuer = simple_cn_name("Test CA");
    let spki = seq(&concat(&[&signature_algorithm(), &tlv(0x03, &[0x00, 0x01, 0x00])]));

    let mut tbs_body = Vec::new();
    if let Some(v) = version {
        tbs_body.extend_from_slice(&tlv(0xA0, &tlv(0x02, &[v])));
    }
    tbs_body.extend_from_slice(&serial);
    tbs_body.extend_from_slice(&signature_algorithm());
    tbs_body.extend_from_slice(&issuer);
    tbs_body.extend_from_slice(&validity);
    tbs_body.extend_from_slice(&subject);
    tbs_body.extend_from_slice(&spki);
    tbs_body.extend_from_slice(trailing);
    let tbs = seq(&tbs_body);

    seq(&concat(&[&tbs, &signature_algorithm(), &tlv(0x03, &[0x00])]))
}

fn signature_algorithm() -> Vec<u8> {
    seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 11]), &tlv(0x05, &[])]))
}

fn simple_cn_name(cn: &str) -> Vec<u8> {
    let atv = seq(&concat(&[&oid(&[2, 5, 4, 3]), &tlv(0x0C, cn.as_bytes())]));
    tlv(0x30, &tlv(0x31, &atv))
}

/// Extension ::= SEQUENCE { extnID OID, critical BOOLEAN OPTIONAL,
/// extnValue OCTET STRING }. `critical` carries the exact BOOLEAN content
/// bytes (use [0xFF] for TRUE; omit for DEFAULT FALSE).
fn extension_of(oid: &[u8], critical: Option<Vec<u8>>, value: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(oid);
    if let Some(c) = critical {
        body.extend_from_slice(&tlv(0x01, &c));
    }
    body.extend_from_slice(&tlv(0x04, value));
    seq(&body)
}

/// Well-formed [3] EXPLICIT wrapper around one SEQUENCE OF complete
/// extensions.
fn extensions_field(extensions: &[Vec<u8>]) -> Vec<u8> {
    let items = concat(&extensions.iter().map(Vec::as_slice).collect::<Vec<_>>());
    tlv(0xA3, &seq(&items))
}

/// [3] wrapper whose inner SEQUENCE wraps raw, possibly malformed bytes.
fn extensions_field_raw(inner_item_bytes: &[u8]) -> Vec<u8> {
    tlv(0xA3, &seq(inner_item_bytes))
}

/// TLV with caller-supplied length bytes; used to forge truncations.
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
