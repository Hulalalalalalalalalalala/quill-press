//! End-to-end regression tests for SubjectPublicKeyInfo decoding when
//! running `chainview inspect <DER-FILE>`, locking the behavior already
//! implemented in `src/main.rs`.
//!
//! Public-key information never appears in the five displayed fields, but it
//! is still read in full: a structurally damaged SubjectPublicKeyInfo must
//! reject the whole certificate even though Subject/Issuer/validity are all
//! readable. The check is purely about the DER encoding — it does not verify
//! that the bytes form a key usable by any algorithm, never verifies a
//! signature and never judges trust.
//!
//! Coverage:
//! - AlgorithmIdentifier: absent parameters, an explicit NULL and any other
//!   single complete parameter TLV are all accepted; unknown OIDs that still
//!   decode are accepted (OIDs are not matched against an allow-list);
//! - subjectPublicKey BIT STRING: zero and nonzero unused-bits counts (with
//!   the declared low bits of the final data byte clear), and the existing
//!   acceptance of an empty bit string carrying only a zero count byte;
//! - structural rejection: missing algorithm/key, wrong element tags,
//!   trailing elements; missing/truncated/non-minimal OIDs; truncated or
//!   repeated parameters; and every malformed BIT STRING form (missing count
//!   byte, count > 7, nonzero count without data, set bits among the unused
//!   trailing bits).
//!
//! Every case drives the real `chainview` binary, so the public contract is
//! locked as a whole: on success exit code 0, empty stderr and exactly the
//! five fixed fields in their fixed order (their content never varies with
//! the public-key encoding); on malformed input exit code 2, completely empty
//! stdout and a `chainview: invalid DER certificate: <reason>` stderr.
//!
//! Certificates are built with the tiny zero-dependency DER constructor at
//! the bottom of this file, which lets individual encodings (including
//! truncated or non-minimal ones) be crafted byte for byte while every
//! enclosing wrapper keeps a correct length.

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

const RSA_ENCRYPTION: &[u64] = &[1, 2, 840, 113549, 1, 1, 1];
const EC_PUBLIC_KEY: &[u64] = &[1, 2, 840, 10045, 2, 1];
const P256_CURVE: &[u64] = &[1, 2, 840, 10045, 3, 1, 7];

// ----- Success: AlgorithmIdentifier parameter forms ------------------------

#[test]
fn spki_without_parameters_is_accepted() {
    // The parameters element is OPTIONAL: an AlgorithmIdentifier carrying
    // only the OID must be accepted exactly like one with an explicit NULL.
    let spki = spki_of(&alg_with_oid_only(RSA_ENCRYPTION), &default_key());
    let (out, _cert) = run_inspect("params-absent", &build_cert(&spki));
    assert_success(&out);
}

#[test]
fn spki_with_explicit_null_parameters_is_accepted() {
    let spki = spki_of(&alg_with_null_params(RSA_ENCRYPTION), &default_key());
    let (out, _cert) = run_inspect("params-null", &build_cert(&spki));
    assert_success(&out);
}

#[test]
fn spki_with_other_single_complete_parameter_value_is_accepted() {
    // Parameters need not be NULL: any one complete DER TLV is read and
    // accepted without being interpreted. An INTEGER parameter is one such
    // value; an embedded OID (the namedCurve form used by EC keys) is another.
    let integer_params = tlv(0x02, &[0x01, 0x00]); // INTEGER 0
    let spki = spki_of(
        &alg_with(RSA_ENCRYPTION, &integer_params),
        &default_key(),
    );
    let (out, _cert) = run_inspect("params-integer", &build_cert(&spki));
    assert_success(&out);

    let oid_params = oid(P256_CURVE);
    let spki = spki_of(&alg_with(EC_PUBLIC_KEY, &oid_params), &default_key());
    let (out, _cert) = run_inspect("params-oid", &build_cert(&spki));
    assert_success(&out);
}

#[test]
fn spki_with_unknown_but_decodable_algorithm_oid_is_accepted() {
    // The tool does not recognize 1.2.3.4 or 1.2.840.99999.7 as key
    // algorithms, but recognition is irrelevant: an OID that decodes by the
    // normal base-128 rules (99999 needs a multi-byte arc) is accepted, both
    // with and without parameters.
    let spki = spki_of(
        &alg_with_null_params(&[1, 2, 3, 4]),
        &default_key(),
    );
    let (out, _cert) = run_inspect("unknown-oid-null", &build_cert(&spki));
    assert_success(&out);

    let spki = spki_of(
        &alg_with_oid_only(&[1, 2, 840, 99999, 7]),
        &default_key(),
    );
    let (out, _cert) = run_inspect("unknown-oid-longarc", &build_cert(&spki));
    assert_success(&out);
}

