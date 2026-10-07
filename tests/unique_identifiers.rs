//! End-to-end regression tests for the two optional unique-identifier fields
//! of an X.509 tbsCertificate when running `chainview inspect <DER-FILE>`:
//!
//! ```text
//! issuerUniqueID  [1] IMPLICIT BIT STRING  (v2/v3 only, at most once)
//! subjectUniqueID [2] IMPLICIT BIT STRING  (v2/v3 only, at most once)
//! extensions      [3] EXPLICIT             (v3 only, always last)
//! ```
//!
//! Neither field is ever displayed, but its presence count and its position
//! are part of the certificate structure, so an otherwise decodable
//! certificate whose subject, issuer, serial number and validity all read
//! fine must still be rejected wholesale when:
//!
//! - the same unique ID occurs twice (identical or different content, adjacent
//!   or with the other unique ID in between) — one value is never picked;
//! - subjectUniqueID precedes issuerUniqueID;
//! - either unique ID follows the extensions field, which must be last;
//! - a v1 certificate (implicit or explicit) carries either field;
//! - the field is not an implicitly tagged primitive BIT STRING, or its BIT
//!   STRING content is not itself valid DER.
//!
//! Legal placements do not change the five fixed output lines, their order or
//! their wording. Every failure is exit code 2, completely empty stdout and a
//! `chainview: invalid DER certificate: <reason>` stderr whose reason names
//! the offending unique-ID field, so structural damage is distinguishable
//! from a plain file-read failure (`chainview: cannot read certificate file:`).
//!
//! Certificates are built with the tiny zero-dependency DER constructor at the
//! bottom of this file so individual field orderings can be crafted byte for
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

// An unremarkable private extension OID the tool does not recognize.
const UNKNOWN_OID: &[u64] = &[1, 2, 3, 4];

// ----- Success: legal presence and legal ordering ---------------------------

#[test]
fn v2_certificate_with_issuer_unique_id_only_is_accepted() {
    // v2 introduced the unique IDs; either one alone is optional.
    let tail = issuer_uid(&[0x00, 0x01, 0x02]);
    let cert = build_cert(Some(1), &tail);
    assert_success(&run_inspect("v2-issuer-only", &cert));
}

#[test]
fn v2_certificate_with_subject_unique_id_only_is_accepted() {
    let tail = subject_uid(&[0x00, 0x03, 0x04]);
    let cert = build_cert(Some(1), &tail);
    assert_success(&run_inspect("v2-subject-only", &cert));
}

#[test]
fn v2_certificate_with_both_unique_ids_in_order_is_accepted() {
    // issuerUniqueID [1] must precede subjectUniqueID [2].
    let tail = concat(&[
        &issuer_uid(&[0x00, 0xAA]),
        &subject_uid(&[0x00, 0xBB]),
    ]);
    let cert = build_cert(Some(1), &tail);
    assert_success(&run_inspect("v2-both", &cert));
}

#[test]
fn v3_certificate_with_either_unique_id_alone_is_accepted() {
    let cert = build_cert(Some(2), &issuer_uid(&[0x00]));
    assert_success(&run_inspect("v3-issuer-only", &cert));

    let cert = build_cert(Some(2), &subject_uid(&[0x00]));
    assert_success(&run_inspect("v3-subject-only", &cert));
}

#[test]
fn v3_certificate_with_both_unique_ids_and_no_extensions_is_accepted() {
    let tail = concat(&[&issuer_uid(&[0x00, 0x01]), &subject_uid(&[0x00, 0x02])]);
    let cert = build_cert(Some(2), &tail);
    assert_success(&run_inspect("v3-both-no-ext", &cert));
}

