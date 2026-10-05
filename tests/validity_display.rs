//! End-to-end regression tests for the validity-period (Not Before /
//! Not After) decoding and display behavior documented in README.md.
//!
//! Like `tests/name_display.rs`, these drive the real `chainview` binary so
//! the public contract is locked as a whole: on success the five fields keep
//! their fixed order, and malformed input means exit code 2, empty stdout and
//! the `chainview: invalid DER certificate:` stderr prefix with a non-empty
//! reason. Only time decoding is exercised here — signature verification,
//! trust and whether the certificate is currently expired are intentionally
//! out of scope: `inspect` displays fields, it never judges them.
//!
//! Coverage:
//! - `YYYY-MM-DDTHH:MM:SSZ` rendering for both UTCTime and GeneralizedTime;
//! - the UTCTime two-digit year pivot (<50 => 2000..2049, >=50 =>
//!   1950..1999), including the 49/50 boundary, and GeneralizedTime's
//!   four-digit years at and after 2050;
//! - the two time fields interpreting their own (possibly different)
//!   encodings independently;
//! - output independence from the local timezone;
//! - calendar validity (Gregorian leap years, range checks, no rollover)
//!   kept separate from expiry/not-yet-valid;
//! - strict textual form: mandatory seconds, trailing `Z`, no offsets,
//!   fractions, stray characters or other (non-time) tags, in either field.
//!
//! Certificates are built with the tiny zero-dependency DER constructor
//! copied below so individual time encodings can be crafted byte for byte.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXED_SERIAL: &str = "0E8A4C2F9B17D603";
const FIXED_SUBJECT: &str = "CN=example.com";
const FIXED_ISSUER: &str = "CN=Test CA";

// ----- Known X.520 attribute OID arcs -------------------------------------

const CN: &[u64] = &[2, 5, 4, 3];

const TAG_UTC_TIME: u8 = 0x17;
const TAG_GENERALIZED_TIME: u8 = 0x18;

// ----- Success: year pivot and encodings -----------------------------------

#[test]
fn utctime_year_pivot_49_is_2049_and_50_is_1950() {
    // The boundary years must map by the fixed rule, never to an adjacent
    // year: 49 is 2049 (not 1949), 50 is 1950 (not 2050). A certificate may
    // have Not After before Not Before; validity windows are not judged.
    let cert = default_cert(
        &utc(b"490630120000Z"),
        &utc(b"500101000000Z"),
    );
    let (out, _cert) = run_inspect("utc-pivot-49-50", &cert);
    assert_times_success(&out, "2049-06-30T12:00:00Z", "1950-01-01T00:00:00Z");

    // The other end of each half: 00 is 2000 and 99 is 1999.
    let cert = default_cert(
        &utc(b"000101000000Z"),
        &utc(b"991231235959Z"),
    );
    let (out, _cert) = run_inspect("utc-pivot-00-99", &cert);
    assert_times_success(&out, "2000-01-01T00:00:00Z", "1999-12-31T23:59:59Z");
}

#[test]
fn generalized_time_keeps_four_digit_years_including_2050_and_beyond() {
    // GeneralizedTime is read literally as a four-digit year — there is no
    // pivot — so 2050 and later render correctly, and a fully written 1949
    // is not moved into 2049.
    let cert = default_cert(
        &generalized(b"20500101000000Z"),
        &generalized(b"20991231235959Z"),
    );
    let (out, _cert) = run_inspect("gt-2050-2099", &cert);
    assert_times_success(&out, "2050-01-01T00:00:00Z", "2099-12-31T23:59:59Z");

    let cert = default_cert(
        &generalized(b"19491231235959Z"),
        &generalized(b"20491231235959Z"),
    );
    let (out, _cert) = run_inspect("gt-1949-2049", &cert);
    assert_times_success(&out, "1949-12-31T23:59:59Z", "2049-12-31T23:59:59Z");
}

#[test]
fn not_before_and_not_after_may_use_different_time_encodings() {
    // Each field is decoded from its own tag: UTCTime in one and
    // GeneralizedTime in the other, in both arrangements.
    let cert = default_cert(
        &utc(b"260115093000Z"),
        &generalized(b"20500115093000Z"),
    );
    let (out, _cert) = run_inspect("mixed-utc-then-gt", &cert);
    assert_times_success(&out, "2026-01-15T09:30:00Z", "2050-01-15T09:30:00Z");

    let cert = default_cert(
        &generalized(b"19990115093000Z"),
        &utc(b"490115093000Z"),
    );
    let (out, _cert) = run_inspect("mixed-gt-then-utc", &cert);
    assert_times_success(&out, "1999-01-15T09:30:00Z", "2049-01-15T09:30:00Z");
}