// ----- Success: BIT STRING unused-bits encoding ----------------------------

#[test]
fn public_key_with_zero_unused_bits_and_data_is_accepted() {
    // Zero unused bits over several data bytes, including a final 0xFF byte
    // whose low bits are all significant: nothing about the payload content
    // is judged (it need not be a real RSA/EC key DER).
    let key = bit_string(&[0x00, 0xAA, 0x55, 0xFF, 0x00]);
    let spki = spki_of(&alg_with_null_params(RSA_ENCRYPTION), &key);
    let (out, _cert) = run_inspect("bits-zero", &build_cert(&spki));
    assert_success(&out);
}

#[test]
fn public_key_with_nonzero_unused_bits_and_clear_low_bits_is_accepted() {
    // Unused counts 1, 3 and 7 all succeed as long as the corresponding low
    // bits of the final data byte are zero. These encodings are checked; the
    // data itself is never required to be a usable public key.
    let cases: &[&[u8]] = &[
        &[0x01, 0x02], // 1 unused bit;  last byte low 1 bit  = 0
        &[0x03, 0xF8], // 3 unused bits; last byte low 3 bits = 000
        &[0x07, 0x80], // 7 unused bits; last byte low 7 bits = 0000000
    ];
    for (i, content) in cases.iter().enumerate() {
        let spki = spki_of(&alg_with_null_params(RSA_ENCRYPTION), &bit_string(content));
        let (out, _cert) = run_inspect(&format!("bits-nonzero-{i}"), &build_cert(&spki));
        assert_success(&out);
    }
}

#[test]
fn public_key_empty_bit_string_with_zero_count_is_accepted() {
    // Existing behavior, pinned deliberately: a BIT STRING holding only the
    // zero unused-bits count (no data at all) is accepted. The check covers
    // encoding well-formedness, not whether a cryptographically usable key
    // exists.
    let spki = spki_of(&alg_with_null_params(RSA_ENCRYPTION), &bit_string(&[0x00]));
    let (out, _cert) = run_inspect("bits-empty", &build_cert(&spki));
    assert_success(&out);
}

// ----- Failure: SubjectPublicKeyInfo structure -----------------------------

