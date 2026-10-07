//! End-to-end regression tests for X.509 extensions decoding when running
//! `chainview inspect <DER-FILE>`.
//!
//! The extensions field never appears among the five displayed fields, but it
//! must be read in full: inspect used to skip the interior of the `[3]`
//! wrapper as long as its outer length was intact, so a corrupted extension
//! still printed the certificate. These tests pin the fixed contract:
//!
//! - the field is OPTIONAL and, when present, legal only in an explicit v3
//!   certificate (v1 without a version, explicit v1 and v2 must reject it);
//! - it may occur at most once; the `[3] EXPLICIT` wrapper contains exactly
//!   one non-empty SEQUENCE of extensions;
//! - every Extension is a SEQUENCE of OID, optional BOOLEAN, OCTET STRING in
//!   that order with nothing appended; the OID must be complete and encoded in
//!   minimal form;
//! - `critical` omitted means FALSE; an explicit BOOLEAN is accepted only as
//!   the DER encoding of TRUE (content exactly FF). Explicit FALSE, other
//!   nonzero bytes, empty content and multi-byte contents are all rejected;
//! - unknown but legal extension OIDs, critical TRUE and empty OCTET STRING
//!   values are all accepted; extension bytes are never interpreted.
//! - every extension OID may appear at most once in the list: identity is the
//!   complete OID alone, so identical entries or the same OID with different
//!   values/critical flags, adjacent or separated, are all rejected with the
//!   full dotted OID named; distinct OIDs with identical bodies still coexist.
//!
//! Every case drives the real binary: success prints exactly the five fixed
//! fields (no extension summary is added); failure is exit code 2, completely
//! empty stdout and a `chainview: invalid DER certificate: <reason>` stderr
//! whose reason locates the problem in the extensions field.

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

// ----- Success: legal encodings ---------------------------------------------

#[test]
fn v3_certificate_without_extensions_is_accepted() {
    // Explicit v3 with no [3] field is perfectly legal.
    let cert = build_cert(Some(2), &[]);
    assert_success(&run_inspect("v3-no-ext", &cert));
}

#[test]
fn v3_certificate_with_one_unknown_extension_is_accepted() {
    // Unknown extension types need no allow-list match: a complete,
    // minimal-form OID with any OCTET STRING body is accepted.
    let ext = extension(UNKNOWN_OID, None, &[0x30, 0x00]);
    let cert = build_cert(Some(2), &extensions_field(&ext));
    assert_success(&run_inspect("v3-one-ext", &cert));
}

#[test]
fn v3_certificate_with_critical_extension_is_accepted() {
    // critical TRUE (01 01 FF) never causes inspect to fail.
    let ext = extension(UNKNOWN_OID, Some(true), b"data");
    let cert = build_cert(Some(2), &extensions_field(&ext));
    assert_success(&run_inspect("v3-critical", &cert));
}

#[test]
fn v3_certificate_with_empty_extension_octet_string_is_accepted() {
    // The extnValue OCTET STRING may carry zero bytes; nothing inside it is
    // interpreted.
    let ext = extension(UNKNOWN_OID, None, &[]);
    let cert = build_cert(Some(2), &extensions_field(&ext));
    assert_success(&run_inspect("v3-empty-octets", &cert));
}

#[test]
fn v3_certificate_with_multiple_extensions_is_accepted() {
    // A non-empty SEQUENCE may hold many extensions, mixing critical and
    // non-critical and using multi-byte OID arcs.
    let a = extension(&[1, 2, 840, 99999, 7], Some(true), &[0x01]);
    let b = extension(&[2, 5, 29, 14], None, &[0x04, 0x02, 0xAA, 0xBB]);
    let c = extension(UNKNOWN_OID, None, &[]);
    let cert = build_cert(Some(2), &extensions_field(&concat(&[&a, &b, &c])));
    assert_success(&run_inspect("v3-multi-ext", &cert));
}

