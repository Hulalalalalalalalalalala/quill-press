//! End-to-end regression tests for the way `chainview inspect` renders the
//! Subject and Issuer names.
//!
//! The behavior pinned here is the one documented in README.md:
//! - known attribute types keep their RFC 4514 short name (`CN`, `O`, ...)
//!   and their value is shown as (escaped) UTF-8 text;
//! - attribute types without a known short name are shown as their dotted
//!   decimal OID, followed by `=` and an unescaped `#` that introduces the
//!   FULL DER encoding of the value (tag, length and content) in upper-case
//!   hex — readable text values are never shown directly for such types;
//! - RDN groups are rendered in reverse order, attributes inside one
//!   multi-valued RDN are joined with `+` and never split into groups;
//! - a malformed value (invalid UTF-8 in a UTF8String, truncated DER,
//!   non-minimal length encoding) invalidates the whole certificate:
//!   exit code 2, empty stdout, diagnostic on stderr.
//!
//! Certificates are built byte-by-byte below so the suite needs no external
//! tooling and works with `cargo test --offline`. Signature verification,
//! trust decisions and notBefore/notAfter validity are intentionally outside
//! the scope of these expectations.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_chainview");

// ---------------------------------------------------------------------------
// Tiny DER encoder: only what is needed to build certificates for the tests.
// ---------------------------------------------------------------------------

mod der {
    /// Encode one complete DER TLV with the minimal length encoding.
    pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        put_len(content.len(), &mut out);
        out.extend_from_slice(content);
        out
    }

    pub fn utf8_string(s: &str) -> Vec<u8> {
        tlv(0x0C, s.as_bytes())
    }

    pub fn printable_string(s: &str) -> Vec<u8> {
        tlv(0x13, s.as_bytes())
    }

    pub fn oid(arcs: &[u64]) -> Vec<u8> {
        assert!(arcs.len() >= 2, "an OID has at least two arcs");
        let mut content = Vec::new();
        // First subidentifier encodes the first two arcs as 40*x1 + x2;
        // for the 2.* tree the second arc is unbounded (e.g. 2.999).
        put_arc(arcs[0] * 40 + arcs[1], &mut content);
        for &arc in &arcs[2..] {
            put_arc(arc, &mut content);
        }
        tlv(0x06, &content)
    }

    pub fn concat(parts: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for part in parts {
            out.extend_from_slice(part);
        }
        out
    }

    fn put_len(len: usize, out: &mut Vec<u8>) {
        if len < 0x80 {
            out.push(len as u8);
            return;
        }
        let mut tail = Vec::new();
        let mut v = len;
        while v > 0 {
            tail.push((v & 0xFF) as u8);
            v >>= 8;
        }
        out.push(0x80 | tail.len() as u8);
        tail.reverse();
        out.extend_from_slice(&tail);
    }

    fn put_arc(mut v: u64, out: &mut Vec<u8>) {
        let mut tail = vec![(v & 0x7F) as u8];
        v >>= 7;
        while v > 0 {
            tail.push(0x80 | ((v & 0x7F) as u8));
            v >>= 7;
        }
        tail.reverse();
        out.extend_from_slice(&tail);
    }
}

/// Known X.520 OIDs used when building names.
const OID_CN: &[u64] = &[2, 5, 4, 3];
const OID_O: &[u64] = &[2, 5, 4, 10];
const OID_OU: &[u64] = &[2, 5, 4, 11];
const OID_C: &[u64] = &[2, 5, 4, 6];
/// A type deliberately absent from chainview's short-name table.
const OID_UNKNOWN: &[u64] = &[1, 2, 3, 4];
/// Another unknown type, used so equal OIDs can repeat without merging.
const OID_UNKNOWN_2: &[u64] = &[1, 2, 3, 5];

/// AttributeTypeAndValue ::= SEQUENCE { type OID, value ANY }
fn atv(oid: &[u64], value_tlv: &[u8]) -> Vec<u8> {
    der::tlv(
        0x30,
        &der::concat(&[&der::oid(oid), value_tlv]),
    )
}

/// One RelativeDistinguishedName (SET OF AttributeTypeAndValue).
fn rdn(atvs: Vec<Vec<u8>>) -> Vec<u8> {
    let refs: Vec<&[u8]> = atvs.iter().map(Vec::as_slice).collect();
    der::tlv(0x31, &der::concat(&refs))
}

/// Name ::= SEQUENCE OF RelativeDistinguishedName, in encoded order
/// (inspect displays them reversed).
fn name(rdns: Vec<Vec<u8>>) -> Vec<u8> {
    let refs: Vec<&[u8]> = rdns.iter().map(Vec::as_slice).collect();
    der::tlv(0x30, &der::concat(&refs))
}

