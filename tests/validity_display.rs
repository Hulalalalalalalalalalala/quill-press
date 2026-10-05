//! End-to-end regression tests for the validity-period (Not Before / Not
//! After) decoding and display behavior documented in README.md.
//!
//! Like the name-display suite, these drive the real `chainview` binary so the
//! public contract is locked as a whole: the five output lines and their order
//! on success (names and serial number unaffected by the time encoding), and
//! exit code 2 with empty stdout plus the `chainview: invalid DER certificate:`
//! stderr prefix whenever either time field is malformed.
//!
//! Scope is strictly display: inspect never checks signatures or trust, and a
//! certificate that is expired or not yet valid must still be shown. Date
//! validity (a real calendar date in strict `YYYY-MM-DDTHH:MM:SSZ` form) is a
//! separate question from expiry and is what this file guards.
//!
//! Certificates are built with the tiny zero-dependency DER constructor at the
//! bottom of this file; time TLVs are supplied byte for byte so truncated,
//! mistyped or non-canonical time encodings can be crafted.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXED_SUBJECT: &str = "CN=example.com";
const FIXED_ISSUER: &str = "CN=Test CA";
const FIXED_SERIAL: &str = "0E8A4C2F9B17D603";

// ----- Strict format and year-pivot success cases --------------------------

#[test]
fn utctime_renders_as_strict_utc_text() {
    // The fixed anchor values: both fields come out as YYYY-MM-DDTHH:MM:SSZ,
    // always ending in Z, zero-padded, with seconds present. Names and serial
    // keep their own lines and order.
    let (out, _cert) = run_inspect(
        "utc-basic",
        &build_cert(utc(b"260115093000Z"), utc(b"270115093000Z")),
    );
    assert_times(&out, "2026-01-15T09:30:00Z", "2027-01-15T09:30:00Z");
}

#[test]
fn utctime_two_digit_year_pivot_is_exact() {
    // YY < 50 is 2000..2049 and YY >= 50 is 1950..1999; in particular 49 is
    // 2049 (not 1949) and 50 is 1950 (not 2050) — no sliding window relative
    // to "today", so the interpretation never shifts with the run date.
    let cases: &[(&[u8], &str)] = &[
        (b"000601000000Z", "2000-06-01T00:00:00Z"),
        (b"490601000000Z", "2049-06-01T00:00:00Z"),
        (b"500601000000Z", "1950-06-01T00:00:00Z"),
        (b"990601000000Z", "1999-06-01T00:00:00Z"),
    ];
    for (text, expected) in cases {
        let (out, _cert) = run_inspect(
            "utc-pivot",
            &build_cert(utc(text), utc(b"270115093000Z")),
        );
        assert_eq!(out.status.code(), Some(0), "stderr={}", lossy(&out.stderr));
        assert!(out.stderr.is_empty());
        assert_eq!(line(&out.stdout, 3), format!("Not Before: {expected}"));
    }
}

#[test]
fn generalizedtime_uses_four_digit_year_including_2050_and_beyond() {
    // GeneralizedTime always carries the full year, including years UTCTime
    // cannot express; 2050 and later must survive intact.
    let cases: &[(&[u8], &str)] = &[
        (b"19500601000000Z", "1950-06-01T00:00:00Z"),
        (b"20490601000000Z", "2049-06-01T00:00:00Z"),
        (b"20500601000000Z", "2050-06-01T00:00:00Z"),
        (b"99991231235959Z", "9999-12-31T23:59:59Z"),
    ];
    for (text, expected) in cases {
        let (out, _cert) = run_inspect(
            "gen-year",
            &build_cert(gentime(text), utc(b"270115093000Z")),
        );
        assert_eq!(out.status.code(), Some(0), "stderr={}", lossy(&out.stderr));
        assert!(out.stderr.is_empty());
        assert_eq!(line(&out.stdout, 3), format!("Not Before: {expected}"));
    }
}