// ----- Success: timezone independence --------------------------------------

#[test]
fn output_is_identical_in_every_local_timezone() {
    // The values are instants in UTC; the local zone (including date-line
    // offsets) must never shift the printed date or time. POSIX TZ strings
    // are used so the test needs no zoneinfo files: UTC-14 is fourteen hours
    // *ahead* of UTC, HST10 ten hours behind.
    let cert = default_cert(
        &utc(b"260115093000Z"),
        &utc(b"270115093000Z"),
    );
    let expected = format!(
        "Subject: {FIXED_SUBJECT}\n\
         Issuer: {FIXED_ISSUER}\n\
         Serial Number: {FIXED_SERIAL}\n\
         Not Before: 2026-01-15T09:30:00Z\n\
         Not After: 2027-01-15T09:30:00Z\n"
    );

    let zones: &[Option<&str>] = &[
        None,
        Some(""),
        Some("UTC0"),
        Some("JST-9"),
        Some("IST-5:30"),
        Some("UTC-14"),
        Some("HST10"),
    ];
    for (i, zone) in zones.iter().enumerate() {
        let label = zone.map_or("unset", |z| if z.is_empty() { "empty" } else { z });
        let out = run_inspect_with_tz(&format!("tz-{i}"), &cert, *zone);
        assert_eq!(out.status.code(), Some(0), "zone {label}: stderr={}", lossy(&out.stderr));
        assert!(out.stderr.is_empty(), "zone {label}: stderr={}", lossy(&out.stderr));
        assert_eq!(
            lossy(&out.stdout),
            expected,
            "zone {label} shifted the output"
        );
    }
}

// ----- Success: expiry is not a parse error --------------------------------

#[test]
fn expired_and_not_yet_valid_certificates_still_display() {
    // Calendar validity and "is the certificate valid today" are separate
    // questions. A long-expired certificate and one far in the future must
    // both display regardless of when the test runs (this file pins no
    // "today", so the assertion holds on any execution date).
    let expired = default_cert(
        &utc(b"800101000000Z"), // 1980-01-01T00:00:00Z
        &utc(b"990101000000Z"), // 1999-01-01T00:00:00Z
    );
    let (out, _cert) = run_inspect("expired", &expired);
    assert_times_success(&out, "1980-01-01T00:00:00Z", "1999-01-01T00:00:00Z");

    let future = default_cert(
        &generalized(b"20990101000000Z"),
        &generalized(b"21000101000000Z"),
    );
    let (out, _cert) = run_inspect("not-yet-valid", &future);
    assert_times_success(&out, "2099-01-01T00:00:00Z", "2100-01-01T00:00:00Z");
}

// ----- Success: Gregorian leap years ---------------------------------------

#[test]
fn february_29_in_2000_and_2004_displays() {
    // 2000 is a leap year (divisible by 400). Through UTCTime the digits "00"
    // mean 2000 — so this exercises the pivot and the century leap rule at
    // once; a "1900" reading would have to reject the date.
    let cert = default_cert(
        &utc(b"000229000000Z"),
        &utc(b"000301000000Z"),
    );
    let (out, _cert) = run_inspect("leap-2000", &cert);
    assert_times_success(&out, "2000-02-29T00:00:00Z", "2000-03-01T00:00:00Z");

    // Ordinary divisible-by-four leap year, also via UTCTime.
    let cert = default_cert(
        &utc(b"040229000000Z"),
        &utc(b"040301000000Z"),
    );
    let (out, _cert) = run_inspect("leap-2004", &cert);
    assert_times_success(&out, "2004-02-29T00:00:00Z", "2004-03-01T00:00:00Z");
}

// ----- Success: boundaries are preserved, not adjusted ---------------------

#[test]
fn midnight_and_month_end_times_keep_their_values() {
    // Exactly-on-boundary values display verbatim: nothing rounds, shifts a
    // day, or rejects a midnight / month-end timestamp.
    let cert = default_cert(
        &utc(b"260101000000Z"),
        &utc(b"261231235959Z"),
    );
    let (out, _cert) = run_inspect("midnight-yearend", &cert);
    assert_times_success(&out, "2026-01-01T00:00:00Z", "2026-12-31T23:59:59Z");

    // Last instant of the leap day and of a 30-day month.
    let cert = default_cert(
        &generalized(b"20240229000000Z"),
        &generalized(b"20240430235959Z"),
    );
    let (out, _cert) = run_inspect("leap-monthend", &cert);
    assert_times_success(&out, "2024-02-29T00:00:00Z", "2024-04-30T23:59:59Z");
}

// ----- Failure: calendar validity ------------------------------------------