/// Build a complete, structurally valid v3 DER certificate around the given
/// subject/issuer names. Serial number and validity are fixed; the signature
/// is an empty BIT STRING because inspect does not verify signatures.
fn certificate(subject: &[u8], issuer: &[u8]) -> Vec<u8> {
    let version = der::tlv(0xA0, &der::tlv(0x02, &[2])); // [0] EXPLICIT INTEGER 2
    let serial = der::tlv(0x02, &[1]);
    let sha256_rsa = &[1u64, 2, 840, 113549, 1, 1, 11];
    let rsa_encryption = &[1u64, 2, 840, 113549, 1, 1, 1];
    let null = der::tlv(0x05, &[]);
    let sigalg = der::tlv(
        0x30,
        &der::concat(&[&der::oid(sha256_rsa), &null]),
    );
    let validity = der::tlv(
        0x30,
        &der::concat(&[
            &der::tlv(0x17, b"260115093000Z"),
            &der::tlv(0x17, b"270115093000Z"),
        ]),
    );
    let spki = der::tlv(
        0x30,
        &der::concat(&[
            &der::tlv(
                0x30,
                &der::concat(&[&der::oid(rsa_encryption), &null]),
            ),
            &der::tlv(0x03, &[0]), // empty key BIT STRING, zero unused bits
        ]),
    );
    let tbs = der::tlv(
        0x30,
        &der::concat(&[
            &version, &serial, &sigalg, issuer, &validity, subject, &spki,
        ]),
    );
    let signature = der::tlv(0x03, &[0]);
    der::tlv(
        0x30,
        &der::concat(&[&tbs, &sigalg, &signature]),
    )
}

/// The issuer used by most tests: encoded C,O,CN so it displays
/// `CN=Example CA,O=示例公司,C=CN`.
fn example_issuer() -> Vec<u8> {
    name(vec![
        rdn(vec![atv(OID_C, &der::printable_string("CN"))]),
        rdn(vec![atv(OID_O, &der::utf8_string("示例公司"))]),
        rdn(vec![atv(OID_CN, &der::utf8_string("Example CA"))]),
    ])
}

// ---------------------------------------------------------------------------
// Process-level helpers
// ---------------------------------------------------------------------------

static TEMP_SEQ: AtomicU32 = AtomicU32::new(0);

struct TempCert(PathBuf);

impl TempCert {
    fn new(bytes: &[u8]) -> Self {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "chainview-regression-{}-{seq}.der",
            std::process::id()
        ));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
        TempCert(path)
    }
}

impl Drop for TempCert {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn inspect_bytes(bytes: &[u8]) -> Output {
    let file = TempCert::new(bytes);
    Command::new(BIN)
        .arg("inspect")
        .arg(&file.0)
        .output()
        .unwrap()
}

fn expect_success(bytes: &[u8]) -> String {
    let out = inspect_bytes(bytes);
    assert!(
        out.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
    String::from_utf8(out.stdout).unwrap()
}

fn expect_invalid_certificate(bytes: &[u8]) -> String {
    let out = inspect_bytes(bytes);
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit code 2, got {:?}",
        out.status
    );
    assert!(
        out.stdout.is_empty(),
        "stdout must stay empty on failure, got: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.starts_with("chainview: invalid DER certificate:"),
        "unexpected stderr: {err:?}"
    );
    assert!(err.ends_with('\n'));
    err
}

fn line<'a>(output: &'a str, prefix: &str) -> &'a str {
    output
        .lines()
        .find(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("missing line starting {prefix:?} in:\n{output}"))
}

// ---------------------------------------------------------------------------
// Success cases: name rendering
// ---------------------------------------------------------------------------

#[test]
fn full_output_matches_the_readme_example_field_by_field() {
    let subject = name(vec![
        rdn(vec![atv(OID_C, &der::printable_string("CN"))]),
        rdn(vec![atv(OID_O, &der::utf8_string("示例公司"))]),
        rdn(vec![atv(OID_CN, &der::utf8_string("example.com"))]),
    ]);
    let out = expect_success(&certificate(&subject, &example_issuer()));
    assert_eq!(
        out,
        "Subject: CN=example.com,O=示例公司,C=CN\n\
         Issuer: CN=Example CA,O=示例公司,C=CN\n\
         Serial Number: 01\n\
         Not Before: 2026-01-15T09:30:00Z\n\
         Not After: 2027-01-15T09:30:00Z\n"
    );
}