#[test]
fn v3_extensions_coexist_with_unique_id_fields() {
    // issuerUniqueID [1] / subjectUniqueID [2] sit in the same optional tail;
    // they must not be mistaken for the [3] extensions field.
    let issuer_uid = raw_tlv(0x81, &[0x03], &[0x00, 0xFF, 0xFF]);
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let tail = concat(&[&issuer_uid, &extensions_field(&ext)]);
    let cert = build_cert(Some(2), &tail);
    assert_success(&run_inspect("v3-unique-id", &cert));
}

#[test]
fn expired_or_future_certificate_with_extensions_is_still_displayed() {
    // Validity is not judged: extensions decode the same on an already
    // expired and a not-yet-valid certificate. The two certs intentionally
    // differ from the fixed validity dates, so their times are checked
    // directly rather than via assert_success.
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let field = extensions_field(&ext);

    let expired = build_cert_times(Some(2), &field, b"200115093000Z", b"210115093000Z");
    assert_ok_fields(
        &run_inspect("ext-expired", &expired),
        "2020-01-15T09:30:00Z",
        "2021-01-15T09:30:00Z",
    );

    let future = build_cert_times(Some(2), &field, b"280115093000Z", b"290115093000Z");
    assert_ok_fields(
        &run_inspect("ext-future", &future),
        "2028-01-15T09:30:00Z",
        "2029-01-15T09:30:00Z",
    );
}

// ----- Failure: version rules -----------------------------------------------

#[test]
fn v1_certificate_without_version_carrying_extensions_is_rejected() {
    // No [0] version field means v1; v1 cannot carry extensions.
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let cert = build_cert(None, &extensions_field(&ext));
    assert_invalid_certificate(&run_inspect("v1-ext", &cert));
}

#[test]
fn explicit_v1_certificate_carrying_extensions_is_rejected() {
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let cert = build_cert(Some(0), &extensions_field(&ext));
    assert_invalid_certificate(&run_inspect("v1explicit-ext", &cert));
}

#[test]
fn explicit_v2_certificate_carrying_extensions_is_rejected() {
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let cert = build_cert(Some(1), &extensions_field(&ext));
    assert_invalid_certificate(&run_inspect("v2-ext", &cert));
}

#[test]
fn duplicate_extensions_field_is_rejected() {
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let field = extensions_field(&ext);
    let cert = build_cert(Some(2), &concat(&[&field, &field]));
    assert_invalid_certificate(&run_inspect("dup-ext", &cert));
}

// ----- Failure: [3] wrapper contents ----------------------------------------

#[test]
fn empty_extensions_wrapper_is_rejected() {
    // [3] EXPLICIT with no content at all.
    let cert = build_cert(Some(2), &tlv(0xA3, &[]));
    assert_invalid_certificate(&run_inspect("wrap-empty", &cert));
}

#[test]
fn extensions_wrapper_without_sequence_is_rejected() {
    // The single wrapped element must be a SEQUENCE, even if some other
    // complete TLV is present.
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x02, &[0x01, 0x00])));
    assert_invalid_certificate(&run_inspect("wrap-no-seq", &cert));
}

#[test]
fn extensions_wrapper_with_two_sequences_is_rejected() {
    // Exactly one Extensions SEQUENCE belongs inside the wrapper.
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let list = tlv(0x30, &ext);
    let cert = build_cert(Some(2), &tlv(0xA3, &concat(&[&list, &list])));
    assert_invalid_certificate(&run_inspect("wrap-two-seq", &cert));
}

#[test]
fn extensions_wrapper_with_trailing_element_is_rejected() {
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let inner = concat(&[&tlv(0x30, &ext), &tlv(0x02, &[0x01, 0x2A])]);
    let cert = build_cert(Some(2), &tlv(0xA3, &inner));
    assert_invalid_certificate(&run_inspect("wrap-trailing", &cert));
}

