//! End-to-end regression tests for file paths that are not valid UTF-8.
//!
//! On Unix a file or directory name is an arbitrary byte string and may
//! contain bytes that cannot be decoded as UTF-8. `chainview inspect <path>`
//! must open the path exactly as the operating system received it: relative
//! and absolute paths, and paths with an undecodable byte in an intermediate
//! directory, all reach the file the user named. The undecodable bytes are
//! rendered as the replacement character only inside the human-readable
//! read-error diagnostic; that rendering must never change which file is
//! opened, and it must never make the process panic.
//!
//! Every case here drives the real binary with raw-byte arguments (built via
//! the Unix `OsString`/`OsStr` byte extensions). The file name handling under
//! test is Unix-specific, hence the whole module is `cfg(unix)`.

#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_chainview");

const USAGE: &str = "Usage: chainview inspect <DER-FILE>\n       chainview --version\n";

const EXPECTED_CERT_STDOUT: &str = "Subject: CN=example.com\n\
Issuer: CN=Test CA\n\
Serial Number: 0E8A4C2F9B17D603\n\
Not Before: 2026-01-15T09:30:00Z\n\
Not After: 2027-01-15T09:30:00Z\n";

// ----- Success: the raw OS path reaches the file the user named -------------

#[test]
fn non_utf8_file_name_with_a_valid_cert_succeeds() {
    let dir = TempDir::new("badname-file");
    let path = dir.join_bytes(b"cert-\xff\xfe.der");
    fs::write(&path, valid_cert()).unwrap();

    let out = chainview().arg("inspect").arg(&path).output().unwrap();

    assert_cert_success(&out);
}

#[test]
fn non_utf8_relative_path_succeeds() {
    // A relative argument (with an explicit "./") carrying undecodable bytes
    // is resolved relative to the current directory exactly as typed.
    let dir = TempDir::new("badname-relative");
    let _file = dir.join_bytes(b"rel-\xff.der");
    fs::write(&_file, valid_cert()).unwrap();

    let out = chainview_in(&dir)
        .arg("inspect")
        .arg(os(b"./rel-\xff.der"))
        .output()
        .unwrap();

    assert_cert_success(&out);
}

#[test]
fn non_utf8_absolute_path_succeeds() {
    let dir = TempDir::new("badname-absolute");
    let path = dir.join_bytes(b"abs-\xff.der");
    fs::write(&path, valid_cert()).unwrap();

    let out = chainview().arg("inspect").arg(&path).output().unwrap();

    assert_cert_success(&out);
}

#[test]
fn non_utf8_intermediate_directory_succeeds() {
    // The undecodable byte sits in a directory component, not only in the
    // final file name; every component is passed through untouched.
    let dir = TempDir::new("badname-dir");
    let subdir = dir.join_bytes(b"dir-\xff");
    fs::create_dir(&subdir).unwrap();
    let path = subdir.join(OsStr::from_bytes(b"c.der"));
    fs::write(&path, valid_cert()).unwrap();

    let out = chainview()
        .arg("inspect")
        .arg(os(b"dir-\xff/c.der"))
        .current_dir(&dir)
        .output()
        .unwrap();

    assert_cert_success(&out);
}

#[test]
fn replacement_character_display_name_is_a_different_file() {
    // The raw argument b"cert-\xff.der" displays as "cert-<U+FFFD>.der". A
    // second file whose name is literally that displayed text (the UTF-8
    // encoding of U+FFFD) must NOT be opened: the file the user named holds a
    // valid certificate, while the replacement-named decoy holds garbage.
    // Reading the decoy by mistake would fail as an invalid certificate.
    let dir = TempDir::new("badname-collision");

    let real = dir.join_bytes(b"cert-\xff.der");
    fs::write(&real, valid_cert()).unwrap();

    let decoy = dir.join_bytes("cert-\u{FFFD}.der".as_bytes());
    assert_ne!(
        real.as_os_str().as_bytes(),
        decoy.as_os_str().as_bytes(),
        "the decoy must be a distinct directory entry"
    );
    fs::write(&decoy, b"this is not a certificate").unwrap();

    // The user's raw bytes reach the real, valid certificate.
    let out = chainview()
        .current_dir(&dir)
        .arg("inspect")
        .arg(os(b"cert-\xff.der"))
        .output()
        .unwrap();
    assert_cert_success(&out);

    // Naming the decoy by its real (valid UTF-8) name reads the decoy and
    // fails as corrupt DER, proving the two entries are never conflated.
    let out = chainview()
        .current_dir(&dir)
        .arg("inspect")
        .arg(OsString::from("cert-\u{FFFD}.der"))
        .output()
        .unwrap();
    assert_invalid_certificate(&out);
}