#[test]
fn unknown_oid_shows_dotted_form_and_full_value_der_hex() {
    // 1.2.3.4 with UTF8String "abc": the complete value TLV is
    // 0C 03 61 62 63, so the text "abc" must NOT appear on its own.
    let subject = name(vec![rdn(vec![atv(
        OID_UNKNOWN,
        &der::utf8_string("abc"),
    )])]);
    let out = expect_success(&certificate(&subject, &example_issuer()));
    let subject_line = line(&out, "Subject: ");
    assert_eq!(subject_line, "Subject: 1.2.3.4=#0C03616263");
    assert!(
        !subject_line.contains("abc"),
        "readable unknown value must not be rendered as text: {subject_line}"
    );
}

#[test]
fn unknown_oid_hex_preserves_tag_long_form_length_and_content() {
    // 200 content bytes force the long-form length 81 C8; the rendered hex
    // must carry tag, that long-form length and every content byte.
    let content = vec![b'A'; 200];
    let value_tlv = der::tlv(0x0C, &content);
    let subject = name(vec![rdn(vec![atv(OID_UNKNOWN_2, &value_tlv)])]);
    let out = expect_success(&certificate(&subject, &example_issuer()));

    let mut expected = String::from("Subject: 1.2.3.5=#0C81C8");
    expected.push_str(&"41".repeat(200));
    assert_eq!(line(&out, "Subject: "), expected);
}

#[test]
fn oid_2_999_keeps_its_full_dotted_number() {
    // 2.999 encodes as 88 37 and must not be collapsed or renamed.
    let subject = name(vec![rdn(vec![atv(
        &[2, 999],
        &der::printable_string("z"),
    )])]);
    let out = expect_success(&certificate(&subject, &example_issuer()));
    assert_eq!(line(&out, "Subject: "), "Subject: 2.999=#13017A");
}

#[test]
fn mixed_known_and_unknown_attributes_keep_their_own_forms() {
    // Encoded order C, O, 1.2.3.4, CN -> displayed reversed. The unknown
    // attribute must stay hex while the neighbors keep text form, and the
    // Chinese O value must pass through as UTF-8 untouched.
    let subject = name(vec![
        rdn(vec![atv(OID_C, &der::printable_string("CN"))]),
        rdn(vec![atv(OID_O, &der::utf8_string("示例公司"))]),
        rdn(vec![atv(OID_UNKNOWN, &der::utf8_string("abc"))]),
        rdn(vec![atv(OID_CN, &der::utf8_string("example.com"))]),
    ]);
    let out = expect_success(&certificate(&subject, &example_issuer()));
    assert_eq!(
        line(&out, "Subject: "),
        "Subject: CN=example.com,1.2.3.4=#0C03616263,O=示例公司,C=CN"
    );
}

#[test]
fn repeated_unknown_attributes_are_not_merged_or_dropped() {
    // Same unknown OID twice, in two distinct RDNs, with different values;
    // both entries survive (in reversed display order).
    let subject = name(vec![
        rdn(vec![atv(OID_CN, &der::utf8_string("host"))]),
        rdn(vec![atv(OID_UNKNOWN, &der::utf8_string("a"))]),
        rdn(vec![atv(OID_UNKNOWN, &der::utf8_string("b"))]),
    ]);
    let out = expect_success(&certificate(&subject, &example_issuer()));
    assert_eq!(
        line(&out, "Subject: "),
        "Subject: 1.2.3.4=#0C0162,1.2.3.4=#0C0161,CN=host"
    );
}

#[test]
fn multi_valued_rdn_is_joined_with_plus_and_not_split() {
    // One RDN carries both a known OU and an unknown OID; a second RDN has
    // the CN. The two attributes must stay in one comma-separated group.
    let subject = name(vec![
        rdn(vec![
            atv(OID_OU, &der::printable_string("Team")),
            atv(OID_UNKNOWN, &der::utf8_string("abc")),
        ]),
        rdn(vec![atv(OID_CN, &der::utf8_string("example.com"))]),
    ]);
    let out = expect_success(&certificate(&subject, &example_issuer()));
    let subject_line = line(&out, "Subject: ");
    assert_eq!(
        subject_line,
        "Subject: CN=example.com,OU=Team+1.2.3.4=#0C03616263"
    );
    // Exactly one comma: two RDNs, the multi-valued RDN was not split apart.
    assert_eq!(subject_line[..].matches(',').count(), 1);
    assert!(subject_line.contains('+'));
}

