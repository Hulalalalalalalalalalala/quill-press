//! End-to-end regression tests for OID arcs (subidentifiers) whose values
//! exceed the range of a 64-bit machine integer.
//!
//! An OID arc has no upper bound in X.690: any shortest-form base-128
//! subidentifier is legal, however many bits its value needs. `chainview
//! inspect` used to accumulate arcs into a `u64` and reject the whole
//! certificate with "OID arc overflow" as soon as one arc needed more than
//! 64 bits — a machine limit, not an encoding rule. These tests pin the
//! fixed contract:
//!
//! - a legal arc of any size is kept at full precision and rendered as exact
//!   dotted decimal — never truncated, wrapped, approximated or replaced by
//!   placeholder text — in unknown subject/issuer attributes (`OID=#hex`),
//!   in the public-key algorithm, in both signature algorithm copies and in
//!   extension types;
//! - the first subidentifier (40*first + second) is itself unbounded: a huge
//!   second arc under first arc 2 is accepted, and so is a combined
//!   subidentifier that overflows 64 bits while the second arc itself still
//!   fits;
//! - duplicate extension detection compares complete OIDs at full precision:
//!   the same huge OID twice is rejected with the full dotted form named,
//!   while two huge OIDs differing only in the last arc are not duplicates;
//! - corrupt encodings stay corrupt: empty OID content, a subidentifier
//!   missing its terminating byte and non-minimal (padded) base-128 groups
//!   are still rejected with exit code 2, empty stdout and the
//!   `chainview: invalid DER certificate:` prefix.
//!
//! Certificates are built with the same zero-dependency DER constructor style
//! as the other test files; arcs are supplied as decimal strings so values
//! beyond `u64` can be written down exactly and the expected display text is
//! the very same string.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXED_SERIAL: &str = "0E8A4C2F9B17D603";
const FIXED_NOT_BEFORE: &str = "2026-01-15T09:30:00Z";
const FIXED_NOT_AFTER: &str = "2027-01-15T09:30:00Z";

/// 2^64: one past the largest 64-bit unsigned integer.
const TWO_64: &str = "18446744073709551616";
/// 2^64 - 1: the largest value that still fits a `u64`.
const U64_MAX: &str = "18446744073709551615";
/// 2^128: needs three 64-bit words.
const TWO_128: &str = "340282366920938463463374607431768211456";

// ----- Huge arcs in subject/issuer attribute types --------------------------

