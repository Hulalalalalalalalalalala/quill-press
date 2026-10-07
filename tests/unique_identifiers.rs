//! End-to-end regression tests for the two optional unique-identifier fields
//! of `tbsCertificate` when running `chainview inspect <DER-FILE>`:
//!
//! ```text
//! issuerUniqueID  [1] IMPLICIT BIT STRING  (v2/v3, at most once)
//! subjectUniqueID [2] IMPLICIT BIT STRING  (v2/v3, at most once)
//! ```
//!
//! Neither field is ever displayed, so a viewer that merely skips the tail
//! after subjectPublicKeyInfo would accept a structurally corrupt certificate
//! as long as the five visible fields parse. These tests pin the fixed
//! contract instead:
//!
//! - on an otherwise correct v2 or v3 certificate either field may appear
//!   alone, or both may appear, but issuerUniqueID must precede
//!   subjectUniqueID;
//! - on a v3 certificate carrying extensions [3] every unique ID must stand
//!   before the extensions field (which closes tbsCertificate);
//! - a legal placement changes nothing in the output: still exactly the five
//!   fixed lines, exit 0, empty stderr;
//! - each unique ID may occur at most once: identical or different values,
//!   adjacent or separated by the other unique ID, the whole certificate is
//!   rejected rather than one copy picked;
//! - reversed issuer/subject order and any unique ID after extensions are
//!   structural errors even when every field content is legal;
//! - the fields are v2/v3-only: a v1 certificate (implicit or explicit)
//!   carrying either is rejected.
//!
//! Every failure case must exit with code 2, produce completely empty stdout
//! and write a `chainview: invalid DER certificate: <reason>` stderr whose
//! reason names the unique-identifier field(s) involved, so structural
//! corruption stays distinguishable from a file-read failure.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXED_SERIAL: &str = "0E8A4C2F9B17D603";
const FIXED_SUBJECT: &str = "CN=example.com";
const FIXED_ISSUER: &str = "CN=Test CA";
const FIXED_NOT_BEFORE: &str = "2026-01-15T09:30:00Z";
const FIXED_NOT_AFTER: &str = "2027-01-15T09:30:00Z";

const ISSUER_UID: &str = "issuerUniqueID";
const SUBJECT_UID: &str = "subjectUniqueID";

// ----- Success: legal presence and placement --------------------------------

#[test]
fn v2_certificate_with_only_issuer_unique_id_is_accepted() {
    let tail = issuer_uid(&[0x00]);
    let cert = build_cert(Some(1), &tail);
    assert_success(&run_inspect("v2-only-issuer-uid", &cert));
}

#[test]
fn v2_certificate_with_only_subject_unique_id_is_accepted() {
    let tail = subject_uid(&[0x00]);
    let cert = build_cert(Some(1), &tail);
    assert_success(&run_inspect("v2-only-subject-uid", &cert));
}

#[test]
fn v2_certificate_with_both_unique_ids_in_order_is_accepted() {
    let tail = concat(&[
        &issuer_uid(&[0x00, 0xAA, 0xBB]),
        &subject_uid(&[0x00, 0xCC, 0xDD]),
    ]);
    let cert = build_cert(Some(1), &tail);
    assert_success(&run_inspect("v2-both-uids", &cert));
}

#[test]
fn v3_certificate_with_only_issuer_unique_id_is_accepted() {
    let cert = build_cert(Some(2), &issuer_uid(&[0x00]));
    assert_success(&run_inspect("v3-only-issuer-uid", &cert));
}

#[test]
fn v3_certificate_with_only_subject_unique_id_is_accepted() {
    let cert = build_cert(Some(2), &subject_uid(&[0x00]));
    assert_success(&run_inspect("v3-only-subject-uid", &cert));
}

#[test]
fn v3_certificate_with_both_unique_ids_in_order_is_accepted() {
    let tail = concat(&[&issuer_uid(&[0x00]), &subject_uid(&[0x00])]);
    let cert = build_cert(Some(2), &tail);
    assert_success(&run_inspect("v3-both-uids", &cert));
}

#[test]
fn v3_certificate_with_unique_ids_before_extensions_is_accepted() {
    // Both IDs before [3], each ID alone before [3]: every legal mix with the
    // extensions field must pass.
    let both = concat(&[
        &issuer_uid(&[0x00]),
        &subject_uid(&[0x00]),
        &extensions_field(),
    ]);
    assert_success(&run_inspect(
        "v3-both-uids-before-ext",
        &build_cert(Some(2), &both),
    ));

    let issuer_only = concat(&[&issuer_uid(&[0x00]), &extensions_field()]);
    assert_success(&run_inspect(
        "v3-issuer-uid-before-ext",
        &build_cert(Some(2), &issuer_only),
    ));

    let subject_only = concat(&[&subject_uid(&[0x00]), &extensions_field()]);
    assert_success(&run_inspect(
        "v3-subject-uid-before-ext",
        &build_cert(Some(2), &subject_only),
    ));
}