#[test]
fn spki_missing_algorithm_identifier_is_rejected() {
    // The BIT STRING on its own, with no leading AlgorithmIdentifier
    // SEQUENCE, must not be skipped even though the key bytes are present.
    let spki = seq(&default_key());
    let (out, _cert) = run_inspect("spki-no-alg", &build_cert(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn spki_missing_public_key_bit_string_is_rejected() {
    let spki = seq(&alg_with_null_params(RSA_ENCRYPTION));
    let (out, _cert) = run_inspect("spki-no-key", &build_cert(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn spki_algorithm_identifier_with_wrong_tag_is_rejected() {
    // A bare OID in the AlgorithmIdentifier position is the wrong element
    // type: the identifier must itself be a SEQUENCE.
    let spki = seq(&concat(&[&oid(RSA_ENCRYPTION), &default_key()]));
    let (out, _cert) = run_inspect("spki-alg-tag", &build_cert(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn spki_public_key_with_wrong_tag_is_rejected() {
    // An OCTET STRING where the subjectPublicKey BIT STRING must stand is a
    // type error, not a key that merely cannot be interpreted.
    let wrong_key = tlv(0x04, &[0x00, 0x01, 0x00]);
    let spki = spki_of(&alg_with_null_params(RSA_ENCRYPTION), &wrong_key);
    let (out, _cert) = run_inspect("spki-key-tag", &build_cert(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn spki_with_extra_element_after_bit_string_is_rejected() {
    // Exactly two elements belong in SubjectPublicKeyInfo; a third TLV after
    // the key (here an INTEGER) makes the certificate corrupt.
    let extra = tlv(0x02, &[0x01, 0x2A]);
    let spki = seq(&concat(&[
        &alg_with_null_params(RSA_ENCRYPTION),
        &default_key(),
        &extra,
    ]));
    let (out, _cert) = run_inspect("spki-trailing", &build_cert(&spki));
    assert_invalid_certificate(&out);
}

// ----- Failure: AlgorithmIdentifier contents -------------------------------

#[test]
fn algorithm_identifier_missing_oid_is_rejected() {
    // An empty AlgorithmIdentifier SEQUENCE has no algorithm OID at all.
    let alg = seq(&[]);
    let spki = spki_of(&alg, &default_key());
    let (out, _cert) = run_inspect("alg-no-oid", &build_cert(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn algorithm_oid_with_truncated_encoding_is_rejected() {
    // A single continuation byte 0x88 with no terminating byte promises
    // another base-128 byte that never arrives. The enclosing SEQUENCEs are
    // correctly sized, so the truncation is detected inside the OID itself.
    let truncated_oid = raw_tlv(0x06, &[0x01], &[0x88]);
    let alg = seq(&truncated_oid);
    let spki = spki_of(&alg, &default_key());
    let (out, _cert) = run_inspect("alg-oid-trunc", &build_cert(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn algorithm_oid_with_nonminimal_encoding_is_rejected() {
    // Arc value 1 encoded as 0x80 0x01 (leading 0x80 continuation byte) is
    // not the shortest possible base-128 encoding and is invalid DER.
    let nonminimal_oid = raw_tlv(0x06, &[0x02], &[0x80, 0x01]);
    let alg = seq(&nonminimal_oid);
    let spki = spki_of(&alg, &default_key());
    let (out, _cert) = run_inspect("alg-oid-nonmin", &build_cert(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn algorithm_parameter_with_truncated_value_is_rejected() {
    // The parameter TLV announces five content bytes but only three follow;
    // a half-read parameter cannot be salvaged by ignoring its tail.
    let truncated_params = raw_tlv(0x04, &[0x05], b"abc");
    let alg = seq(&concat(&[&oid(RSA_ENCRYPTION), &truncated_params]));
    let spki = spki_of(&alg, &default_key());
    let (out, _cert) = run_inspect("alg-params-trunc", &build_cert(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn algorithm_identifier_with_multiple_parameters_is_rejected() {
    // Parameters are one OPTIONAL element, never a sequence of them: a NULL
    // followed by a second TLV must be rejected.
    let null_params = tlv(0x05, &[]);
    let second = tlv(0x02, &[0x01, 0x00]);
    let alg = seq(&concat(&[&oid(RSA_ENCRYPTION), &null_params, &second]));
    let spki = spki_of(&alg, &default_key());
    let (out, _cert) = run_inspect("alg-params-multi", &build_cert(&spki));
    assert_invalid_certificate(&out);
}

// ----- Failure: subjectPublicKey BIT STRING contents -----------------------

#[test]
fn public_key_bit_string_without_unused_bits_byte_is_rejected() {
    // A zero-length BIT STRING content has no leading unused-bits count at
    // all (contrast with [0x00], the empty-but-counted form that is accepted).
    let spki = spki_of(&alg_with_null_params(RSA_ENCRYPTION), &bit_string(&[]));
    let (out, _cert) = run_inspect("bits-no-count", &build_cert(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn public_key_bit_string_with_unused_count_above_seven_is_rejected() {
    // The count is three bits wide; 8 cannot name unused bits in a byte.
    let spki = spki_of(
        &alg_with_null_params(RSA_ENCRYPTION),
        &bit_string(&[0x08, 0x00]),
    );
    let (out, _cert) = run_inspect("bits-count-8", &build_cert(&spki));
    assert_invalid_certificate(&out);
}

#[test]
fn public_key_bit_string_nonzero_count_without_data_is_rejected() {
    // Declaring unused bits requires at least the byte they are unused in:
    // a lone nonzero count names bits that cannot exist.
    for (i, count) in [0x01u8, 0x07].into_iter().enumerate() {
        let spki = spki_of(
            &alg_with_null_params(RSA_ENCRYPTION),
            &bit_string(&[count]),
        );
        let (out, _cert) = run_inspect(&format!("bits-nodata-{i}"), &build_cert(&spki));
        assert_invalid_certificate(&out);
    }
}

#[test]
fn public_key_bit_string_with_set_unused_tail_bits_is_rejected() {
    // With a nonzero count the named low bits of the final data byte must be
    // zero. Each case below has at least one of them set, so the encoding
    // lies about its padding even though the rest of the certificate parses.
    let cases: &[&[u8]] = &[
        &[0x01, 0x01], // 1 unused bit;  low 1 bit set
        &[0x02, 0x03], // 2 unused bits; both low bits set
        &[0x03, 0xF9], // 3 unused bits; lowest of them set (...001)
        &[0x07, 0x81], // 7 unused bits; lowest bit set
        &[0x03, 0x00, 0x01], // padding is judged on the FINAL byte only,
                             // and that final byte still has a set low bit
    ];
    for (i, content) in cases.iter().enumerate() {
        let spki = spki_of(&alg_with_null_params(RSA_ENCRYPTION), &bit_string(content));
        let (out, _cert) = run_inspect(&format!("bits-tailset-{i}"), &build_cert(&spki));
        assert_invalid_certificate(&out);
    }
}

// ----- Assertions / process plumbing --------------------------------------

/// A successful inspect must print exactly the five fixed fields in their
/// fixed order; none of their content varies with the SPKI encoding, and
/// stderr stays empty.
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

/// Every malformed-input case shares the documented contract: exit code 2,
/// nothing at all on stdout (parse failure precedes every print), and the
/// fixed invalid-certificate prefix on stderr with a non-empty reason after
/// it. The reason's exact wording is intentionally not pinned.
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
            "chainview-spki-test-{}-{tag}-{unique}.der",
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
// Only the structural pieces needed here. Lengths are always emitted in the
// minimal DER form, except where a test injects hand-written length or
// content bytes (truncated or non-minimal OID arcs, a truncated parameter);
// such bytes are always wrapped by correctly sized SEQUENCEs so rejection is
// proven to come from the element under test.

/// Assemble a complete, parseable v1 DER certificate with fixed names,
/// serial, validity and signature material around the supplied
/// SubjectPublicKeyInfo TLV.
fn build_cert(spki: &[u8]) -> Vec<u8> {
    let serial = tlv(0x02, &[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]);
    let validity = seq(&concat(&[
        &tlv(0x17, b"260115093000Z"), // UTCTime 2026-01-15T09:30:00Z
        &tlv(0x17, b"270115093000Z"), // UTCTime 2027-01-15T09:30:00Z
    ]));
    let subject = simple_cn_name("example.com");
    let issuer = simple_cn_name("Test CA");

    let tbs = seq(&concat(&[
        &serial,
        &signature_algorithm(),
        &issuer,
        &validity,
        &subject,
        spki,
    ]));
    seq(&concat(&[&tbs, &signature_algorithm(), &tlv(0x03, &[0x00])]))
}

fn signature_algorithm() -> Vec<u8> {
    // sha256WithRSAEncryption with explicit NULL parameters.
    seq(&concat(&[&oid(&[1, 2, 840, 113549, 1, 1, 11]), &tlv(0x05, &[])]))
}

/// SubjectPublicKeyInfo ::= SEQUENCE { algorithm AlgorithmIdentifier,
/// subjectPublicKey BIT STRING }.
fn spki_of(alg: &[u8], key: &[u8]) -> Vec<u8> {
    seq(&concat(&[alg, key]))
}

fn alg_with_oid_only(arcs: &[u64]) -> Vec<u8> {
    seq(&oid(arcs))
}

fn alg_with_null_params(arcs: &[u64]) -> Vec<u8> {
    alg_with(arcs, &tlv(0x05, &[]))
}

/// AlgorithmIdentifier with one caller-supplied complete parameter TLV.
fn alg_with(arcs: &[u64], params: &[u8]) -> Vec<u8> {
    seq(&concat(&[&oid(arcs), params]))
}

/// BIT STRING carrying the supplied content (unused-bits count plus data).
fn bit_string(content: &[u8]) -> Vec<u8> {
    tlv(0x03, content)
}

/// A small, well-formed key BIT STRING: zero unused bits, two data bytes.
fn default_key() -> Vec<u8> {
    bit_string(&[0x00, 0x01, 0x00])
}

fn simple_cn_name(cn: &str) -> Vec<u8> {
    let atv = seq(&concat(&[&oid(&[2, 5, 4, 3]), &tlv(0x0C, cn.as_bytes())]));
    let rdn = tlv(0x31, &atv); // SET { CN=... }
    tlv(0x30, &rdn) // SEQUENCE { RDN }
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