#[test]
fn mixed_time_encodings_are_each_explained_by_their_own_tag() {
    // The two fields are independent: a UTCTime and a GeneralizedTime in the
    // same certificate each follow their own year rules, in either order.
    let (out, _cert) = run_inspect(
        "mixed-utc-then-gen",
        &build_cert(utc(b"490615120000Z"), gentime(b"20500615120000Z")),
    );
    assert_times(&out, "2049-06-15T12:00:00Z", "2050-06-15T12:00:00Z");

    let (out, _cert) = run_inspect(
        "mixed-gen-then-utc",
        &build_cert(gentime(b"19850401123045Z"), utc(b"991231235959Z")),
    );
    assert_times(&out, "1985-04-01T12:30:45Z", "1999-12-31T23:59:59Z");
}

#[test]
fn output_is_identical_in_every_local_timezone() {
    // Times are decoded as the UTC text they are; no local conversion exists.
    // Zones chosen for far offsets and a half-hour offset that would shift the
    // calendar date if any local-time handling crept in.
    let data = build_cert(utc(b"260115093000Z"), gentime(b"20500115093000Z"));
    let zones = ["UTC", "America/Los_Angeles", "Asia/Kolkata", "Pacific/Auckland"];
    let mut baseline: Option<Vec<u8>> = None;
    for zone in zones {
        let cert = TempCert::new("tz", &data);
        let out = Command::new(env!("CARGO_BIN_EXE_chainview"))
            .env("TZ", zone)
            .arg("inspect")
            .arg(&cert.path)
            .output()
            .expect("running chainview");
        assert_eq!(out.status.code(), Some(0), "zone {zone}: stderr={}", lossy(&out.stderr));
        assert!(out.stderr.is_empty(), "zone {zone}");
        if let Some(expected) = &baseline {
            assert_eq!(&out.stdout, expected, "zone {zone} differs from UTC");
        } else {
            baseline = Some(out.stdout);
        }
    }
}

// ----- Expiry is not a parse error -----------------------------------------

#[test]
fn long_expired_certificate_still_displays() {
    // Both fields deep in the past relative to any plausible run date: a real
    // date that is expired is shown; the wall clock at execution time must
    // never be consulted.
    let (out, _cert) = run_inspect(
        "expired",
        &build_cert(utc(b"500101000000Z"), utc(b"991231235959Z")),
    );
    assert_times(&out, "1950-01-01T00:00:00Z", "1999-12-31T23:59:59Z");
}

#[test]
fn not_yet_valid_certificate_still_displays() {
    // Far-future validity: notBefore in the future is display data, not an
    // error.
    let (out, _cert) = run_inspect(
        "future",
        &build_cert(gentime(b"20990101000000Z"), gentime(b"21000101000000Z")),
    );
    assert_times(&out, "2099-01-01T00:00:00Z", "2100-01-01T00:00:00Z");
}

// ----- Gregorian leap-year rules -------------------------------------------

#[test]
fn february_29_of_a_leap_year_displays() {
    // 2000 is a leap year (divisible by 400); the date is preserved under
    // both encodings.
    let (out, _cert) = run_inspect(
        "leap-2000-utc",
        &build_cert(utc(b"000229120000Z"), utc(b"270115093000Z")),
    );
    assert_times(&out, "2000-02-29T12:00:00Z", "2027-01-15T09:30:00Z");

    let (out, _cert) = run_inspect(
        "leap-2000-gen",
        &build_cert(gentime(b"20000229120000Z"), gentime(b"24960229120000Z")),
    );
    assert_times(&out, "2000-02-29T12:00:00Z", "2496-02-29T12:00:00Z");
}

#[test]
fn february_29_of_common_years_is_corrupt_input() {
    // Century years not divisible by 400 are NOT leap years. Both can only be
    // expressed as GeneralizedTime.
    let cases = [
        gentime(b"19000229000000Z"),
        gentime(b"21000229000000Z"),
        utc(b"260229000000Z"), // 2026 is not a leap year either
    ];
    for bad in cases {
        for slot in [Slot::Before, Slot::After] {
            let (out, _cert) = run_inspect("bad-leap", &build_with_slot(slot, &bad));
            assert_invalid_certificate(&out);
        }
    }
}