#[test]
fn valid_utf8_cjk_path_keeps_working() {
    // Ordinary non-ASCII but valid UTF-8 names continue to work unchanged.
    let dir = TempDir::new("cjk-name");
    let path = dir.join_bytes("证书.der".as_bytes());
    fs::write(&path, valid_cert()).unwrap();

    let out = chainview()
        .arg("inspect")
        .arg(OsString::from(path.as_os_str().to_owned()))
        .output()
        .unwrap();

    assert_cert_success(&out);
}

// ----- Failure: the path cannot be read -------------------------------------

#[test]
fn missing_non_utf8_path_reports_the_read_error_without_panicking() {
    let dir = TempDir::new("badname-missing");
    let raw = b"missing-\xff.der";
    let path = dir.join_bytes(raw);

    let out = chainview().arg("inspect").arg(&path).output().unwrap();

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    // The diagnostic keeps every representable byte and shows the
    // undecodable one as U+FFFD; this is a lossy *display* of the path,
    // produced only because the read failed.
    let shown = String::from_utf8_lossy(path.as_os_str().as_bytes());
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.starts_with(&format!(
            "chainview: cannot read certificate file '{shown}': "
        )),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("No such file or directory"),
        "the read failure must still carry the OS reason: {stderr}"
    );
    assert!(!stderr.contains("panicked"), "must not crash: {stderr}");
}

#[test]
fn non_utf8_path_that_is_a_directory_reports_the_read_error() {
    // Reading a directory fails just like a missing file: exit 2, empty
    // stdout, the established message with the lossily shown path.
    let dir = TempDir::new("badname-isdir");
    let subdir = dir.join_bytes(b"adirectory-\xff");
    fs::create_dir(&subdir).unwrap();

    let out = chainview().arg("inspect").arg(&subdir).output().unwrap();

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let shown = String::from_utf8_lossy(subdir.as_os_str().as_bytes());
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.starts_with(&format!(
            "chainview: cannot read certificate file '{shown}': "
        )),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("Is a directory"),
        "the read failure must still carry the OS reason: {stderr}"
    );
}

#[test]
fn missing_file_under_non_utf8_directory_keeps_displayable_components() {
    // The bad byte is in the existing intermediate directory while the final
    // component is plain ASCII; both survive in the diagnostic.
    let dir = TempDir::new("badname-missing-under");
    let subdir = dir.join_bytes(b"dir-\xff");
    fs::create_dir(&subdir).unwrap();
    let raw = b"dir-\xff/absent.der";

    let out = chainview()
        .current_dir(&dir)
        .arg("inspect")
        .arg(os(raw))
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let shown = String::from_utf8_lossy(raw);
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.starts_with(&format!(
            "chainview: cannot read certificate file '{shown}': "
        )),
        "unexpected stderr: {stderr}"
    );
}

// ----- Failure: readable file, corrupt content ------------------------------

#[test]
fn corrupt_content_at_non_utf8_path_is_invalid_der_not_a_path_error() {
    // A reachable file whose bytes are not a certificate must stay a
    // certificate error; path encoding must never be mistaken for the cause.
    let dir = TempDir::new("badname-corrupt");
    let path = dir.join_bytes(b"corrupt-\xff.der");
    fs::write(&path, b"\x30\x05garba").unwrap();

    let out = chainview().arg("inspect").arg(&path).output().unwrap();

    assert_invalid_certificate(&out);
    let stderr = lossy(&out.stderr);
    assert!(
        !stderr.contains("cannot read"),
        "corrupt content must not be reported as a path/read error: {stderr}"
    );
}

// ----- Usage rules: undecodable bytes never bypass them ---------------------

#[test]
fn unknown_command_with_non_utf8_bytes_is_a_usage_error() {
    let out = chainview().arg(os(b"frobnicate-\xff")).output().unwrap();
    assert_usage(&out);
}

#[test]
fn extra_non_utf8_argument_is_a_usage_error_and_prints_nothing() {
    // Two paths (the second carrying bad bytes) is a usage error; the valid
    // first file must not be inspected before the error is noticed.
    let dir = TempDir::new("badname-extra");
    let path = dir.join_bytes(b"cert-\xff.der");
    fs::write(&path, valid_cert()).unwrap();

    let out = chainview()
        .arg("inspect")
        .arg(&path)
        .arg(os(b"extra-\xff"))
        .output()
        .unwrap();

    assert_usage(&out);
}