#[test]
fn empty_extensions_sequence_is_rejected() {
    // Extensions ::= SEQUENCE SIZE (1..MAX): zero extensions is invalid.
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &[])));
    assert_invalid_certificate(&run_inspect("ext-seq-empty", &cert));
}

#[test]
fn truncated_extensions_wrapper_is_rejected() {
    // The wrapper announces 8 content bytes but the TBS certificate provides
    // only 4; a correct outer length must not hide a truncated interior.
    let wrapper = raw_tlv(0xA3, &[0x08], &[0x30, 0x06, 0x06, 0x01]);
    let cert = build_cert(Some(2), &wrapper);
    assert_invalid_certificate(&run_inspect("wrap-truncated", &cert));
}

// ----- Failure: individual extension items ----------------------------------

#[test]
fn extension_item_that_is_not_a_sequence_is_rejected() {
    // The list holds one OCTET STRING where an Extension SEQUENCE must stand.
    let body = tlv(0x04, &[0x06, 0x01, 0x55]);
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("item-not-seq", &cert));
}

#[test]
fn truncated_extension_item_inside_a_sized_wrapper_is_rejected() {
    // Every enclosing length is correct; only the extension SEQUENCE lies
    // about its own length (announces 5 content bytes, provides 2).
    let broken_ext = raw_tlv(0x30, &[0x05], &[0x06, 0x01]);
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &broken_ext)));
    assert_invalid_certificate(&run_inspect("item-truncated", &cert));
}

#[test]
fn extension_missing_oid_is_rejected() {
    // SEQUENCE containing only the OCTET STRING.
    let body = tlv(0x30, &tlv(0x04, &[0x00]));
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("ext-no-oid", &cert));
}

#[test]
fn extension_missing_octet_string_is_rejected() {
    // SEQUENCE containing only the OID.
    let body = tlv(0x30, &oid(UNKNOWN_OID));
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("ext-no-octets", &cert));
}

#[test]
fn extension_with_oid_and_boolean_but_no_octet_string_is_rejected() {
    let body = tlv(
        0x30,
        &concat(&[&oid(UNKNOWN_OID), &tlv(0x01, &[0xFF])]),
    );
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("ext-bool-no-octets", &cert));
}

#[test]
fn extension_with_swapped_octet_string_and_oid_order_is_rejected() {
    let body = tlv(
        0x30,
        &concat(&[&tlv(0x04, &[0x00]), &oid(UNKNOWN_OID)]),
    );
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("ext-swap", &cert));
}

#[test]
fn extension_with_boolean_after_octet_string_is_rejected() {
    // OID, OCTET STRING, BOOLEAN: the BOOLEAN is trailing data, not a
    // recognized critical position.
    let body = tlv(
        0x30,
        &concat(&[
            &oid(UNKNOWN_OID),
            &tlv(0x04, &[0x00]),
            &tlv(0x01, &[0xFF]),
        ]),
    );
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("ext-bool-trailing", &cert));
}

#[test]
fn extension_with_two_booleans_is_rejected() {
    let body = tlv(
        0x30,
        &concat(&[
            &oid(UNKNOWN_OID),
            &tlv(0x01, &[0xFF]),
            &tlv(0x01, &[0xFF]),
            &tlv(0x04, &[0x00]),
        ]),
    );
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("ext-two-bools", &cert));
}

#[test]
fn extension_with_integer_in_critical_position_is_rejected() {
    // A non-BOOLEAN element between OID and OCTET STRING cannot be skipped.
    let body = tlv(
        0x30,
        &concat(&[
            &oid(UNKNOWN_OID),
            &tlv(0x02, &[0x01, 0x00]),
            &tlv(0x04, &[0x00]),
        ]),
    );
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("ext-int-pos", &cert));
}

#[test]
fn extension_with_appended_element_is_rejected() {
    let body = tlv(
        0x30,
        &concat(&[
            &oid(UNKNOWN_OID),
            &tlv(0x04, &[0x00]),
            &tlv(0x02, &[0x01, 0x2A]),
        ]),
    );
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("ext-extra", &cert));
}