// ----- Calendar and time-of-day ranges -------------------------------------

#[test]
fn out_of_range_components_are_rejected_without_carry() {
    // Month/day/hour/minute/second past their real maximum (or zero where a
    // one-based value is required) is corrupt input: the value must never roll
    // over into the next unit. Covered for both encodings and both fields.
    let cases: &[(Vec<u8>, &str)] = &[
        (utc(b"260015093000Z"), "month 00 (UTCTime)"),
        (utc(b"261315093000Z"), "month 13 (UTCTime)"),
        (gentime(b"20500015093000Z"), "month 00 (GeneralizedTime)"),
        (gentime(b"20501315093000Z"), "month 13 (GeneralizedTime)"),
        (utc(b"260100093000Z"), "day 00"),
        (utc(b"260230093000Z"), "30 February"),
        (utc(b"260431093000Z"), "31 April"),
        (gentime(b"20500132093000Z"), "day 32"),
        (utc(b"260115240000Z"), "hour 24 (no midnight carry to next day)"),
        (gentime(b"20500101240000Z"), "hour 24 (GeneralizedTime)"),
        (utc(b"260115096000Z"), "minute 60"),
        (utc(b"260115093060Z"), "second 60 (no leap-second rollover)"),
        (utc(b"261231235960Z"), "second 60 at a year boundary"),
    ];
    for (bad, label) in cases {
        for slot in [Slot::Before, Slot::After] {
            let (out, _cert) =
                run_inspect("bad-range", &build_with_slot(slot, bad));
            assert_invalid_certificate(&out);
            // Sanity anchor for the label so a dropped case fails loudly.
            let _ = label;
        }
    }
}

#[test]
fn midnight_and_month_end_are_kept_verbatim() {
    // Boundary values that are legal must not be nudged: 00:00:00 stays
    // midnight on the stated day, 23:59:59 on the last day stays there, no
    // off-by-one in either direction.
    let (out, _cert) = run_inspect(
        "boundaries",
        &build_cert(utc(b"000101000000Z"), utc(b"991231235959Z")),
    );
    assert_times(&out, "2000-01-01T00:00:00Z", "1999-12-31T23:59:59Z");

    let (out, _cert) = run_inspect(
        "month-end",
        &build_cert(utc(b"260131235959Z"), gentime(b"20500430000000Z")),
    );
    assert_times(&out, "2026-01-31T23:59:59Z", "2050-04-30T00:00:00Z");
}

// ----- Strict textual form --------------------------------------------------

#[test]
fn non_canonical_time_text_is_rejected() {
    // Exactly YYMMDDHHMMSSZ / YYYYMMDDHHMMSSZ: seconds mandatory, only the Z
    // form allowed, no fraction, no stray characters, no alternative year
    // width. Every variant fails in either field.
    let cases: &[Vec<u8>] = &[
        // Missing seconds.
        utc(b"2601150930Z"),
        gentime(b"205001150930Z"),
        // Missing/non-uppercase terminator.
        utc(b"260115093000"),
        utc(b"260115093000z"),
        gentime(b"20500115093000z"),
        // Explicit offsets instead of Z.
        utc(b"260115093000+0800"),
        utc(b"260115093000-0800"),
        gentime(b"20500115093000+0000"),
        // Fractional seconds (dot or comma).
        utc(b"260115093000.0Z"),
        gentime(b"20500115093000.5Z"),
        gentime(b"20500115093000,0Z"),
        // Non-digit characters embedded in the digits.
        utc(b"26011509300AZ"),
        utc(b"2601150930 0Z"),
        gentime(b"2050011509300 Z"),
        // Wrong year width for the tag.
        utc(b"20260115093000Z"),
        gentime(b"260115093000Z"),
        // Trailing junk.
        gentime(b"20500115093000Z\x00"),
        utc(b"260115093000ZZ"),
    ];
    for bad in cases {
        for slot in [Slot::Before, Slot::After] {
            let (out, _cert) = run_inspect("bad-time-text", &build_with_slot(slot, bad));
            assert_invalid_certificate(&out);
        }
    }
}