#[test]
fn dash_prefixed_non_utf8_path_is_a_usage_error() {
    let out = chainview()
        .arg("inspect")
        .arg(os(b"-\xff"))
        .output()
        .unwrap();
    assert_usage(&out);
}

#[test]
fn non_utf8_version_extra_argument_is_a_usage_error() {
    // --version takes no arguments; an undecodable extra argument is still a
    // plain usage error, not a version print and not a crash.
    let out = chainview()
        .arg("--version")
        .arg(os(b"-\xff"))
        .output()
        .unwrap();
    assert_usage(&out);
}

#[test]
fn version_output_is_unchanged() {
    let out = chainview().arg("--version").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty());
    assert_eq!(lossy(&out.stdout), "chainview 0.1.0\n");
}

// ----- Assertions / process plumbing ----------------------------------------

fn chainview() -> Command {
    Command::new(BIN)
}

fn chainview_in(dir: impl AsRef<Path>) -> Command {
    let mut c = chainview();
    c.current_dir(dir.as_ref());
    c
}

fn assert_cert_success(out: &Output) {
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={} stderr={}",
        lossy(&out.stdout),
        lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty(), "stderr={}", lossy(&out.stderr));
    assert_eq!(lossy(&out.stdout), EXPECTED_CERT_STDOUT);
}

fn assert_invalid_certificate(out: &Output) {
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = lossy(&out.stderr);
    assert!(
        stderr.starts_with("chainview: invalid DER certificate: "),
        "unexpected stderr: {stderr}"
    );
}

fn assert_usage(out: &Output) {
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout={} stderr={}",
        lossy(&out.stdout),
        lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "stdout must be empty on a usage error, got: {}",
        lossy(&out.stdout)
    );
    assert_eq!(lossy(&out.stderr), USAGE);
    assert!(
        !lossy(&out.stderr).contains("panicked"),
        "undecodable arguments must not crash"
    );
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn os(bytes: &[u8]) -> OsString {
    OsString::from_vec(bytes.to_vec())
}

/// A unique scratch directory removed on drop. Its path is built from raw
/// bytes so a non-UTF-8 temp base would not matter.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut name = b"chainview-rawpaths-".to_vec();
        name.extend_from_slice(tag.as_bytes());
        name.push(b'-');
        name.extend_from_slice(std::process::id().to_string().as_bytes());
        name.push(b'-');
        name.extend_from_slice(unique.to_string().as_bytes());
        let path = std::env::temp_dir().join(OsString::from_vec(name));
        fs::create_dir_all(&path).expect("creating scratch directory");
        TempDir { path }
    }

    fn join_bytes(&self, bytes: &[u8]) -> PathBuf {
        self.path.join(OsStr::from_bytes(bytes))
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// ----- Certificate / DER construction ---------------------------------------
//
// A fixed, strictly valid v3 DER certificate, assembled with the same minimal
// encoder used by the other end-to-end test files. The visible fields are
// fixed so success output can be compared exactly.

fn valid_cert() -> Vec<u8> {
    let serial = tlv(0x02, &[0x0E, 0x8A, 0x4C, 0x2F, 0x9B, 0x17, 0xD6, 0x03]);
    let validity = seq(&concat(&[
        &tlv(0x17, b"260115093000Z"),
        &tlv(0x17, b"270115093000Z"),
    ]));
    let subject = simple_cn_name(b"example.com");
    let issuer = simple_cn_name(b"Test CA");
    let spki = seq(&concat(&[
        &seq(&concat(&[
            &oid(&[1, 2, 840, 113549, 1, 1, 1]),
            &tlv(0x05, &[]),
        ])),
        &tlv(0x03, &[0x00, 0x01, 0x00]),
    ]));
    let alg = signature_algorithm();

    let tbs = seq(&concat(&[
        &tlv(0xA0, &tlv(0x02, &[2])), // explicit v3
        &serial,
        &alg,
        &issuer,
        &validity,
        &subject,
        &spki,
    ]));
    seq(&concat(&[&tbs, &alg, &tlv(0x03, &[0x00])]))
}

fn signature_algorithm() -> Vec<u8> {
    seq(&concat(&[
        &oid(&[1, 2, 840, 113549, 1, 1, 11]),
        &tlv(0x05, &[]),
    ]))
}

fn simple_cn_name(cn: &[u8]) -> Vec<u8> {
    let atv = seq(&concat(&[&oid(&[2, 5, 4, 3]), &tlv(0x0C, cn)]));
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