#[test]
fn legal_unique_id_contents_never_change_the_five_displayed_lines() {
    // Empty bit string, plain data, data with all unused bits zeroed: the
    // unique ID carries BIT STRING content, and none of it may surface in the
    // fixed five-line output or alter how name/serial/validity render. The
    // exact stdout comparison in assert_success pins that.
    const LEGAL_CONTENTS: &[&[u8]] = &[
        &[0x00],             // zero-length bit string
        &[0x00, 0x01, 0x02], // data, no unused bits
        &[0x07, 0x80],       // 7 unused bits, all zero
        &[0x03, 0xF8],       // 3 unused bits, all zero
    ];
    for (version, version_name) in [(Some(1u8), "v2"), (Some(2), "v3")] {
        for (tag, tag_name) in [(0x81u8, "issuer"), (0x82, "subject")] {
            for (i, content) in LEGAL_CONTENTS.iter().enumerate() {
                let tail = tlv(tag, content);
                let cert = build_cert(version, &tail);
                let label = format!("legal-{version_name}-{tag_name}-uid-content-{i}");
                assert_success(&run_inspect(&label, &cert));
            }
        }
    }
}

// ----- Failure: a unique ID occurs twice ------------------------------------

#[test]
fn duplicate_issuer_unique_id_with_identical_value_is_rejected() {
    let field = issuer_uid(&[0x00, 0xAA]);
    let tail = concat(&[&field, &field]);
    let cert = build_cert(Some(2), &tail);
    assert_duplicate_field(&run_inspect("dup-issuer-uid-same", &cert), ISSUER_UID);
}