#[test]
fn rdn_groups_render_in_reverse_encoded_order() {
    let subject = name(vec![
        rdn(vec![atv(OID_C, &der::printable_string("CN"))]),
        rdn(vec![atv(OID_UNKNOWN, &der::utf8_string("abc"))]),
        rdn(vec![atv(OID_CN, &der::utf8_string("example.com"))]),
    ]);
    let out = expect_success(&certificate(&subject, &example_issuer()));
    assert_eq!(
        line(&out, "Subject: "),
        "Subject: CN=example.com,1.2.3.4=#0C03616263,C=CN"
    );
}

#[test]
fn empty_subject_keeps_an_empty_field_while_issuer_still_renders() {
    let out = expect_success(&certificate(&der::tlv(0x30, &[]), &example_issuer()));
    assert!(out.starts_with("Subject: \n"));
    assert_eq!(
        line(&out, "Issuer: "),
        "Issuer: CN=Example CA,O=示例公司,C=CN"
    );
    // Still five fields in the documented order.
    assert_eq!(
        out.lines().map(|l| l.split_once(':').unwrap().0).collect::<Vec<_>>(),
        vec!["Subject", "Issuer", "Serial Number", "Not Before", "Not After"]
    );
}

#[test]
fn subject_and_issuer_reflect_their_own_names() {
    // The unknown attribute exists only in the subject; it must not leak
    // into the issuer line (or vice versa).
    let subject = name(vec![
        rdn(vec![atv(OID_UNKNOWN, &der::utf8_string("abc"))]),
        rdn(vec![atv(OID_CN, &der::utf8_string("subject.example"))]),
    ]);
    let issuer = name(vec![
        rdn(vec![atv(OID_C, &der::printable_string("CN"))]),
        rdn(vec![atv(OID_CN, &der::utf8_string("Example CA"))]),
    ]);
    let out = expect_success(&certificate(&subject, &issuer));
    assert_eq!(
        line(&out, "Subject: "),
        "Subject: CN=subject.example,1.2.3.4=#0C03616263"
    );
    assert_eq!(line(&out, "Issuer: "), "Issuer: CN=Example CA,C=CN");
}

// ---------------------------------------------------------------------------
// Failure cases: malformed unknown-attribute values
// ---------------------------------------------------------------------------

#[test]
fn invalid_utf8_in_unknown_utf8string_rejects_the_certificate() {
    let broken_value = der::tlv(0x0C, &[0xFF, 0xFE]);
    let subject = name(vec![rdn(vec![atv(OID_UNKNOWN, &broken_value)])]);
    expect_invalid_certificate(&certificate(&subject, &example_issuer()));
}

#[test]
fn truncated_unknown_value_tlv_rejects_the_certificate() {
    // Consistent outer lengths, but the value TLV itself claims five content
    // bytes while only two are present.
    let mut atv_content = der::oid(OID_UNKNOWN);
    atv_content.extend_from_slice(&[0x0C, 0x05, 0x61, 0x62]);
    let broken_atv = der::tlv(0x30, &atv_content);
    let subject = name(vec![rdn(vec![broken_atv])]);
    expect_invalid_certificate(&certificate(&subject, &example_issuer()));
}

#[test]
fn non_minimal_length_encoding_in_unknown_value_rejects_the_certificate() {
    // 0C 81 03 61 62 63: long-form length used for a 3-byte content.
    let mut atv_content = der::oid(OID_UNKNOWN);
    atv_content.extend_from_slice(&[0x0C, 0x81, 0x03, b'a', b'b', b'c']);
    let broken_atv = der::tlv(0x30, &atv_content);
    let subject = name(vec![rdn(vec![broken_atv])]);
    expect_invalid_certificate(&certificate(&subject, &example_issuer()));
}

// ---------------------------------------------------------------------------
// Command usage / output conventions
// ---------------------------------------------------------------------------

#[test]
fn version_flag_prints_the_version() {
    let out = Command::new(BIN).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, b"chainview 0.1.0\n");
    assert!(out.stderr.is_empty());
}

#[test]
fn inspect_without_path_is_a_usage_error() {
    let out = Command::new(BIN).arg("inspect").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "Usage: chainview inspect <DER-FILE>\n       chainview --version\n"
    );
}

#[test]
fn inspect_missing_file_reports_the_documented_read_error() {
    let path = std::env::temp_dir().join("chainview-regression-no-such-file.der");
    let _ = std::fs::remove_file(&path);
    let out = Command::new(BIN)
        .arg("inspect")
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.starts_with("chainview: cannot read certificate file '"),
        "unexpected stderr: {err:?}"
    );
    assert!(err.contains("chainview-regression-no-such-file.der"));
    assert!(err.ends_with('\n'));
}