#[test]
fn invalid_calendar_values_fail_in_either_field() {
    // Each entry is a syntactically well-formed time text naming an
    // impossible date/time. Nothing may roll over into the next unit:
    // second 60 is not the next minute, day 32 is not the next month.
    let cases: &[(u8, &[u8])] = &[
        // UTCTime (YYMMDDHHMMSSZ)
        (TAG_UTC_TIME, b"260015093000Z"), // month 00
        (TAG_UTC_TIME, b"261315093000Z"), // month 13
        (TAG_UTC_TIME, b"260100093000Z"), // day 00
        (TAG_UTC_TIME, b"260132093000Z"), // January has 31 days
        (TAG_UTC_TIME, b"260231093000Z"), // February never has 31
        (TAG_UTC_TIME, b"260431093000Z"), // April has 30 days
        (TAG_UTC_TIME, b"260229093000Z"), // 2026 is not a leap year
        (TAG_UTC_TIME, b"240230093000Z"), // even leap February has 29
        (TAG_UTC_TIME, b"260115240000Z"), // hour 24
        (TAG_UTC_TIME, b"260115250000Z"), // hour 25
        (TAG_UTC_TIME, b"260115096000Z"), // minute 60
        (TAG_UTC_TIME, b"260115093060Z"), // second 60 (no leap-second rollover)
        (TAG_UTC_TIME, b"260131235960Z"), // second 60 does not advance the date
        // GeneralizedTime (YYYYMMDDHHMMSSZ)
        (TAG_GENERALIZED_TIME, b"20260015093000Z"), // month 00
        (TAG_GENERALIZED_TIME, b"20261315093000Z"), // month 13
        (TAG_GENERALIZED_TIME, b"20260100093000Z"), // day 00
        (TAG_GENERALIZED_TIME, b"20260132093000Z"), // day 32
        (TAG_GENERALIZED_TIME, b"20260115240000Z"), // hour 24
        (TAG_GENERALIZED_TIME, b"20260115096000Z"), // minute 60
        (TAG_GENERALIZED_TIME, b"20260115093060Z"), // second 60
        // Century exceptions to the four-year leap rule.
        (TAG_GENERALIZED_TIME, b"19000229000000Z"), // 1900 is not a leap year
        (TAG_GENERALIZED_TIME, b"21000229000000Z"), // 2100 is not a leap year
        (TAG_GENERALIZED_TIME, b"19990229000000Z"), // 1999 is not a leap year
    ];
    for (i, &(tag, text)) in cases.iter().enumerate() {
        let bad = tlv(tag, text);
        let cert = default_cert(&bad, &good_na());
        let (out, _cert) = run_inspect(&format!("badcal-before-{i}"), &cert);
        assert_invalid_certificate(&out);

        let cert = default_cert(&good_nb(), &bad);
        let (out, _cert) = run_inspect(&format!("badcal-after-{i}"), &cert);
        assert_invalid_certificate(&out);
    }
}

// ----- Failure: strict textual format --------------------------------------

#[test]
fn noncanonical_time_syntax_fails_in_either_field() {
    // Seconds are mandatory, the value must end in 'Z' (UTC only), digits
    // must be digits, and the tag must actually be a time type. Each input
    // is wrapped in a well-formed TLV so structural parsing succeeds and
    // rejection comes from time decoding itself.
    let cases: &[(u8, &[u8])] = &[
        // UTCTime
        (TAG_UTC_TIME, b""),                 // empty
        (TAG_UTC_TIME, b"2601150930Z"),      // seconds omitted
        (TAG_UTC_TIME, b"260115093000"),     // no trailing Z
        (TAG_UTC_TIME, b"260115093000z"),    // lowercase z
        (TAG_UTC_TIME, b"26011509300aZ"),    // non-digit character
        (TAG_UTC_TIME, b"26011509 000Z"),    // space among the digits
        (TAG_UTC_TIME, b"260115093000+0000"),// explicit offset instead of Z
        (TAG_UTC_TIME, b"260115093000-0800"),// negative offset
        (TAG_UTC_TIME, b"260115093000.0Z"),  // fractional seconds
        (TAG_UTC_TIME, b"260115093000Z "),   // trailing byte after Z
        (TAG_UTC_TIME, b"20260115093000Z"),  // four-digit year under UTCTime
        // GeneralizedTime
        (TAG_GENERALIZED_TIME, b""),                   // empty
        (TAG_GENERALIZED_TIME, b"202601150930Z"),      // seconds omitted
        (TAG_GENERALIZED_TIME, b"20260115093000"),     // no trailing Z
        (TAG_GENERALIZED_TIME, b"20260115093000z"),   // lowercase z
        (TAG_GENERALIZED_TIME, b"2026011509300aZ"),   // non-digit character
        (TAG_GENERALIZED_TIME, b"20260115093000+0000"),// offset instead of Z
        (TAG_GENERALIZED_TIME, b"20260115093000.5Z"), // fractional seconds
        (TAG_GENERALIZED_TIME, b"260115093000Z"),     // two-digit year under GeneralizedTime
        // Other universal types are not Time alternatives.
        (0x16, b"260115093000Z"), // IA5String
        (0x1A, b"260115093000Z"), // VisibleString
        (0x04, b"20260115093000Z"), // OCTET STRING
    ];
    for (i, &(tag, text)) in cases.iter().enumerate() {
        let bad = tlv(tag, text);
        let cert = default_cert(&bad, &good_na());
        let (out, _cert) = run_inspect(&format!("badfmt-before-{i}"), &cert);
        assert_invalid_certificate(&out);

        let cert = default_cert(&good_nb(), &bad);
        let (out, _cert) = run_inspect(&format!("badfmt-after-{i}"), &cert);
        assert_invalid_certificate(&out);
    }
}

