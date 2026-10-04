use std::env;
use std::fs;
use std::process::ExitCode;

const VERSION: &str = "chainview 0.1.0";

const USAGE: &str = "\
Usage: chainview inspect <file>
       chainview --version";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [arg] if arg == "--version" => {
            println!("{VERSION}");
            ExitCode::SUCCESS
        }
        [cmd, path] if cmd == "inspect" => inspect(path),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn inspect(path: &str) -> ExitCode {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(err) => {
            eprintln!("chainview: cannot read file '{path}': {err}");
            return ExitCode::from(2);
        }
    };

    match cert::parse_certificate(&data) {
        Ok(info) => {
            println!("Subject: {}", info.subject);
            println!("Issuer: {}", info.issuer);
            println!("Serial Number: {}", info.serial);
            println!("Not Before: {}", info.not_before);
            println!("Not After: {}", info.not_after);
            ExitCode::SUCCESS
        }
        Err(reason) => {
            eprintln!("chainview: invalid DER certificate in '{path}': {reason}");
            ExitCode::from(2)
        }
    }
}

/// A minimal, strict DER decoder for the handful of X.509 structures the
/// `inspect` command displays. Everything is zero-copy; malformed input is an
/// error rather than best-effort output.
mod cert {
    use std::convert::TryInto;

    const TAG_INTEGER: u8 = 0x02;
    const TAG_OID: u8 = 0x06;
    const TAG_UTF8_STRING: u8 = 0x0C;
    const TAG_PRINTABLE_STRING: u8 = 0x13;
    const TAG_TELETEX_STRING: u8 = 0x14;
    const TAG_IA5_STRING: u8 = 0x16;
    const TAG_UTC_TIME: u8 = 0x17;
    const TAG_GENERALIZED_TIME: u8 = 0x18;
    const TAG_UNIVERSAL_STRING: u8 = 0x1C;
    const TAG_BMP_STRING: u8 = 0x1E;
    const TAG_SEQUENCE: u8 = 0x30;
    const TAG_SET: u8 = 0x31;

    pub struct CertInfo {
        pub subject: String,
        pub issuer: String,
        pub serial: String,
        pub not_before: String,
        pub not_after: String,
    }