// ----- Failure: critical BOOLEAN DER encoding -------------------------------

#[test]
fn extension_critical_explicit_false_is_rejected() {
    // DEFAULT FALSE must be omitted; 01 01 00 is never the DER encoding of
    // the default value.
    let body = tlv(
        0x30,
        &concat(&[
            &oid(UNKNOWN_OID),
            &tlv(0x01, &[0x00]),
            &tlv(0x04, &[0x00]),
        ]),
    );
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("crit-false", &cert));
}

#[test]
fn extension_critical_with_nonzero_nonff_byte_is_rejected() {
    // 0x01 and 0x7F are truthy BER BOOLEANs but not the DER value of TRUE.
    for (i, byte) in [0x01u8, 0x7F, 0xFE].into_iter().enumerate() {
        let body = tlv(
            0x30,
            &concat(&[
                &oid(UNKNOWN_OID),
                &tlv(0x01, &[byte]),
                &tlv(0x04, &[0x00]),
            ]),
        );
        let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
        assert_invalid_certificate(&run_inspect(&format!("crit-byte-{i}"), &cert));
    }
}

#[test]
fn extension_critical_with_empty_content_is_rejected() {
    // 01 00: a BOOLEAN must carry exactly one content byte.
    let body = tlv(
        0x30,
        &concat(&[
            &oid(UNKNOWN_OID),
            &tlv(0x01, &[]),
            &tlv(0x04, &[0x00]),
        ]),
    );
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("crit-empty", &cert));
}

#[test]
fn extension_critical_with_multiple_content_bytes_is_rejected() {
    // 01 02 FF FF: even though both bytes are FF, BOOLEAN is one byte.
    let body = tlv(
        0x30,
        &concat(&[
            &oid(UNKNOWN_OID),
            &tlv(0x01, &[0xFF, 0xFF]),
            &tlv(0x04, &[0x00]),
        ]),
    );
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("crit-multi", &cert));
}

// ----- Failure: duplicate extension types -----------------------------------

#[test]
fn adjacent_identical_extension_oid_is_rejected() {
    // The same private OID twice in a row, even with identical encodings, is
    // a corrupt certificate; the duplicate must not simply be dropped or one
    // entry picked.
    let ext = extension(UNKNOWN_OID, None, &[0x00]);
    let items = concat(&[&ext, &ext]);
    let cert = build_cert(Some(2), &extensions_field(&items));
    assert_duplicate_oid(&run_inspect("dup-oid-adjacent", &cert), "1.2.3.4");
}

#[test]
fn same_extension_oid_with_different_values_and_critical_is_rejected() {
    // Duplicate identity is the OID alone: a different extnValue and critical
    // flag must not make the second entry a distinct extension.
    let first = extension(UNKNOWN_OID, Some(true), b"one");
    let second = extension(UNKNOWN_OID, None, b"completely different");
    let items = concat(&[&first, &second]);
    let cert = build_cert(Some(2), &extensions_field(&items));
    assert_duplicate_oid(&run_inspect("dup-oid-diff-body", &cert), "1.2.3.4");
}

#[test]
fn duplicate_extension_oid_separated_by_other_extensions_is_rejected() {
    // The two 1.2.3.4 entries bracket legal, distinct extensions; neither
    // adjacency nor merging is involved.
    let a = extension(UNKNOWN_OID, None, b"a");
    let ski = extension(&[2, 5, 29, 14], None, &[0x04, 0x02, 0xAA, 0xBB]);
    let other = extension(&[1, 2, 840, 99999, 7], Some(true), &[0x01]);
    let dup = extension(UNKNOWN_OID, Some(true), b"b");
    let items = concat(&[&a, &ski, &other, &dup]);
    let cert = build_cert(Some(2), &extensions_field(&items));
    assert_duplicate_oid(&run_inspect("dup-oid-split", &cert), "1.2.3.4");
}