#[test]
fn truncated_time_content_fails_in_either_field() {
    // The TLV length announces 30 content bytes but only eight follow; the
    // enclosing validity SEQUENCE is itself correctly bounded, so the error
    // is the truncated time, in first or second position.
    let short = raw_tlv(TAG_UTC_TIME, &[0x1E], b"26011509");

    let validity = seq(&concat(&[&short, &good_na()]));
    let (out, _cert) = run_inspect("trunc-before", &default_cert_validity(&validity));
    assert_invalid_certificate(&out);

    let validity = seq(&concat(&[&good_nb(), &short]));
    let (out, _cert) = run_inspect("trunc-after", &default_cert_validity(&validity));
    assert_invalid_certificate(&out);
}

// ----- Assertions / process plumbing --------------------------------------

/// A successful inspect must show all five fields in the fixed order with the
/// fixed name and serial values — time encodings never influence them — and
/// must leave stderr empty.
fn assert_times_success(out: &Output, not_before: &str, not_after: &str) {
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

/// Every malformed-input case shares the documented contract: exit code 2,
/// nothing on stdout (so neither a name nor the other, valid time field is
/// printed first), and the fixed invalid-certificate prefix on stderr with a
/// non-empty reason after it.
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

fn run_inspect_with_tz(tag: &str, data: &[u8], tz: Option<&str>) -> Output {
    let cert = TempCert::new(tag, data);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_chainview"));
    cmd.arg("inspect").arg(&cert.path);
    match tz {
        Some(zone) => {
            cmd.env("TZ", zone);
        }
        None => {
            cmd.env_remove("TZ");
        }
    }
    cmd.output().expect("running chainview")
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
// Mirrors the constructor in tests/name_display.rs: minimal DER lengths
// everywhere, except where a test injects hand-written length bytes to
// exercise truncation.

fn utc(text: &[u8]) -> Vec<u8> {
    tlv(TAG_UTC_TIME, text)
}

fn generalized(text: &[u8]) -> Vec<u8> {
    tlv(TAG_GENERALIZED_TIME, text)
}

/// Valid counterpart field used when only one time is being corrupted:
/// 2030-01-01T00:00:00Z / 2031-01-01T00:00:00Z in UTCTime.
fn good_nb() -> Vec<u8> {
    utc(b"300101000000Z")
}

fn good_na() -> Vec<u8> {
    utc(b"310101000000Z")
}

fn default_cert(not_before: &[u8], not_after: &[u8]) -> Vec<u8> {
    let validity = seq(&concat(&[not_before, not_after]));
    default_cert_validity(&validity)
}

fn default_cert_validity(validity: &[u8]) -> Vec<u8> {
    build_cert(
        &simple_cn_name("example.com"),
        &simple_cn_name("Test CA"),
        validity,
    )
}

/// Assemble a complete, parseable v1 DER certificate with fixed serial and
/// key material around the supplied Subject/Issuer names and validity TLV.
fn build_cert(subject: &[u8], issuer: &[u8], validity: &[u8]) -> Vec<u8> {
    let serial = tlv(0x02, &[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]);
    let spki = seq(&concat(&[
        &seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 1]), &tlv(0x05, &[])])),
        &tlv(0x03, &[0x00, 0x01, 0x00]), // BIT STRING, zero unused bits
    ]));

    let tbs = seq(&concat(&[
        &serial,
        &signature_algorithm(),
        issuer,
        validity,
        subject,
        &spki,
    ]));
    seq(&concat(&[&tbs, &signature_algorithm(), &tlv(0x03, &[0x00])]))
}

fn signature_algorithm() -> Vec<u8> {
    // sha256WithRSAEncryption with explicit NULL parameters.
    seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 11]), &tlv(0x05, &[])]))
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