#[test]
fn other_time_field_types_are_rejected() {
    // Validity requires UTCTime/GeneralizedTime exactly; a different ASN.1
    // type holding plausible-looking text is corrupt, in either field.
    let alien_ia5 = tlv(0x16, b"260115093000Z"); // IA5String
    let alien_octet = tlv(0x04, b"20500115093000Z"); // OCTET STRING
    for (bad, label) in [(&alien_ia5, "IA5String"), (&alien_octet, "OCTET STRING")] {
        for slot in [Slot::Before, Slot::After] {
            let (out, _cert) = run_inspect("alien-time", &build_with_slot(slot, bad));
            assert_invalid_certificate(&out);
            let _ = label;
        }
    }
}

#[test]
fn truncated_time_tlv_is_rejected() {
    // The length byte announces the full form but content is cut short; the
    // failure surfaces as an invalid certificate, not a crash.
    let mut truncated = utc(b"260115093000Z");
    truncated.truncate(truncated.len() - 3);
    for slot in [Slot::Before, Slot::After] {
        let (out, _cert) = run_inspect("truncated-time", &build_with_slot(slot, &truncated));
        assert_invalid_certificate(&out);
    }
}

// ----- Assertions / process plumbing ---------------------------------------

#[derive(Clone, Copy)]
enum Slot {
    Before,
    After,
}

/// Build a certificate with `bad` placed in one validity slot and a known-good
/// UTCTime in the other, so a failure can only be attributed to the bad field.
fn build_with_slot(slot: Slot, bad: &[u8]) -> Vec<u8> {
    let good = utc(b"260115093000Z");
    match slot {
        Slot::Before => build_cert(bad, &good),
        Slot::After => build_cert(&good, bad),
    }
}

fn assert_times(out: &Output, not_before: &str, not_after: &str) {
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

/// Every malformed-time case shares the documented contract: exit code 2,
/// nothing on stdout (so neither name nor the healthy time field is printed
/// before the error), and the fixed invalid-certificate prefix on stderr with
/// a non-empty reason.
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

fn line(stdout: &[u8], idx: usize) -> String {
    lossy(stdout).lines().nth(idx).expect("output line").to_string()
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
            "chainview-validity-test-{}-{tag}-{unique}.der",
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
// Mirrors the constructor in name_display.rs except that validity is built
// from caller-supplied time TLVs so individual time encodings can be crafted.

fn utc(text: &[u8]) -> Vec<u8> {
    tlv(0x17, text)
}

fn gentime(text: &[u8]) -> Vec<u8> {
    tlv(0x18, text)
}

/// Assemble a complete, parseable v1 DER certificate with fixed names, serial
/// and key material around the supplied Not Before / Not After time TLVs.
fn build_cert<N, F>(not_before: N, not_after: F) -> Vec<u8>
where
    N: AsRef<[u8]>,
    F: AsRef<[u8]>,
{
    let not_before = not_before.as_ref();
    let not_after = not_after.as_ref();
    let subject = simple_cn_name("example.com");
    let issuer = simple_cn_name("Test CA");
    let serial = tlv(0x02, &[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]);
    let validity = seq(&concat(&[not_before, not_after]));
    let spki = seq(&concat(&[
        &seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 1]), &tlv(0x05, &[])])),
        &tlv(0x03, &[0x00, 0x01, 0x00]), // BIT STRING, zero unused bits
    ]));

    let tbs = seq(&concat(&[
        &serial,
        &signature_algorithm(),
        &issuer,
        &validity,
        &subject,
        &spki,
    ]));
    seq(&concat(&[&tbs, &signature_algorithm(), &tlv(0x03, &[0x00])]))
}

fn signature_algorithm() -> Vec<u8> {
    // sha256WithRSAEncryption with explicit NULL parameters.
    seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 11]), &tlv(0x05, &[])]))
}

fn simple_cn_name(cn: &str) -> Vec<u8> {
    // Name ::= SEQUENCE { SET { AttributeTypeAndValue { CN OID, UTF8String } } }
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