#[test]
fn duplicate_oid_with_multibyte_arcs_is_rejected_with_full_dotted_form() {
    // Unknown private OID whose arcs use multi-byte base-128 encodings: the
    // error must render every arc of the full dotted-decimal OID.
    let long_oid: &[u64] = &[1, 2, 840, 113549, 1, 999999999999];
    let first = extension(long_oid, None, &[0x01]);
    let second = extension(long_oid, Some(true), &[0x02, 0x03]);
    let items = concat(&[&first, &second]);
    let cert = build_cert(Some(2), &extensions_field(&items));
    assert_duplicate_oid(
        &run_inspect("dup-oid-multibyte", &cert),
        "1.2.840.113549.1.999999999999",
    );
}

#[test]
fn duplicate_known_extension_oid_is_rejected() {
    // The rule applies to recognized extension types too.
    let first = extension(&[2, 5, 29, 14], None, &[0x04, 0x00]);
    let second = extension(&[2, 5, 29, 14], None, &[0x04, 0x02, 0xAA, 0xBB]);
    let items = concat(&[&first, &second]);
    let cert = build_cert(Some(2), &extensions_field(&items));
    assert_duplicate_oid(&run_inspect("dup-oid-ski", &cert), "2.5.29.14");
}

// ----- Success: distinct extension types ------------------------------------

#[test]
fn different_extension_oids_with_identical_body_and_critical_are_accepted() {
    // Same value and critical flag never make two distinct OIDs a duplicate.
    let a = extension(&[1, 2, 3, 4], Some(true), b"same");
    let b = extension(&[1, 2, 3, 5], Some(true), b"same");
    let c = extension(&[2, 5, 29, 14], Some(true), b"same");
    let items = concat(&[&a, &b, &c]);
    let cert = build_cert(Some(2), &extensions_field(&items));
    assert_success(&run_inspect("distinct-oid-same-body", &cert));
}

// ----- Arcs wider than any machine integer ---------------------------------
//
// Extension OID identity and legality never depend on a machine integer
// width: an arc past u64::MAX must parse, and the complete dotted-decimal
// value (not a truncation) decides duplicates. Raw OID content bytes are used
// because the test DER builder works in u64. 2^64 encodes shortest-form as
// 82 80 80 80 80 80 80 80 80 00. Both OIDs below start 2.5.29 (first pair 85,
// then 29), then a 2^64 arc, then 1 or 2.

/// 2.5.29.18446744073709551616.1
const BIG_EXT_OID_1: &[u8] = &[
    0x55, 0x1D, 0x82, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00, 0x01,
];
/// 2.5.29.18446744073709551616.2 — differs from the above only in the final
/// digit, after the same oversized arc.
const BIG_EXT_OID_2: &[u8] = &[
    0x55, 0x1D, 0x82, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00, 0x02,
];

#[test]
fn duplicate_big_arc_extension_oid_is_rejected_with_full_dotted_form() {
    // The same big-arced OID twice, even with different values and critical
    // flags, is a duplicate: identity is the complete OID and the reason must
    // print the exact 2^64 arc rather than a truncated or wrapped value.
    let first = extension_raw_oid(BIG_EXT_OID_1, None, b"A");
    let second = extension_raw_oid(BIG_EXT_OID_1, Some(true), b"other");
    let items = concat(&[&first, &second]);
    let cert = build_cert(Some(2), &extensions_field(&items));
    assert_duplicate_oid(
        &run_inspect("dup-oid-big-arc", &cert),
        "2.5.29.18446744073709551616.1",
    );
}

#[test]
fn big_arc_oids_differing_only_in_last_digit_are_not_duplicates() {
    // Two huge OIDs that agree on the 2^64 arc but differ in the very last
    // digit are distinct complete OIDs; with identical bodies they must still
    // coexist rather than collapse into a false duplicate.
    let a = extension_raw_oid(BIG_EXT_OID_1, Some(true), b"same");
    let b = extension_raw_oid(BIG_EXT_OID_2, Some(true), b"same");
    let items = concat(&[&a, &b]);
    let cert = build_cert(Some(2), &extensions_field(&items));
    assert_success(&run_inspect("distinct-big-arc-oid", &cert));
}

