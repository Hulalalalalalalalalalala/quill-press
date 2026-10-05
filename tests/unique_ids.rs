//! End-to-end regression tests for the optional tbsCertificate fields after
//! subjectPublicKeyInfo when running `chainview inspect <DER-FILE>`:
//!
//! ```text
//! issuerUniqueID  [1] IMPLICIT BIT STRING OPTIONAL,  -- v2/v3 only
//! subjectUniqueID [2] IMPLICIT BIT STRING OPTIONAL,  -- v2/v3 only
//! extensions      [3] EXPLICIT Extensions OPTIONAL   -- v3 only, last
//! ```
//!
//! inspect used to skip every tail element except the extensions field as
//! long as its outer length was readable, so v1 certificates carrying unique
//! IDs, duplicated or reordered IDs, IDs after extensions and arbitrary
//! unknown fields were all displayed. These tests pin the fixed contract:
//!
//! - both unique IDs are optional and legal only with an explicit v2/v3
//!   version; a v1 certificate (omitted version or explicit v1) rejects
//!   either one;
//! - each ID occurs at most once, [1] must precede [2] when both are
//!   present, and either one may appear alone;
//! - on a v3 certificate extensions, when present, must follow the IDs and
//!   close the tail (anything after them is rejected);
//! - unique IDs are IMPLICIT BIT STRINGs: tags must be primitive 0x81/0x82
//!   (explicit wrappers 0xA1/0xA2 or other constructed forms are rejected)
//!   and the content obeys BIT STRING rules: a leading unused-bits byte in
//!   0..=7, no trailing set bits among the declared unused low bits, and a
//!   count-only encoding accepted only when the count is zero;
//! - any other tag after subjectPublicKeyInfo is a format error rather than
//!   data to skip, reorder or discard.
//!
//! Legal IDs may contain zero bytes and the two ID values may be identical;
//! inspect neither interprets nor displays them. Every case drives the real
//! binary: success prints exactly the five fixed fields, failure is exit
//! code 2, completely empty stdout and a
//! `chainview: invalid DER certificate: <reason>` stderr whose reason names
//! the offending field.

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

// ----- Success: legal unique IDs --------------------------------------------

#[test]
fn v2_certificate_with_issuer_unique_id_only_is_accepted() {
    let tail = issuer_uid(&[0x00, 0xFF, 0x00]);
    let cert = build_cert(Some(1), &tail);
    assert_success(&run_inspect("v2-issuer-only", &cert));
}

#[test]
fn v2_certificate_with_subject_unique_id_only_is_accepted() {
    let tail = subject_uid(&[0x00, 0x01, 0x02, 0x03]);
    let cert = build_cert(Some(1), &tail);
    assert_success(&run_inspect("v2-subject-only", &cert));
}

#[test]
fn v2_certificate_with_both_unique_ids_in_order_is_accepted() {
    let tail = concat(&[
        &issuer_uid(&[0x00, 0xAA]),
        &subject_uid(&[0x00, 0xBB]),
    ]);
    let cert = build_cert(Some(1), &tail);
    assert_success(&run_inspect("v2-both", &cert));
}