#[test]
fn v3_unique_ids_before_extensions_are_accepted() {
    // The full legal tail: issuer, subject, then extensions last.
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let tail = concat(&[
        &issuer_uid(&[0x03, 0xFF, 0xF8]),
        &subject_uid(&[0x00, 0x7E]),
        &extensions_field(&ext),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_success(&run_inspect("v3-both-before-ext", &cert));
}

#[test]
fn v3_one_unique_id_before_extensions_is_accepted_in_either_combination() {
    let ext = extension(UNKNOWN_OID, None, &[]);
    let fields = extensions_field(&ext);

    let tail = concat(&[&issuer_uid(&[0x00]), &fields]);
    assert_success(&run_inspect("v3-issuer-before-ext", &build_cert(Some(2), &tail)));

    let tail = concat(&[&subject_uid(&[0x00]), &fields]);
    assert_success(&run_inspect("v3-subject-before-ext", &build_cert(Some(2), &tail)));
}

#[test]
fn legal_unique_id_bit_string_contents_are_accepted() {
    // Empty bit string, plain data, and data whose declared unused trailing
    // bits are actually zero all pass; the bytes are never displayed.
    for (tag, body) in [
        (0x81u8, vec![0x00]),
        (0x81, vec![0x00, 0xFF, 0xFF]),
        (0x82, vec![0x07, 0x80]),
        (0x82, vec![0x01, 0xFE]),
    ] {
        let tail = tlv(tag, &body);
        let cert = build_cert(Some(2), &tail);
        assert_success(&run_inspect("v3-uid-content", &cert));
    }
}

#[test]
fn unique_ids_do_not_change_the_displayed_fields() {
    // The exact five lines, order and wording survive a full optional tail;
    // names, serial and dates are interpreted exactly as without the fields.
    let ext = extension(UNKNOWN_OID, Some(true), b"ignored");
    let tail = concat(&[
        &issuer_uid(&[0x00, 0xDE, 0xAD]),
        &subject_uid(&[0x00, 0xBE, 0xEF]),
        &extensions_field(&ext),
    ]);
    let out = run_inspect("v3-display-unchanged", &build_cert(Some(2), &tail));
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

// ----- Failure: duplicate unique-ID fields -----------------------------------

#[test]
fn duplicate_issuer_unique_id_with_identical_value_is_rejected() {
    // Same field, same bytes: still two occurrences, and neither value is
    // picked to salvage the view.
    let uid = issuer_uid(&[0x00, 0xAA]);
    let cert = build_cert(Some(1), &concat(&[&uid, &uid]));
    assert_duplicate_field(&run_inspect("dup-issuer-same", &cert), "issuerUniqueID");
}

#[test]
fn duplicate_issuer_unique_id_with_different_values_is_rejected() {
    // Same field, different contents: identity is the field, not its value.
    let tail = concat(&[
        &issuer_uid(&[0x00, 0xAA]),
        &issuer_uid(&[0x00, 0xBB, 0xCC]),
    ]);
    let cert = build_cert(Some(1), &tail);
    assert_duplicate_field(&run_inspect("dup-issuer-diff", &cert), "issuerUniqueID");
}

#[test]
fn duplicate_subject_unique_id_with_different_values_is_rejected() {
    let tail = concat(&[
        &subject_uid(&[0x00, 0x01]),
        &subject_uid(&[0x03, 0xFF, 0xF8]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_duplicate_field(&run_inspect("dup-subject-diff", &cert), "subjectUniqueID");
}

#[test]
fn duplicate_issuer_unique_id_separated_by_subject_unique_id_is_rejected() {
    // issuer, subject, issuer: the repeat is not adjacent, so a check that
    // only compared neighbors would miss it. The whole certificate fails and
    // is never viewed with one of the issuer IDs chosen.
    let tail = concat(&[
        &issuer_uid(&[0x00, 0xAA]),
        &subject_uid(&[0x00, 0xBB]),
        &issuer_uid(&[0x00, 0xCC]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_duplicate_field(&run_inspect("dup-issuer-split", &cert), "issuerUniqueID");
}

#[test]
fn duplicate_unique_ids_with_extensions_still_present_are_rejected() {
    // A well-formed extensions list must not mask the repeated field before it.
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let tail = concat(&[
        &subject_uid(&[0x00, 0x01]),
        &subject_uid(&[0x00, 0x02]),
        &extensions_field(&ext),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_duplicate_field(&run_inspect("dup-subject-with-ext", &cert), "subjectUniqueID");
}

// ----- Failure: relative ordering of the two fields --------------------------

#[test]
fn subject_unique_id_before_issuer_unique_id_is_rejected() {
    // [2] then [1] is the reversed order even though both tags are otherwise
    // legal; valid BIT STRING contents cannot rescue the arrangement.
    let tail = concat(&[
        &subject_uid(&[0x00, 0xBB]),
        &issuer_uid(&[0x00, 0xAA]),
    ]);
    let cert = build_cert(Some(1), &tail);
    assert_ordering_error(&run_inspect("swap-no-ext", &cert));
}

#[test]
fn swapped_unique_ids_followed_by_extensions_are_rejected() {
    // The swap is structural damage regardless of a legal extensions field.
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let tail = concat(&[
        &subject_uid(&[0x00, 0xBB]),
        &issuer_uid(&[0x00, 0xAA]),
        &extensions_field(&ext),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_ordering_error(&run_inspect("swap-with-ext", &cert));
}

// ----- Failure: unique IDs after the extensions field ------------------------

#[test]
fn issuer_unique_id_after_extensions_is_rejected() {
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let tail = concat(&[&extensions_field(&ext), &issuer_uid(&[0x00, 0xAA])]);
    let cert = build_cert(Some(2), &tail);
    assert_field_after_extensions(&run_inspect("issuer-after-ext", &cert), "issuerUniqueID");
}

#[test]
fn subject_unique_id_after_extensions_is_rejected() {
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let tail = concat(&[&extensions_field(&ext), &subject_uid(&[0x00, 0xBB])]);
    let cert = build_cert(Some(2), &tail);
    assert_field_after_extensions(&run_inspect("subject-after-ext", &cert), "subjectUniqueID");
}

#[test]
fn subject_unique_id_between_extensions_and_nothing_is_rejected() {
    // Issuer present in its legal place; only subject lands after extensions:
    // a partially correct tail is still a corrupt certificate.
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let tail = concat(&[
        &issuer_uid(&[0x00, 0xAA]),
        &extensions_field(&ext),
        &subject_uid(&[0x00, 0xBB]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_field_after_extensions(&run_inspect("issuer-ok-subject-after", &cert), "subjectUniqueID");
}

#[test]
fn both_unique_ids_after_extensions_are_rejected() {
    // Even an in-order pair is illegal once extensions has already appeared.
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let tail = concat(&[
        &extensions_field(&ext),
        &issuer_uid(&[0x00, 0xAA]),
        &subject_uid(&[0x00, 0xBB]),
    ]);
    let cert = build_cert(Some(2), &tail);
    assert_field_after_extensions(&run_inspect("both-after-ext", &cert), "issuerUniqueID");
}

// ----- Failure: version rules ------------------------------------------------

#[test]
fn implicit_v1_certificate_with_unique_id_is_rejected() {
    // No [0] version field means v1; neither unique ID exists in v1.
    let cert = build_cert(None, &issuer_uid(&[0x00]));
    assert_v1_error(&run_inspect("v1-implicit-issuer", &cert), "issuerUniqueID");

    let cert = build_cert(None, &subject_uid(&[0x00]));
    assert_v1_error(&run_inspect("v1-implicit-subject", &cert), "subjectUniqueID");
}

#[test]
fn explicit_v1_certificate_with_unique_id_is_rejected() {
    let cert = build_cert(Some(0), &issuer_uid(&[0x00, 0x01]));
    assert_v1_error(&run_inspect("v1-explicit", &cert), "issuerUniqueID");
}

// ----- Failure: tag shape and BIT STRING content -----------------------------

#[test]
fn constructed_tag_wrappers_for_unique_ids_are_rejected() {
    // [1]/[2] are IMPLICIT tags over a primitive BIT STRING; a constructed
    // 0xA1/0xA2 wrapper (what an EXPLICIT tag would look like) is a different
    // field encoding and must not be accepted as the unique ID.
    let cert = build_cert(Some(2), &tlv(0xA1, &tlv(0x03, &[0x00])));
    assert_mentions_field(&run_inspect("constructed-a1", &cert), "issuerUniqueID");

    let cert = build_cert(Some(2), &tlv(0xA2, &tlv(0x03, &[0x00])));
    assert_mentions_field(&run_inspect("constructed-a2", &cert), "subjectUniqueID");
}

#[test]
fn unique_id_with_empty_bit_string_content_is_rejected() {
    // The implicit content IS the BIT STRING content; the leading unused-bits
    // byte cannot be missing.
    let cert = build_cert(Some(1), &issuer_uid(&[]));
    assert_mentions_field(&run_inspect("uid-empty-content", &cert), "issuerUniqueID");
}

#[test]
fn unique_id_with_unused_bits_count_above_seven_is_rejected() {
    let cert = build_cert(Some(2), &subject_uid(&[0x08, 0x00]));
    assert_mentions_field(&run_inspect("uid-unused-8", &cert), "subjectUniqueID");
}

#[test]
fn unique_id_with_set_unused_trailing_bits_is_rejected() {
    // Three unused bits declared, lowest three bits of the last byte set:
    // invalid BIT STRING content even though the field itself is well placed.
    let cert = build_cert(Some(2), &issuer_uid(&[0x03, 0xFF, 0x07]));
    assert_mentions_field(&run_inspect("uid-stray-bits", &cert), "issuerUniqueID");
}

#[test]
fn truncated_unique_id_field_is_rejected() {
    // The field announces five content bytes and supplies one; correct outer
    // lengths must not hide the truncation, and the error still locates the
    // issuerUniqueID field.
    let broken = raw_tlv(0x81, &[0x05], &[0x00]);
    let cert = build_cert(Some(2), &broken);
    assert_mentions_field(&run_inspect("uid-truncated", &cert), "issuerUniqueID");
}

// ----- Failure reporting: structural damage vs. unreadable file --------------

#[test]
fn missing_file_is_a_read_error_not_an_invalid_certificate() {
    // The two failure classes stay distinguishable by their prefixes: a
    // structurally bad unique-ID field is an invalid certificate; a file that
    // cannot be opened at all is a read error.
    let path = std::env::temp_dir().join(format!(
        "chainview-uid-test-{}-does-not-exist.der",
        std::process::id()
    ));
    let out = Command::new(env!("CARGO_BIN_EXE_chainview"))
        .arg("inspect")
        .arg(&path)
        .output()
        .expect("running chainview");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.starts_with(&format!(
            "chainview: cannot read certificate file '{}': ",
            path.display()
        )),
        "unexpected stderr: {stderr}"
    );
    assert!(
        !stderr.contains("invalid DER certificate"),
        "a missing file must not be reported as a corrupt certificate: {stderr}"
    );
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

/// Shared failure shape for every malformed unique-ID certificate: exit 2, no
/// stdout at all, and the invalid-DER prefix.
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

/// The reason must name the offending field so the user can locate it.
fn assert_mentions_field(out: &Output, field: &str) {
    assert_invalid_certificate(out);
    let reason = lossy(&out.stderr);
    assert!(
        reason.contains(field),
        "reason must name {field}: {reason}"
    );
}

/// A repeated field: the reason must say it is a duplicate and name it.
fn assert_duplicate_field(out: &Output, field: &str) {
    assert_invalid_certificate(out);
    let reason = lossy(&out.stderr);
    assert!(
        reason.to_ascii_lowercase().contains("duplicate"),
        "reason must identify a duplicate field: {reason}"
    );
    assert!(
        reason.contains(field),
        "reason must name the duplicated {field}: {reason}"
    );
}

/// [2] before [1]: the reason has to describe the issuer/subject ordering, so
/// merely mentioning "extension" would not satisfy this assertion.
fn assert_ordering_error(out: &Output) {
    assert_invalid_certificate(out);
    let reason = lossy(&out.stderr);
    assert!(
        reason.contains("issuerUniqueID") && reason.contains("subjectUniqueID"),
        "reason must name both unique IDs and their required order: {reason}"
    );
}

/// A unique ID trailing the extensions field: the reason must name the field
/// and state that extensions must precede it / be last.
fn assert_field_after_extensions(out: &Output, field: &str) {
    assert_invalid_certificate(out);
    let reason = lossy(&out.stderr);
    assert!(
        reason.contains(field),
        "reason must name the offending {field}: {reason}"
    );
    assert!(
        reason.to_ascii_lowercase().contains("extension"),
        "reason must locate the field relative to the extensions field: {reason}"
    );
}

/// v1 carrying a unique ID: the reason names both the field and the version.
fn assert_v1_error(out: &Output, field: &str) {
    assert_invalid_certificate(out);
    let reason = lossy(&out.stderr);
    assert!(
        reason.contains(field),
        "reason must name the offending {field}: {reason}"
    );
    assert!(
        reason.contains("v1"),
        "reason must state that unique IDs are not allowed in v1: {reason}"
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

/// issuerUniqueID [1] IMPLICIT BIT STRING: the supplied bytes are the BIT
/// STRING content (leading unused-bits byte followed by the bit data).
fn issuer_uid(bit_string_content: &[u8]) -> Vec<u8> {
    tlv(0x81, bit_string_content)
}

/// subjectUniqueID [2] IMPLICIT BIT STRING, same content rules.
fn subject_uid(bit_string_content: &[u8]) -> Vec<u8> {
    tlv(0x82, bit_string_content)
}

/// [3] EXPLICIT wrapping one Extensions SEQUENCE holding the supplied items.
fn extensions_field(extension_items: &[u8]) -> Vec<u8> {
    tlv(0xA3, &tlv(0x30, extension_items))
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

fn explicit_version(value: u8) -> Vec<u8> {
    tlv(0xA0, &tlv(0x02, &[value]))
}

/// Assemble a complete certificate with fixed visible fields around the
/// optional explicit version (`None` = v1 with no version field) and an
/// arbitrary, already-encoded TBS tail (unique IDs and/or extensions).
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