#[test]
fn huge_second_arc_under_first_arc_2_displays_exactly() {
    // The spec example: attribute type 2.18446744073709551616.3 with a
    // UTF8String "abc" value. The second arc (2^64) overflows a u64, and the
    // combined first subidentifier (80 + 2^64) overflows it too; both must
    // still decode and render as exact dotted decimal.
    let subject = name(&[rdn(&[atv_utf8(&oid_dec(2, TWO_64, &["3"]), "abc")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("huge-second-arc", &build_cert(&subject, &issuer, &[]));

    assert_success(
        &out,
        "2.18446744073709551616.3=#0C03616263",
        "CN=Test CA",
    );
}

#[test]
fn huge_arc_in_later_position_displays_exactly() {
    // A 2^128 arc sits in the fourth position: the same full-precision rule
    // applies to every arc, not only the leading subidentifier.
    let subject = name(&[rdn(&[atv_utf8(&oid_dec(1, "2", &["3", TWO_128]), "abc")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("huge-later-arc", &build_cert(&subject, &issuer, &[]));

    assert_success(
        &out,
        "1.2.3.340282366920938463463374607431768211456=#0C03616263",
        "CN=Test CA",
    );
}

#[test]
fn combined_first_subidentifier_beyond_u64_with_small_second_arc() {
    // 2.(2^64-1): the second arc itself still fits a u64, but the combined
    // first subidentifier is 80 + (2^64-1) = 2^64 + 79, which does not. The
    // split must happen at full precision instead of rejecting the overflow.
    let subject = name(&[rdn(&[atv_utf8(&oid_dec(2, U64_MAX, &[]), "abc")])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("huge-combined-first", &build_cert(&subject, &issuer, &[]));

    assert_success(&out, "2.18446744073709551615=#0C03616263", "CN=Test CA");
}

#[test]
fn huge_arcs_display_in_issuer_as_well() {
    // The same exact-rendering rule governs the Issuer name independently.
    let subject = simple_cn_name("subject.example");
    let issuer = name(&[rdn(&[atv_utf8(&oid_dec(2, TWO_64, &["3"]), "abc")])]);
    let (out, _cert) = run_inspect("huge-issuer", &build_cert(&subject, &issuer, &[]));

    assert_success(
        &out,
        "CN=subject.example",
        "2.18446744073709551616.3=#0C03616263",
    );
}

#[test]
fn huge_arcs_in_known_oid_positions_still_match_short_names() {
    // Precision must not break the ordinary path: the known 2.5.4.3 still
    // earns its CN short name when it shares a name with huge-OID attributes.
    let subject = name(&[
        rdn(&[atv_utf8(&oid_dec(2, TWO_64, &["3"]), "abc")]),
        rdn(&[atv_utf8(&oid(&[2, 5, 4, 3]), "example.com")]),
    ]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("huge-and-known", &build_cert(&subject, &issuer, &[]));

    assert_success(
        &out,
        "CN=example.com,2.18446744073709551616.3=#0C03616263",
        "CN=Test CA",
    );
}

// ----- Huge OIDs in algorithm identifiers and extensions --------------------

#[test]
fn huge_oid_in_public_key_and_both_signature_algorithms_is_accepted() {
    // The same legal huge OID names the public-key algorithm and both
    // signature algorithm copies: none of these positions may fail merely
    // because an arc is large.
    let alg_oid = oid_dec(2, TWO_64, &["3"]);
    let subject = simple_cn_name("example.com");
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect(
        "huge-algorithms",
        &build_cert_with_algorithms(&subject, &issuer, &alg_oid, &alg_oid, &alg_oid, &[]),
    );

    assert_success(&out, "CN=example.com", "CN=Test CA");
}

#[test]
fn huge_oid_as_extension_type_is_accepted() {
    let ext = extension(&oid_dec(2, TWO_64, &["3"]), None, &[0x00]);
    let subject = simple_cn_name("example.com");
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect(
        "huge-extension",
        &build_cert(&subject, &issuer, &extensions_field(&ext)),
    );

    assert_success(&out, "CN=example.com", "CN=Test CA");
}

#[test]
fn duplicate_huge_extension_oid_is_rejected_with_full_dotted_form() {
    // Identity is the complete OID at full precision: the same huge OID
    // twice is a duplicate even with different extension values, and the
    // reason must name every arc in exact dotted decimal.
    let first = extension(&oid_dec(2, TWO_64, &["3"]), None, &[0x01]);
    let second = extension(&oid_dec(2, TWO_64, &["3"]), Some(true), &[0x02, 0x03]);
    let items = concat(&[&first, &second]);
    let subject = simple_cn_name("example.com");
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect(
        "huge-dup-extension",
        &build_cert(&subject, &issuer, &extensions_field(&items)),
    );

    assert_invalid_certificate(&out);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.contains("duplicate extension type"),
        "reason must identify a duplicate extension type: {stderr}"
    );
    assert!(
        stderr.contains("2.18446744073709551616.3"),
        "reason must give the full dotted OID: {stderr}"
    );
}

#[test]
fn huge_oids_differing_only_in_last_arc_are_not_duplicates() {
    // 2.(2^64).3 and 2.(2^64).4 differ only in the final arc; comparing arcs
    // after truncation to 64 bits would call them equal. Both extensions
    // must coexist.
    let a = extension(&oid_dec(2, TWO_64, &["3"]), None, b"same");
    let b = extension(&oid_dec(2, TWO_64, &["4"]), None, b"same");
    let items = concat(&[&a, &b]);
    let subject = simple_cn_name("example.com");
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect(
        "huge-distinct-extensions",
        &build_cert(&subject, &issuer, &extensions_field(&items)),
    );

    assert_success(&out, "CN=example.com", "CN=Test CA");
}

#[test]
fn signature_algorithm_mismatch_with_huge_oids_is_still_rejected() {
    // Two legal but different huge OIDs in the two signatureAlgorithm copies
    // remain a mismatch; the reason names both OIDs in full dotted decimal.
    let outer_oid = oid_dec(2, TWO_64, &["3"]);
    let inner_oid = oid_dec(2, TWO_64, &["4"]);
    let spki_oid = oid(&[1, 2, 840, 113549, 1, 1, 1]);
    let subject = simple_cn_name("example.com");
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect(
        "huge-sig-mismatch",
        &build_cert_with_algorithms(&subject, &issuer, &spki_oid, &outer_oid, &inner_oid, &[]),
    );

    assert_invalid_certificate(&out);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.contains("signature algorithm mismatch"),
        "reason must identify the mismatch: {stderr}"
    );
    assert!(
        stderr.contains("2.18446744073709551616.3") && stderr.contains("2.18446744073709551616.4"),
        "reason must name both full dotted OIDs: {stderr}"
    );
}

// ----- Corrupt OID encodings stay corrupt -----------------------------------

#[test]
fn empty_oid_content_in_attribute_is_rejected() {
    // 06 00: the OID TLV exists but names no arcs.
    let atv = seq(&concat(&[&tlv(0x06, &[]), &tlv(0x0C, b"abc")]));
    let subject = name(&[rdn(&[atv])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("oid-empty", &build_cert(&subject, &issuer, &[]));

    assert_invalid_certificate(&out);
}

#[test]
fn truncated_oid_arc_in_attribute_is_rejected() {
    // 0x88 promises another base-128 byte that never arrives.
    let atv = seq(&concat(&[&tlv(0x06, &[0x88]), &tlv(0x0C, b"abc")]));
    let subject = name(&[rdn(&[atv])]);
    let issuer = simple_cn_name("Test CA");
    let (out, _cert) = run_inspect("oid-truncated", &build_cert(&subject, &issuer, &[]));

    assert_invalid_certificate(&out);
}

#[test]
fn non_minimal_oid_arc_in_attribute_is_rejected() {
    // Arc 1 as 0x80 0x01 carries a redundant leading zero group; a huge arc
    // with the same padding (0x80 followed by the minimal encoding of 2^64)
    // is equally non-minimal.
    for content in [
        vec![0x80, 0x01],
        concat(&[&[0x80], &oid_arc_dec(TWO_64)]),
    ] {
        let atv = seq(&concat(&[&tlv(0x06, &content), &tlv(0x0C, b"abc")]));
        let subject = name(&[rdn(&[atv])]);
        let issuer = simple_cn_name("Test CA");
        let (out, _cert) = run_inspect("oid-nonminimal", &build_cert(&subject, &issuer, &[]));

        assert_invalid_certificate(&out);
    }
}

// ----- Assertions / process plumbing ----------------------------------------

fn assert_success(out: &Output, subject: &str, issuer: &str) {
    assert_eq!(out.status.code(), Some(0), "stderr={}", lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "stderr={}", lossy(&out.stderr));
    assert_eq!(
        lossy(&out.stdout),
        format!(
            "Subject: {subject}\n\
             Issuer: {issuer}\n\
             Serial Number: {FIXED_SERIAL}\n\
             Not Before: {FIXED_NOT_BEFORE}\n\
             Not After: {FIXED_NOT_AFTER}\n"
        )
    );
}

/// Every malformed-input case shares the documented contract: exit code 2,
/// nothing on stdout, and the fixed invalid-certificate message prefix on
/// stderr.
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
            "chainview-oid-test-{}-{tag}-{unique}.der",
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

fn simple_cn_name(cn: &str) -> Vec<u8> {
    name(&[rdn(&[atv_utf8(&oid(&[2, 5, 4, 3]), cn)])])
}

/// Assemble a complete, parseable explicit-v3 DER certificate with fixed
/// serial, validity and algorithm identifiers around the supplied
/// Subject/Issuer names and an arbitrary, already-encoded TBS tail (the
/// extensions field, or empty).
fn build_cert(subject: &[u8], issuer: &[u8], tail: &[u8]) -> Vec<u8> {
    let spki_oid = oid(&[1, 2, 840, 113549, 1, 1, 1]);
    let sig_oid = oid(&[1, 2, 840, 113549, 1, 1, 11]);
    build_cert_with_algorithms(subject, issuer, &spki_oid, &sig_oid, &sig_oid, tail)
}

/// Same as `build_cert` but with caller-supplied algorithm OID TLVs: the
/// public-key algorithm and the two signature algorithm copies (outer and
/// inside tbsCertificate) are independent parameters.
fn build_cert_with_algorithms(
    subject: &[u8],
    issuer: &[u8],
    spki_oid: &[u8],
    outer_sig_oid: &[u8],
    inner_sig_oid: &[u8],
    tail: &[u8],
) -> Vec<u8> {
    let version = tlv(0xA0, &tlv(0x02, &[2])); // explicit v3
    let serial = tlv(0x02, &[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]);
    let inner_sig_alg = seq(&concat(&[inner_sig_oid, &tlv(0x05, &[])]));
    let validity = seq(&concat(&[
        &tlv(0x17, b"260115093000Z"), // UTCTime 2026-01-15T09:30:00Z
        &tlv(0x17, b"270115093000Z"), // UTCTime 2027-01-15T09:30:00Z
    ]));
    let spki = seq(&concat(&[
        &seq(&concat(&[spki_oid, &tlv(0x05, &[])])),
        &tlv(0x03, &[0x00, 0x01, 0x00]), // BIT STRING, zero unused bits
    ]));

    let tbs = seq(&concat(&[
        &version,
        &serial,
        &inner_sig_alg,
        issuer,
        &validity,
        subject,
        &spki,
        tail,
    ]));
    let outer_sig_alg = seq(&concat(&[outer_sig_oid, &tlv(0x05, &[])]));
    seq(&concat(&[&tbs, &outer_sig_alg, &tlv(0x03, &[0x00])]))
}

/// [3] EXPLICIT wrapping one Extensions SEQUENCE that holds the supplied,
/// already-encoded extension items.
fn extensions_field(extension_items: &[u8]) -> Vec<u8> {
    tlv(0xA3, &tlv(0x30, extension_items))
}

/// Extension ::= SEQUENCE { extnID OID, critical BOOLEAN DEFAULT FALSE,
/// extnValue OCTET STRING } — `oid_tlv` is the complete OID TLV.
fn extension(oid_tlv: &[u8], critical: Option<bool>, value: &[u8]) -> Vec<u8> {
    let mut content = oid_tlv.to_vec();
    if let Some(flag) = critical {
        content.extend_from_slice(&tlv(0x01, &[if flag { 0xFF } else { 0x00 }]));
    }
    content.extend_from_slice(&tlv(0x04, value));
    tlv(0x30, &content)
}

fn name(rdns: &[Vec<u8>]) -> Vec<u8> {
    tlv(
        0x30,
        &concat(&rdns.iter().map(Vec::as_slice).collect::<Vec<_>>()),
    )
}

fn rdn(atvs: &[Vec<u8>]) -> Vec<u8> {
    tlv(
        0x31,
        &concat(&atvs.iter().map(Vec::as_slice).collect::<Vec<_>>()),
    )
}

fn atv_utf8(oid_tlv: &[u8], text: &str) -> Vec<u8> {
    seq(&concat(&[oid_tlv, &tlv(0x0C, text.as_bytes())]))
}

/// OID TLV from small arcs that fit a `u64` (used for the well-known
/// algorithm and attribute OIDs).
fn oid(arcs: &[u64]) -> Vec<u8> {
    assert!(arcs.len() >= 2);
    let mut content = oid_arc_u64(arcs[0] * 40 + arcs[1]);
    for &arc in &arcs[2..] {
        content.extend_from_slice(&oid_arc_u64(arc));
    }
    tlv(0x06, &content)
}

fn oid_arc_u64(mut value: u64) -> Vec<u8> {
    let mut out = vec![(value % 128) as u8];
    value /= 128;
    while value > 0 {
        out.push(0x80 | (value % 128) as u8);
        value /= 128;
    }
    out.reverse();
    out
}

/// OID TLV whose arcs are given as exact decimal strings, so arcs beyond the
/// `u64` range can be written down precisely. `first` is the small first arc
/// (0, 1 or 2); `second` and `rest` are decimal arc values. The first
/// subidentifier (first*40 + second) is computed in decimal.
fn oid_dec(first: u64, second: &str, rest: &[&str]) -> Vec<u8> {
    assert!(first <= 2);
    let mut content = oid_arc_dec(&dec_add_small(second, first * 40));
    for arc in rest {
        content.extend_from_slice(&oid_arc_dec(arc));
    }
    tlv(0x06, &content)
}

/// Base-128 subidentifier bytes for an arc value given as a decimal string,
/// in the minimal (shortest) form DER requires.
fn oid_arc_dec(decimal: &str) -> Vec<u8> {
    let mut digits: Vec<u8> = decimal.bytes().map(|b| b - b'0').collect();
    assert!(!digits.is_empty() && digits[0] != 0 || decimal == "0");
    // Repeatedly divide the decimal number by 128; the remainders are the
    // base-128 groups, least significant first.
    let mut groups: Vec<u8> = Vec::new();
    loop {
        let mut rem = 0u32;
        let mut quo: Vec<u8> = Vec::with_capacity(digits.len());
        for &d in &digits {
            let cur = rem * 10 + d as u32;
            quo.push((cur / 128) as u8);
            rem = cur % 128;
        }
        groups.push(rem as u8);
        match quo.iter().position(|&d| d != 0) {
            Some(p) => digits = quo[p..].to_vec(),
            None => break,
        }
    }
    let n = groups.len();
    groups
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &g)| if i + 1 < n { 0x80 | g } else { g })
        .collect()
}

/// Decimal string addition of a small (u64) addend.
fn dec_add_small(dec: &str, add: u64) -> String {
    let mut digits: Vec<u8> = dec.bytes().map(|b| b - b'0').collect();
    let mut carry = add;
    let mut i = digits.len();
    while carry > 0 {
        if i == 0 {
            digits.insert(0, 0);
            i = 1;
        }
        i -= 1;
        let sum = digits[i] as u64 + carry % 10;
        digits[i] = (sum % 10) as u8;
        carry = carry / 10 + sum / 10;
    }
    digits.into_iter().map(|d| (b'0' + d) as char).collect()
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