#[test]
fn no_extensions_and_single_extension_remain_accepted() {
    // Zero extensions (no field at all) and one extension never collide.
    assert_success(&run_inspect("no-ext-field", &build_cert(Some(2), &[])));
    let ext = extension(UNKNOWN_OID, None, &[]);
    assert_success(&run_inspect("one-ext-field", &build_cert(Some(2), &extensions_field(&ext))));
}

// ----- Failure: extension OID encoding --------------------------------------

#[test]
fn extension_with_empty_oid_is_rejected() {
    let body = tlv(
        0x30,
        &concat(&[&tlv(0x06, &[]), &tlv(0x04, &[0x00])]),
    );
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("ext-oid-empty", &cert));
}

#[test]
fn extension_with_truncated_oid_is_rejected() {
    // 0x88 promises another base-128 byte that never arrives; the wrapper
    // lengths around it are all correct.
    let body = tlv(
        0x30,
        &concat(&[&tlv(0x06, &[0x88]), &tlv(0x04, &[0x00])]),
    );
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("ext-oid-trunc", &cert));
}

#[test]
fn extension_with_nonminimal_oid_is_rejected() {
    // Arc 1 as 0x80 0x01 is not the shortest base-128 encoding.
    let body = tlv(
        0x30,
        &concat(&[&tlv(0x06, &[0x80, 0x01]), &tlv(0x04, &[0x00])]),
    );
    let cert = build_cert(Some(2), &tlv(0xA3, &tlv(0x30, &body)));
    assert_invalid_certificate(&run_inspect("ext-oid-nonmin", &cert));
}

// ----- Assertions / process plumbing ---------------------------------------

fn assert_success(out: &Output) {
    assert_ok_fields(out, FIXED_NOT_BEFORE, FIXED_NOT_AFTER);
}

/// Success assertion for certificates whose validity differs from the fixed
/// fixture dates: still exit 0, empty stderr, the same five fields in order
/// and no extension-related sixth line.
fn assert_ok_fields(out: &Output, not_before: &str, not_after: &str) {
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
    let reason = &stderr[PREFIX.len()..];
    assert!(
        reason.to_ascii_lowercase().contains("extension"),
        "error reason must locate the failure in the extensions field: {reason}"
    );
}

/// Duplicate-extension-type failure: exit 2, empty stdout, and the reason
/// must say the extension type is duplicated and name the complete dotted
/// OID so the conflicting entries can be located.
fn assert_duplicate_oid(out: &Output, dotted_oid: &str) {
    assert_invalid_certificate(out);
    let stderr = lossy(&out.stderr);
    let reason = stderr.trim_end();
    assert!(
        reason.to_ascii_lowercase().contains("duplicate extension type"),
        "reason must identify a duplicate extension type: {reason}"
    );
    assert!(
        reason.contains(dotted_oid),
        "reason must give the full dotted OID {dotted_oid}: {reason}"
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

// ----- Certificate / DER construction --------------------------------------

/// [3] EXPLICIT wrapping one Extensions SEQUENCE that holds the supplied,
/// already-encoded extension items.
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

/// Like `extension`, but the extnID is given as raw OID content bytes so arcs
/// wider than `u64` can be exercised.
fn extension_raw_oid(oid_content: &[u8], critical: Option<bool>, value: &[u8]) -> Vec<u8> {
    let mut content = tlv(0x06, oid_content);
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
/// arbitrary, already-encoded TBS tail (usually the extensions field).
fn build_cert(version: Option<u8>, tail: &[u8]) -> Vec<u8> {
    build_cert_times(version, tail, b"260115093000Z", b"270115093000Z")
}

fn build_cert_times(
    version: Option<u8>,
    tail: &[u8],
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