#[test]
fn v3_certificate_with_both_unique_ids_and_extensions_is_accepted() {
    let tail = concat(&[
        &issuer_uid(&[0x00, 0xFF]),
        &subject_uid(&[0x00, 0xAA, 0xBB]),
        &extensions_field(&extension(UNKNOWN_OID, None, &[0x00])),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_success(&run_inspect("v3-both-ext", &cert));
}

#[test]
fn v3_certificate_with_issuer_uid_then_extensions_is_accepted() {
    let tail = concat(&[
        &issuer_uid(&[0x00]),
        &extensions_field(&extension(UNKNOWN_OID, None, &[0x00])),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_success(&run_inspect("v3-issuer-ext", &cert));
}

#[test]
fn v3_certificate_with_no_unique_ids_but_extensions_still_accepted() {
    // Pre-existing ordering: extensions remain the first tail element when
    // neither unique ID is present.
    let tail = extensions_field(&extension(UNKNOWN_OID, None, &[0x00]));
    let cert = build_cert(Some(2), &tail);
    assert_success(&run_inspect("v3-ext-only", &cert));
}

#[test]
fn unique_id_legal_bit_string_forms_are_accepted() {
    // Count-only zero, data ending in a zero byte, a nonzero unused-bits
    // count whose low bits are clear, embedded zero bytes and identical
    // issuer/subject values are all legal.
    let values: &[&[u8]] = &[
        &[0x00],                      // count byte only, count zero
        &[0x00, 0x00],                // one zero data byte
        &[0x00, 0xAA, 0x00, 0xFF],    // zero count, embedded zero byte
        &[0x01, 0x02],                // 1 unused bit, low bit clear
        &[0x03, 0xF8],                // 3 unused bits, low 3 bits clear
        &[0x07, 0x80],                // 7 unused bits, low 7 bits clear
    ];
    for (i, value) in values.iter().enumerate() {
        let tail = concat(&[&issuer_uid(value), &subject_uid(value)]);
        let cert = build_cert(Some(2), &tail);
        assert_success(&run_inspect(&format!("uid-legal-{i}"), &cert));
    }
}

// ----- Failure: version rules -----------------------------------------------

#[test]
fn v1_certificate_without_version_carrying_issuer_uid_is_rejected() {
    let cert = build_cert(None, &issuer_uid(&[0x00]));
    assert_invalid_certificate(&run_inspect("v1nover-issuer-uid", &cert));
}

#[test]
fn v1_certificate_without_version_carrying_subject_uid_is_rejected() {
    let cert = build_cert(None, &subject_uid(&[0x00]));
    assert_invalid_certificate(&run_inspect("v1nover-subject-uid", &cert));
}

#[test]
fn explicit_v1_certificate_carrying_issuer_uid_is_rejected() {
    let cert = build_cert(Some(0), &issuer_uid(&[0x00]));
    assert_invalid_certificate(&run_inspect("v1-issuer-uid", &cert));
}

#[test]
fn explicit_v1_certificate_carrying_subject_uid_is_rejected() {
    let cert = build_cert(Some(0), &subject_uid(&[0x00]));
    assert_invalid_certificate(&run_inspect("v1-subject-uid", &cert));
}

// ----- Failure: ordering and repetition -------------------------------------

#[test]
fn subject_unique_id_before_issuer_unique_id_is_rejected() {
    let tail = concat(&[
        &subject_uid(&[0x00, 0xBB]),
        &issuer_uid(&[0x00, 0xAA]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_invalid_certificate(&run_inspect("uid-reversed", &cert));
}

#[test]
fn duplicate_issuer_unique_id_is_rejected() {
    let tail = concat(&[
        &issuer_uid(&[0x00, 0xAA]),
        &issuer_uid(&[0x00, 0xBB]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_invalid_certificate(&run_inspect("dup-issuer-uid", &cert));
}

#[test]
fn duplicate_subject_unique_id_is_rejected() {
    let tail = concat(&[
        &subject_uid(&[0x00, 0xAA]),
        &subject_uid(&[0x00, 0xBB]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_invalid_certificate(&run_inspect("dup-subject-uid", &cert));
}

#[test]
fn issuer_unique_id_after_subject_unique_id_with_three_fields_is_rejected() {
    // [1], [2], [1]: the second [1] follows a [2], so it is out of order
    // even though the first pair was legal.
    let tail = concat(&[
        &issuer_uid(&[0x00]),
        &subject_uid(&[0x00]),
        &issuer_uid(&[0x00]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_invalid_certificate(&run_inspect("uid-121", &cert));
}

#[test]
fn unique_id_after_extensions_is_rejected() {
    let tail = concat(&[
        &extensions_field(&extension(UNKNOWN_OID, None, &[0x00])),
        &issuer_uid(&[0x00]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_invalid_certificate(&run_inspect("uid-after-ext", &cert));
}

#[test]
fn subject_uid_then_extensions_then_issuer_uid_is_rejected() {
    let tail = concat(&[
        &subject_uid(&[0x00]),
        &extensions_field(&extension(UNKNOWN_OID, None, &[0x00])),
        &issuer_uid(&[0x00]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_invalid_certificate(&run_inspect("uid-wrap-ext", &cert));
}

#[test]
fn unknown_field_after_subject_public_key_info_is_rejected() {
    // A well-formed, unknown context [4] element must not be skipped: the
    // five displayed fields decode fine but the certificate is corrupt.
    let tail = tlv(0x84, &[0x01, 0x2A]);
    let cert = build_cert(Some(2), &tail);
    assert_invalid_certificate(&run_inspect("unknown-ctx", &cert));
}

#[test]
fn universal_field_after_subject_public_key_info_is_rejected() {
    // Even a plain INTEGER in the tail cannot be ignored or reordered away.
    let tail = tlv(0x02, &[0x01, 0x2A]);
    let cert = build_cert(Some(2), &tail);
    assert_invalid_certificate(&run_inspect("integer-tail", &cert));
}

#[test]
fn unknown_field_after_extensions_is_rejected() {
    let tail = concat(&[
        &extensions_field(&extension(UNKNOWN_OID, None, &[0x00])),
        &tlv(0x84, &[0x00]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_invalid_certificate(&run_inspect("unknown-after-ext", &cert));
}

// ----- Failure: tag construction / explicit wrapping ------------------------

#[test]
fn issuer_unique_id_with_constructed_tag_is_rejected() {
    // 0xA1 = context [1] CONSTRUCTED: an explicit BIT STRING wrapper or any
    // constructed unique ID; the IMPLICIT primitive tag 0x81 is the only
    // legal form.
    let tail = tlv(0xA1, &tlv(0x03, &[0x00]));
    let cert = build_cert(Some(2), &tail);
    assert_invalid_certificate(&run_inspect("issuer-uid-constructed", &cert));
}

#[test]
fn subject_unique_id_with_constructed_tag_is_rejected() {
    let tail = tlv(0xA2, &tlv(0x03, &[0x00]));
    let cert = build_cert(Some(2), &tail);
    assert_invalid_certificate(&run_inspect("subject-uid-constructed", &cert));
}

// ----- Failure: BIT STRING content of the unique IDs ------------------------

#[test]
fn issuer_unique_id_missing_unused_bits_byte_is_rejected() {
    let cert = build_cert(Some(2), &issuer_uid(&[]));
    assert_invalid_certificate(&run_inspect("issuer-uid-nocount", &cert));
}

#[test]
fn subject_unique_id_missing_unused_bits_byte_is_rejected() {
    let cert = build_cert(Some(2), &subject_uid(&[]));
    assert_invalid_certificate(&run_inspect("subject-uid-nocount", &cert));
}

#[test]
fn unique_id_unused_bits_count_above_seven_is_rejected() {
    let cert = build_cert(Some(2), &issuer_uid(&[0x08, 0x00]));
    assert_invalid_certificate(&run_inspect("uid-count-8", &cert));
}

#[test]
fn unique_id_nonzero_count_without_data_is_rejected() {
    let cert = build_cert(Some(2), &subject_uid(&[0x01]));
    assert_invalid_certificate(&run_inspect("uid-nodata", &cert));
}

#[test]
fn unique_id_with_set_unused_tail_bits_is_rejected() {
    let cases: &[&[u8]] = &[
        &[0x01, 0x01], // 1 unused bit, low bit set
        &[0x03, 0xF9], // 3 unused bits, one of them set
        &[0x07, 0x81], // 7 unused bits, lowest bit set
    ];
    for (i, value) in cases.iter().enumerate() {
        let cert = build_cert(Some(2), &issuer_uid(value));
        assert_invalid_certificate(&run_inspect(&format!("uid-tailset-{i}"), &cert));
    }
}

#[test]
fn truncated_unique_id_field_is_rejected() {
    // The field announces three content bytes but supplies one; a correct
    // enclosing TBS length must not hide the truncation.
    let broken = raw_tlv(0x81, &[0x03], &[0x00]);
    let cert = build_cert(Some(2), &broken);
    assert_invalid_certificate(&run_inspect("uid-truncated", &cert));
}

// ----- Assertions / process plumbing ---------------------------------------

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

/// Every malformed case: exit code 2, nothing on stdout (the parse fails
/// before any field is printed), and the fixed invalid-certificate prefix
/// with a non-empty reason after it that names a unique ID, extension or
/// tail field. Exact wording is intentionally not pinned.
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
    const PREFIX: &str = "chainview: invalid DER certificate: ";
    assert!(
        stderr.starts_with(PREFIX),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr[PREFIX.len()..].trim().len() > 1,
        "missing error reason in stderr: {stderr}"
    );
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
            "chainview-uid-test-{}-{tag}-{unique}.der",
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

/// issuerUniqueID [1] IMPLICIT BIT STRING with raw BIT STRING content.
fn issuer_uid(bit_string_content: &[u8]) -> Vec<u8> {
    tlv(0x81, bit_string_content)
}

/// subjectUniqueID [2] IMPLICIT BIT STRING with raw BIT STRING content.
fn subject_uid(bit_string_content: &[u8]) -> Vec<u8> {
    tlv(0x82, bit_string_content)
}

/// [3] EXPLICIT wrapping one Extensions SEQUENCE holding one extension item.
fn extensions_field(extension_item: &[u8]) -> Vec<u8> {
    tlv(0xA3, &tlv(0x30, extension_item))
}

/// Extension ::= SEQUENCE { extnID OID, critical BOOLEAN DEFAULT FALSE,
/// extnValue OCTET STRING }.
fn extension(arcs: &[u64], critical: Option<bool>, value: &[u8]) -> Vec<u8> {
    let mut content = oid(arcs);
    if let Some(flag) = critical {
        content.extend_from_slice(&tlv(0x01, &[if flag { 0xFF } else { 0x00 }]));
    }
    content.extend_from_slice(&tlv(0x04, value));
    tlv(0x30, &content)
}

/// Assemble a complete certificate with fixed visible fields around the
/// optional explicit version (`None` = v1 with no version field) and an
/// arbitrary, already-encoded TBS tail.
fn build_cert(version: Option<u8>, tail: &[u8]) -> Vec<u8> {
    let serial = tlv(0x02, &[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]);
    let validity = seq(&concat(&[
        &tlv(0x17, b"260115093000Z"),
        &tlv(0x17, b"270115093000Z"),
    ]));
    let subject = simple_cn_name("example.com");
    let issuer = simple_cn_name("Test CA");
    let spki = spki();

    let mut tbs_content = Vec::new();
    if let Some(v) = version {
        tbs_content.extend_from_slice(&tlv(0xA0, &tlv(0x02, &[v])));
    }
    tbs_content.extend_from_slice(&serial);
    tbs_content.extend_from_slice(&signature_algorithm());
    tbs_content.extend_from_slice(&issuer);
    tbs_content.extend_from_slice(&validity);
    tbs_content.extend_from_slice(&subject);
    tbs_content.extend_from_slice(&spki);
    tbs_content.extend_from_slice(tail);
    let tbs = seq(&tbs_content);

    seq(&concat(&[
        &tbs,
        &signature_algorithm(),
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
    seq(&concat(&[&alg, &tlv(0x03, &[0x00, 0x01, 0x00])]))
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