    struct Tlv<'a> {
        tag: u8,
        content: &'a [u8],
        rest: &'a [u8],
    }

    /// Parse one DER TLV. Rejects BER-only forms (indefinite length,
    /// non-minimal lengths, high-tag-number form).
    fn read_tlv(input: &[u8]) -> Result<Tlv<'_>, String> {
        let (&tag, after_tag) = input
            .split_first()
            .ok_or_else(|| "unexpected end of data: expected DER TLV".to_string())?;
        if tag & 0x1F == 0x1F {
            return Err("high-tag-number form is not supported".to_string());
        }

        let (&len_byte, mut p) = after_tag
            .split_first()
            .ok_or_else(|| "truncated DER length".to_string())?;

        let length = if len_byte & 0x80 == 0 {
            usize::from(len_byte)
        } else {
            let n = usize::from(len_byte & 0x7F);
            if n == 0 {
                return Err("indefinite length is not valid DER".to_string());
            }
            if n > 8 {
                return Err("DER length is too large".to_string());
            }
            if p.len() < n {
                return Err("truncated DER length".to_string());
            }
            if p[0] == 0x00 {
                return Err("non-minimal DER length encoding".to_string());
            }
            if n == 1 && p[0] < 0x80 {
                return Err("non-minimal DER length encoding".to_string());
            }
            let mut value: u64 = 0;
            for &b in &p[..n] {
                value = value
                    .checked_shl(8)
                    .and_then(|v| v.checked_add(u64::from(b)))
                    .ok_or_else(|| "DER length is too large".to_string())?;
            }
            let length = value
                .try_into()
                .map_err(|_| "DER length exceeds platform usize".to_string())?;
            p = &p[n..];
            length
        };

        if p.len() < length {
            return Err("truncated DER content".to_string());
        }
        let (content, rest) = p.split_at(length);
        Ok(Tlv { tag, content, rest })
    }

    fn expect_tag<'a>(input: &'a [u8], tag: u8, what: &str) -> Result<Tlv<'a>, String> {
        let tlv = read_tlv(input)?;
        if tlv.tag == tag {
            Ok(tlv)
        } else {
            Err(format!(
                "{what}: expected tag {:#04x}, found {:#04x}",
                tag, tlv.tag
            ))
        }
    }

    /// Validate the contents of a DER INTEGER: non-empty and minimal length.
    fn check_integer(bytes: &[u8]) -> Result<(), String> {
        match bytes {
            [] => Err("empty INTEGER".to_string()),
            [0x00, second, ..] if *second < 0x80 => {
                Err("non-minimal INTEGER encoding".to_string())
            }
            [0xFF, second, ..] if *second >= 0x80 => {
                Err("non-minimal INTEGER encoding".to_string())
            }
            _ => Ok(()),
        }
    }

    pub fn parse_certificate(data: &[u8]) -> Result<CertInfo, String> {
        // Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signature }
        let cert = expect_tag(data, TAG_SEQUENCE, "Certificate")?;
        if !cert.rest.is_empty() {
            return Err("trailing bytes after certificate".to_string());
        }

        let tbs = expect_tag(cert.content, TAG_SEQUENCE, "TBSCertificate")?;
        let mut p = tbs.content;

        // version [0] EXPLICIT Version DEFAULT v1
        if !p.is_empty() && p[0] == 0xA0 {
            let wrapper = read_tlv(p)?;
            let version = expect_tag(wrapper.content, TAG_INTEGER, "version")?;
            if !version.rest.is_empty() {
                return Err("version wrapper contains extra data".to_string());
            }
            check_integer(version.content)?;
            let value = unsigned_integer(version.content)?;
            if value > 2 {
                return Err(format!("unsupported certificate version {}", value + 1));
            }
            p = wrapper.rest;
        }

        // serialNumber CertificateSerialNumber (INTEGER)
        let serial = expect_tag(p, TAG_INTEGER, "serialNumber")?;
        check_integer(serial.content)?;
        // RFC 5280 requires a positive serial number; the leading 0x00 octet
        // present when the high bit is set is only sign padding.
        let serial_bytes = serial.content;
        if serial_bytes[0] & 0x80 != 0 {
            return Err("certificate serial number must be positive".to_string());
        }
        let serial_hex = match serial_bytes {
            [0x00, rest @ ..] if !rest.is_empty() && rest[0] & 0x80 != 0 => {
                rest.iter().map(|b| format!("{b:02X}")).collect::<String>()
            }
            _ => serial_bytes
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<String>(),
        };
        p = serial.rest;

        // signature AlgorithmIdentifier
        let sig_alg_inner = expect_tag(p, TAG_SEQUENCE, "signature AlgorithmIdentifier")?;
        p = sig_alg_inner.rest;

        // issuer Name
        let issuer_tlv = expect_tag(p, TAG_SEQUENCE, "issuer")?;
        let issuer = format_name(issuer_tlv.content)?;
        p = issuer_tlv.rest;

        // validity Validity
        let validity = expect_tag(p, TAG_SEQUENCE, "validity")?;
        let (not_before, not_after) = parse_validity(validity.content)?;
        p = validity.rest;

        // subject Name (may legitimately be empty)
        let subject_tlv = expect_tag(p, TAG_SEQUENCE, "subject")?;
        let subject = format_name(subject_tlv.content)?;
        p = subject_tlv.rest;

        // subjectPublicKeyInfo SubjectPublicKeyInfo
        let spki = expect_tag(p, TAG_SEQUENCE, "subjectPublicKeyInfo")?;
        p = spki.rest;

        // Optional issuerUniqueID [1], subjectUniqueID [2], extensions [3],
        // in that order, each at most once.
        let mut max_optional: Option<u8> = None;
        while !p.is_empty() {
            let tlv = read_tlv(p)?;
            let order = match tlv.tag {
                0x81 => 0, // issuerUniqueID
                0x82 => 1, // subjectUniqueID
                0xA3 => 2, // extensions
                other => {
                    return Err(format!("unexpected tag {:#04x} in TBSCertificate", other))
                }
            };
            if max_optional.is_some_and(|m| order <= m) {
                return Err("TBSCertificate optional fields are out of order or repeated".to_string());
            }
            max_optional = Some(order);
            p = tlv.rest;
        }

        // signatureAlgorithm AlgorithmIdentifier + signatureValue BIT STRING
        let sig_alg_outer = expect_tag(tbs.rest, TAG_SEQUENCE, "signatureAlgorithm")?;
        let signature = expect_tag(sig_alg_outer.rest, TAG_BIT_STRING, "signatureValue")?;
        if !signature.rest.is_empty() {
            return Err("trailing bytes after signatureValue".to_string());
        }
        match signature.content.first() {
            Some(0) => {}
            Some(n) => return Err(format!("signature BIT STRING has {n} unused bits")),
            None => return Err("empty signature BIT STRING".to_string()),
        }

        Ok(CertInfo {
            subject,
            issuer,
            serial: serial_hex,
            not_before,
            not_after,
        })
    }

    const TAG_BIT_STRING: u8 = 0x03;

    fn unsigned_integer(bytes: &[u8]) -> Result<u128, String> {
        let mut value: u128 = 0;
        for &b in bytes {
            value = value
                .checked_shl(8)
                .and_then(|v| v.checked_add(u128::from(b)))
                .ok_or_else(|| "INTEGER value too large".to_string())?;
        }
        Ok(value)
    }

    /// Render an RDNSequence as RFC 4514 text: RDNs in reverse order,
    /// multi-valued RDNs joined with '+', repeated attributes preserved.
    fn format_name(content: &[u8]) -> Result<String, String> {
        let mut rdns: Vec<String> = Vec::new();
        let mut p = content;
        while !p.is_empty() {
            let rdn = expect_tag(p, TAG_SET, "RelativeDistinguishedName")?;
            if rdn.content.is_empty() {
                return Err("empty RelativeDistinguishedName".to_string());
            }
            let mut parts: Vec<String> = Vec::new();
            let mut q = rdn.content;
            while !q.is_empty() {
                let atv = expect_tag(q, TAG_SEQUENCE, "AttributeTypeAndValue")?;
                let oid_tlv = expect_tag(atv.content, TAG_OID, "attribute type")?;
                let value_tlv = read_tlv(oid_tlv.rest)?;
                if !value_tlv.rest.is_empty() {
                    return Err("trailing bytes in AttributeTypeAndValue".to_string());
                }
                let label = oid_label(&format_oid(oid_tlv.content)?);
                let value = decode_directory_string(value_tlv.tag, value_tlv.content)?;
                parts.push(format!("{label}={}", escape_rdn_value(&value)));
                q = atv.rest;
            }
            rdns.push(parts.join("+"));
            p = rdn.rest;
        }
        rdns.reverse();
        Ok(rdns.join(","))
    }

    /// Escape an attribute value per RFC 4514 so each field stays on one line
    /// and non-ASCII content is passed through untouched.
    fn escape_rdn_value(value: &str) -> String {
        let chars: Vec<char> = value.chars().collect();
        let last = chars.len().saturating_sub(1);
        let mut out = String::with_capacity(value.len());
        for (i, c) in chars.iter().enumerate() {
            let needs_quote = matches!(c, ',' | '+' | '"' | '\\' | '<' | '>' | ';')
                || c.is_control()
                || (i == 0 && (*c == '#' || *c == ' '))
                || (i == last && *c == ' ');
            if needs_quote {
                if c.is_control() {
                    let mut buf = [0u8; 4];
                    let n = c.encode_utf8(&mut buf).len();
                    for &b in &buf[..n] {
                        out.push_str(&format!("\\{b:02X}"));
                    }
                } else {
                    out.push('\\');
                    out.push(*c);
                }
            } else {
                out.push(*c);
            }
        }
        out
    }

    fn decode_directory_string(tag: u8, bytes: &[u8]) -> Result<String, String> {
        match tag {
            TAG_UTF8_STRING => std::str::from_utf8(bytes)
                .map(String::from)
                .map_err(|_| "invalid UTF-8 in UTF8String".to_string()),
            // PrintableString and IA5String are subsets of ASCII; accept any
            // ASCII byte rather than rejecting certificates over a strict
            // character-set technicality.
            TAG_PRINTABLE_STRING | TAG_IA5_STRING => {
                if bytes.iter().all(|&b| b < 0x80) {
                    Ok(bytes.iter().map(|&b| b as char).collect())
                } else {
                    Err("non-ASCII byte in ASCII string".to_string())
                }
            }
            // T61/TeletexString in certificates is effectively Latin-1 in
            // practice; Latin-1 maps every byte 1:1 so nothing is lost.
            TAG_TELETEX_STRING => Ok(bytes.iter().map(|&b| b as char).collect()),
            TAG_BMP_STRING => decode_utf16_be(bytes),
            TAG_UNIVERSAL_STRING => decode_utf32_be(bytes),
            other => Err(format!("unsupported attribute value tag {other:#04x}")),
        }
    }

    fn decode_utf16_be(bytes: &[u8]) -> Result<String, String> {
        if bytes.len() % 2 != 0 {
            return Err("BMPString has an odd number of bytes".to_string());
        }
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).map_err(|_| "invalid UTF-16 content in BMPString".to_string())
    }

    fn decode_utf32_be(bytes: &[u8]) -> Result<String, String> {
        if bytes.len() % 4 != 0 {
            return Err("UniversalString length is not a multiple of 4".to_string());
        }
        let mut out = String::new();
        for c in bytes.chunks_exact(4) {
            let code = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
            if (0xD800..=0xDFFF).contains(&code) || code > 0x10FFFF {
                return Err("invalid code point in UniversalString".to_string());
            }
            out.push(char::from_u32(code).unwrap());
        }
        Ok(out)
    }

    fn format_oid(bytes: &[u8]) -> Result<String, String> {
        let first = *bytes
            .first()
            .ok_or_else(|| "empty OBJECT IDENTIFIER".to_string())?;
        let (first_arc, second_arc) = if first < 40 {
            (0u8, first)
        } else if first < 80 {
            (1, first - 40)
        } else {
            (2, first - 80)
        };
        let mut out = format!("{first_arc}.{second_arc}");

        let mut i = 1;
        while i < bytes.len() {
            let mut value: u128 = 0;
            let mut component_bytes = 0;
            loop {
                let &b = bytes
                    .get(i)
                    .ok_or_else(|| "truncated OBJECT IDENTIFIER component".to_string())?;
                i += 1;
                if component_bytes == 0 && b == 0x80 {
                    return Err("non-minimal OBJECT IDENTIFIER encoding".to_string());
                }
                component_bytes += 1;
                value = value
                    .checked_mul(128)
                    .and_then(|v| v.checked_add(u128::from(b & 0x7F)))
                    .ok_or_else(|| "OBJECT IDENTIFIER component overflow".to_string())?;
                if b & 0x80 == 0 {
                    break;
                }
            }
            out.push('.');
            out.push_str(&value.to_string());
        }
        Ok(out)
    }

    fn oid_label(oid: &str) -> String {
        let label = match oid {
            "2.5.4.3" => "CN",
            "2.5.4.4" => "SN",
            "2.5.4.5" => "serialNumber",
            "2.5.4.6" => "C",
            "2.5.4.7" => "L",
            "2.5.4.8" => "ST",
            "2.5.4.9" => "STREET",
            "2.5.4.10" => "O",
            "2.5.4.11" => "OU",
            "2.5.4.12" => "title",
            "2.5.4.42" => "GN",
            "2.5.4.44" => "generationQualifier",
            "1.2.840.113549.1.9.1" => "emailAddress",
            "0.9.2342.19200300.100.1.1" => "UID",
            "0.9.2342.19200300.100.1.25" => "DC",
            _ => return oid.to_string(),
        };
        label.to_string()
    }

    fn parse_validity(content: &[u8]) -> Result<(String, String), String> {
        let not_before_tlv = read_tlv(content)?;
        let not_after_tlv = read_tlv(not_before_tlv.rest)?;
        if !not_after_tlv.rest.is_empty() {
            return Err("trailing bytes in Validity".to_string());
        }
        let before = parse_time(not_before_tlv.tag, not_before_tlv.content)?;
        let after = parse_time(not_after_tlv.tag, not_after_tlv.content)?;
        Ok((before, after))
    }

    /// Parse UTCTime (YYMMDDHHMMSSZ) or GeneralizedTime (YYYYMMDDHHMMSSZ),
    /// always the trailing-Z form required by RFC 5280, and render as
    /// YYYY-MM-DDTHH:MM:SSZ. The values are UTC by definition, so the local
    /// time zone never enters into it.
    fn parse_time(tag: u8, bytes: &[u8]) -> Result<String, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "time field is not ASCII")?;

        let (year, rest) = match tag {
            TAG_UTC_TIME => {
                if text.len() != 13 || !text.ends_with('Z') {
                    return Err("UTCTime must be YYMMDDHHMMSSZ".to_string());
                }
                let yy = two_digits(&text[0..2], "year")?;
                let year = i64::from(if yy < 50 { 2000 + yy } else { 1900 + yy });
                (year, &text[2..12])
            }
            TAG_GENERALIZED_TIME => {
                if text.len() != 15 || !text.ends_with('Z') {
                    return Err("GeneralizedTime must be YYYYMMDDHHMMSSZ".to_string());
                }
                let year = four_digits(&text[0..4], "year")?;
                (i64::from(year), &text[4..14])
            }
            other => return Err(format!("expected time tag, found {other:#04x}")),
        };

        let month = two_digits(&rest[0..2], "month")?;
        let day = two_digits(&rest[2..4], "day")?;
        let hour = two_digits(&rest[4..6], "hour")?;
        let minute = two_digits(&rest[6..8], "minute")?;
        let second = two_digits(&rest[8..10], "second")?;

        if !(1..=12).contains(&month) {
            return Err(format!("invalid month {month}"));
        }
        if day < 1 || day > days_in_month(year, month) {
            return Err(format!("invalid day {day}"));
        }
        if hour > 23 || minute > 59 || second > 59 {
            return Err("invalid time of day".to_string());
        }

        Ok(format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
        ))
    }

    fn two_digits(s: &str, field: &str) -> Result<u32, String> {
        let digits = s.as_bytes();
        if digits.iter().all(|b| b.is_ascii_digit()) {
            Ok(u32::from(digits[0] - b'0') * 10 + u32::from(digits[1] - b'0'))
        } else {
            Err(format!("non-digit characters in {field}"))
        }
    }

    fn four_digits(s: &str, field: &str) -> Result<u32, String> {
        if s.bytes().all(|b| b.is_ascii_digit()) {
            Ok(s.bytes().fold(0u32, |acc, b| acc * 10 + u32::from(b - b'0')))
        } else {
            Err(format!("non-digit characters in {field}"))
        }
    }

    fn is_leap_year(year: i64) -> bool {
        year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
    }

    fn days_in_month(year: i64, month: u32) -> u32 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => 28 + u32::from(is_leap_year(year)),
            _ => 0,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_utc_time_with_2049_pivot() {
            let t = parse_time(TAG_UTC_TIME, b"490101000000Z").unwrap();
            assert_eq!(t, "2049-01-01T00:00:00Z");
            let t = parse_time(TAG_UTC_TIME, b"500101000000Z").unwrap();
            assert_eq!(t, "1950-01-01T00:00:00Z");
        }

        #[test]
        fn parses_generalized_time_and_validates_calendar() {
            let t = parse_time(TAG_GENERALIZED_TIME, b"20240229123059Z").unwrap();
            assert_eq!(t, "2024-02-29T12:30:59Z");
            assert!(parse_time(TAG_GENERALIZED_TIME, b"20230229123059Z").is_err());
            assert!(parse_time(TAG_UTC_TIME, b"490101000000+00").is_err());
        }

        #[test]
        fn formats_oid() {
            assert_eq!(format_oid(&[0x55, 0x04, 0x03]).unwrap(), "2.5.4.3");
            assert_eq!(
                format_oid(&[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x01]).unwrap(),
                "1.2.840.113549.1.9.1"
            );
            assert!(format_oid(&[0x55, 0x80, 0x04]).is_err());
        }

        #[test]
        fn decodes_bmp_string_with_chinese() {
            let bytes = [0x4E, 0x2D, 0x65, 0x87]; // "中文"
            assert_eq!(decode_directory_string(TAG_BMP_STRING, &bytes).unwrap(), "中文");
        }

        #[test]
        fn empty_name_renders_empty() {
            assert_eq!(format_name(&[]).unwrap(), "");
        }

        #[test]
        fn preserves_repeated_attributes_and_escapes() {
            // SEQUENCE { SET { SEQ { OID CN, UTF8 "a" } }, SET { SEQ { OID CN, UTF8 ",b" } } }
            let der = [
                0x31, 0x0A, 0x30, 0x08, 0x06, 0x03, 0x55, 0x04, 0x03, 0x0C, 0x01, 0x61,
                0x31, 0x0B, 0x30, 0x09, 0x06, 0x03, 0x55, 0x04, 0x03, 0x0C, 0x02, 0x2C, 0x62,
            ];
            // Output is reversed and the comma is escaped so fields stay on one line.
            assert_eq!(format_name(&der).unwrap(), r"CN=\,b,CN=a");
        }
    }
}