#[test]
fn duplicate_issuer_unique_id_with_different_values_is_rejected() {
    // Same field twice with different BIT STRING contents must not be resolved
    // by keeping one value: the whole certificate is corrupt.
    let tail = concat(&[
        &issuer_uid(&[0x00, 0xAA]),
        &issuer_uid(&[0x03, 0xF8]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_duplicate_field(&run_inspect("dup-issuer-uid-diff", &cert), ISSUER_UID);
}

#[test]
fn duplicate_subject_unique_id_with_identical_value_is_rejected() {
    let field = subject_uid(&[0x00, 0xBB]);
    let tail = concat(&[&field, &field]);
    let cert = build_cert(Some(2), &tail);
    assert_duplicate_field(&run_inspect("dup-subject-uid-same", &cert), SUBJECT_UID);
}

#[test]
fn duplicate_subject_unique_id_with_different_values_is_rejected() {
    let tail = concat(&[
        &subject_uid(&[0x00]),
        &subject_uid(&[0x00, 0x01, 0x02, 0x03]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_duplicate_field(&run_inspect("dup-subject-uid-diff", &cert), SUBJECT_UID);
}

#[test]
fn duplicate_issuer_unique_id_separated_by_subject_unique_id_is_rejected() {
    // issuer, subject, issuer: the duplicate is not adjacent and a second,
    // different unique ID stands between the copies; it must still be caught,
    // not skipped.
    let tail = concat(&[
        &issuer_uid(&[0x00, 0xAA]),
        &subject_uid(&[0x00, 0xBB]),
        &issuer_uid(&[0x00, 0xCC]),
    ]);
    let cert = build_cert(Some(1), &tail);
    assert_duplicate_field(&run_inspect("dup-issuer-uid-split", &cert), ISSUER_UID);
}

#[test]
fn duplicate_unique_ids_are_rejected_even_when_extensions_also_present() {
    // A well-formed extensions field does not legitimize a duplicate unique ID
    // that precedes it.
    let tail = concat(&[
        &issuer_uid(&[0x00]),
        &issuer_uid(&[0x00, 0x01]),
        &extensions_field(),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_duplicate_field(&run_inspect("dup-issuer-uid-with-ext", &cert), ISSUER_UID);

    let tail = concat(&[
        &subject_uid(&[0x00]),
        &subject_uid(&[0x00, 0x01]),
        &extensions_field(),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_duplicate_field(&run_inspect("dup-subject-uid-with-ext", &cert), SUBJECT_UID);
}

// ----- Failure: issuer/subject ordering -------------------------------------

#[test]
fn subject_unique_id_before_issuer_unique_id_is_rejected() {
    // Both field contents are legal; only their order is wrong (v2, no
    // extensions involved).
    let tail = concat(&[&subject_uid(&[0x00]), &issuer_uid(&[0x00])]);
    let cert = build_cert(Some(1), &tail);
    assert_reversed_order(&run_inspect("uid-order-v2", &cert));
}

#[test]
fn subject_unique_id_before_issuer_unique_id_with_extensions_is_rejected() {
    // The same reversed pair in a v3 certificate ahead of a legal extensions
    // field is still a structural error.
    let tail = concat(&[
        &subject_uid(&[0x00, 0xBB]),
        &issuer_uid(&[0x00, 0xAA]),
        &extensions_field(),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_reversed_order(&run_inspect("uid-order-v3-ext", &cert));
}

#[test]
fn unique_ids_stay_rejected_when_duplicate_and_order_violations_coexist() {
    // subject, issuer, subject: parsing must fail on the ordering violation
    // (the issuer appears after the subject) — either way the certificate is
    // rejected with the unique-ID fields named, never displayed.
    let tail = concat(&[
        &subject_uid(&[0x00]),
        &issuer_uid(&[0x00]),
        &subject_uid(&[0x00]),
    ]);
    let cert = build_cert(Some(2), &tail);
    let out = run_inspect("uid-order-and-dup", &cert);
    assert_invalid_certificate(&out);
    let reason = reason(&out);
    assert!(
        reason.contains("issueruniqueid") && reason.contains("subjectuniqueid"),
        "reason must name both unique-ID fields: {reason}"
    );
}

// ----- Failure: unique ID after the extensions field ------------------------

#[test]
fn issuer_unique_id_after_extensions_is_rejected() {
    let tail = concat(&[&extensions_field(), &issuer_uid(&[0x00])]);
    let cert = build_cert(Some(2), &tail);
    assert_field_after_extensions(&run_inspect("issuer-uid-after-ext", &cert), ISSUER_UID);
}

#[test]
fn subject_unique_id_after_extensions_is_rejected() {
    let tail = concat(&[&extensions_field(), &subject_uid(&[0x00])]);
    let cert = build_cert(Some(2), &tail);
    assert_field_after_extensions(&run_inspect("subject-uid-after-ext", &cert), SUBJECT_UID);
}

#[test]
fn both_unique_ids_after_extensions_are_rejected() {
    // Even in their mutual correct order, both IDs are illegal once [3] has
    // already closed the optional tail.
    let tail = concat(&[
        &extensions_field(),
        &issuer_uid(&[0x00]),
        &subject_uid(&[0x00]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_field_after_extensions(&run_inspect("both-uids-after-ext", &cert), ISSUER_UID);
}

#[test]
fn one_unique_id_before_and_one_after_extensions_is_rejected() {
    // A legal issuerUniqueID ahead of [3] must not let a trailing
    // subjectUniqueID slip through behind the extensions field.
    let tail = concat(&[
        &issuer_uid(&[0x00]),
        &extensions_field(),
        &subject_uid(&[0x00]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_field_after_extensions(
        &run_inspect("uid-before-and-after-ext", &cert),
        SUBJECT_UID,
    );
}

// ----- Failure: version rules -----------------------------------------------

#[test]
fn v1_certificate_without_version_field_carrying_unique_ids_is_rejected() {
    // No explicit [0] version means v1; unique IDs are v2/v3-only.
    let cert = build_cert(None, &issuer_uid(&[0x00]));
    assert_v1_rejected(&run_inspect("v1-issuer-uid", &cert), ISSUER_UID);

    let cert = build_cert(None, &subject_uid(&[0x00]));
    assert_v1_rejected(&run_inspect("v1-subject-uid", &cert), SUBJECT_UID);
}

#[test]
fn explicit_v1_certificate_carrying_unique_ids_is_rejected() {
    let tail = concat(&[&issuer_uid(&[0x00]), &subject_uid(&[0x00])]);
    let cert = build_cert(Some(0), &tail);
    assert_v1_rejected(&run_inspect("v1explicit-uids", &cert), ISSUER_UID);
}

// ----- Failure: field shape and BIT STRING content --------------------------

#[test]
fn constructed_explicit_wrapper_for_unique_ids_is_rejected() {
    // [1]/[2] are IMPLICIT BIT STRINGs: a constructed 0xA1/0xA2 tag is an
    // explicit wrapper, not the legal primitive encoding, even though it wraps
    // a syntactically valid BIT STRING.
    let wrapped_bit_string = tlv(0x03, &[0x00, 0xFF]);
    let cert = build_cert(Some(2), &tlv(0xA1, &wrapped_bit_string));
    let out = run_inspect("issuer-uid-explicit-tag", &cert);
    assert_invalid_certificate(&out);
    assert!(
        reason(&out).contains("issueruniqueid"),
        "reason must name issuerUniqueID: {}",
        reason(&out)
    );

    let cert = build_cert(Some(2), &tlv(0xA2, &wrapped_bit_string));
    let out = run_inspect("subject-uid-explicit-tag", &cert);
    assert_invalid_certificate(&out);
    assert!(
        reason(&out).contains("subjectuniqueid"),
        "reason must name subjectUniqueID: {}",
        reason(&out)
    );
}

#[test]
fn unique_id_with_invalid_bit_string_content_is_rejected() {
    // Unused-bits count 8 and set bits among the declared unused bits: the
    // field exists in the right place but its BIT STRING content is corrupt.
    let cert = build_cert(Some(2), &issuer_uid(&[0x08]));
    let out = run_inspect("issuer-uid-bad-unused-count", &cert);
    assert_invalid_bit_string(&out, ISSUER_UID);

    let cert = build_cert(Some(2), &subject_uid(&[0x03, 0x0F]));
    let out = run_inspect("subject-uid-set-unused-bits", &cert);
    assert_invalid_bit_string(&out, SUBJECT_UID);
}

#[test]
fn unique_id_with_empty_bit_string_content_is_rejected() {
    // An implicit BIT STRING must still carry the leading unused-bits byte.
    let cert = build_cert(Some(2), &tlv(0x82, &[]));
    let out = run_inspect("subject-uid-empty", &cert);
    assert_invalid_bit_string(&out, SUBJECT_UID);
}

// ----- Assertions / process plumbing ----------------------------------------

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
}

/// Lower-cased reason text after the shared error prefix.
fn reason(out: &Output) -> String {
    let stderr = lossy(&out.stderr);
    const PREFIX: &str = "chainview: invalid DER certificate: ";
    stderr.trim_end().strip_prefix(PREFIX).unwrap().to_ascii_lowercase()
}

fn assert_duplicate_field(out: &Output, field: &str) {
    assert_invalid_certificate(out);
    let reason = reason(out);
    assert!(
        reason.contains("duplicate"),
        "reason must call the field a duplicate: {reason}"
    );
    assert!(
        reason.contains(&field.to_ascii_lowercase()),
        "reason must name the duplicated field {field}: {reason}"
    );
}

fn assert_reversed_order(out: &Output) {
    assert_invalid_certificate(out);
    let reason = reason(out);
    assert!(
        reason.contains("before"),
        "reason must describe the required order: {reason}"
    );
    assert!(
        reason.contains("issueruniqueid") && reason.contains("subjectuniqueid"),
        "reason must name both unique-ID fields: {reason}"
    );
}

fn assert_field_after_extensions(out: &Output, field: &str) {
    assert_invalid_certificate(out);
    let reason = reason(out);
    assert!(
        reason.contains(&field.to_ascii_lowercase()),
        "reason must name the offending field {field}: {reason}"
    );
    assert!(
        reason.contains("extension"),
        "reason must locate the field relative to the extensions field: {reason}"
    );
}

fn assert_v1_rejected(out: &Output, field: &str) {
    assert_invalid_certificate(out);
    let reason = reason(out);
    assert!(
        reason.contains(&field.to_ascii_lowercase()),
        "reason must name the offending field {field}: {reason}"
    );
    assert!(
        reason.contains("v2 or v3"),
        "reason must state unique IDs are v2/v3-only: {reason}"
    );
}

fn assert_invalid_bit_string(out: &Output, field: &str) {
    assert_invalid_certificate(out);
    let reason = reason(out);
    assert!(
        reason.contains(&field.to_ascii_lowercase()),
        "reason must name the offending field {field}: {reason}"
    );
    assert!(
        reason.contains("bit string"),
        "reason must locate the failure in the field's BIT STRING content: {reason}"
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

// ----- Certificate / DER construction ---------------------------------------

fn issuer_uid(content: &[u8]) -> Vec<u8> {
    tlv(0x81, content)
}

fn subject_uid(content: &[u8]) -> Vec<u8> {
    tlv(0x82, content)
}

/// One well-formed `[3] EXPLICIT` extensions field holding a single unknown
/// extension; its contents are irrelevant to the unique-ID ordering tests.
fn extensions_field() -> Vec<u8> {
    let ext = seq(&concat(&[&oid(&[1, 2, 3, 4]), &tlv(0x04, &[0x00])]));
    tlv(0xA3, &tlv(0x30, &ext))
}

fn explicit_version(value: u8) -> Vec<u8> {
    tlv(0xA0, &tlv(0x02, &[value]))
}

/// Assemble a complete certificate with fixed visible fields around the
/// optional explicit version (`None` = v1 with no version field) and an
/// arbitrary, already-encoded TBS tail (the unique-ID/extensions fields).
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
        tbs_content.extend_from_slice(&explicit_version(v));
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
    seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 11]), &tlv(0x05, &[])]))
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
