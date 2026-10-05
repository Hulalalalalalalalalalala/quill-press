use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(Fail::Usage) => {
            eprintln!("{}", usage());
            ExitCode::from(2)
        }
        Err(Fail::Read(msg)) => {
            eprintln!("chainview: {msg}");
            ExitCode::from(2)
        }
        Err(Fail::Cert(msg)) => {
            eprintln!("chainview: invalid DER certificate: {msg}");
            ExitCode::from(2)
        }
        Err(Fail::Output(msg)) => {
            eprintln!("chainview: write error: {msg}");
            ExitCode::from(2)
        }
    }
}

enum Fail {
    Usage,
    Read(String),
    Cert(String),
    Output(String),
}

fn usage() -> &'static str {
    "Usage: chainview inspect <DER-FILE>\n       chainview --version"
}

fn run() -> Result<(), Fail> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") if args.len() == 1 => {
            use std::io::Write;
            std::io::stdout()
                .lock()
                .write_all(b"chainview 0.1.0\n")
                .map_err(|e| Fail::Output(e.to_string()))
        }
        Some("inspect") => inspect(&args[1..]),
        _ => Err(Fail::Usage),
    }
}

fn inspect(args: &[String]) -> Result<(), Fail> {
    let path = match args {
        [path] if !path.starts_with('-') => path,
        _ => return Err(Fail::Usage),
    };

    let data = fs::read(path).map_err(|e| {
        Fail::Read(format!("cannot read certificate file '{path}': {e}"))
    })?;

    let cert = x509::parse(&data).map_err(Fail::Cert)?;

    let mut out = String::new();
    out.push_str("Subject: ");
    out.push_str(&cert.subject);
    out.push('\n');
    out.push_str("Issuer: ");
    out.push_str(&cert.issuer);
    out.push('\n');
    out.push_str("Serial Number: ");
    out.push_str(&cert.serial_hex);
    out.push('\n');
    out.push_str("Not Before: ");
    out.push_str(&cert.not_before);
    out.push('\n');
    out.push_str("Not After: ");
    out.push_str(&cert.not_after);
    out.push('\n');

    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(out.as_bytes())
        .and_then(|_| stdout.flush())
        .map_err(|e| Fail::Output(e.to_string()))
}

/// Minimal, strict DER decoder and the slice of X.509 needed by `inspect`.
mod x509 {
    use std::fmt::Write;

    pub(super) struct Cert {
        pub(super) subject: String,
        pub(super) issuer: String,
        pub(super) serial_hex: String,
        pub(super) not_before: String,
        pub(super) not_after: String,
    }

    #[derive(Clone, Copy)]
    struct Tlv<'a> {
        tag: u8,
        content: &'a [u8],
    }

    const TAG_INTEGER: u8 = 0x02;
    const TAG_BIT_STRING: u8 = 0x03;
    const TAG_OCTET_STRING: u8 = 0x04;
    const TAG_BOOLEAN: u8 = 0x01;
    const TAG_OID: u8 = 0x06;
    const TAG_SEQUENCE: u8 = 0x30;
    const TAG_SET: u8 = 0x31;
    const TAG_UTC_TIME: u8 = 0x17;
    const TAG_GENERALIZED_TIME: u8 = 0x18;

    /// Read exactly one DER TLV. Rejects indefinite length, non-minimal
    /// length encodings and truncated data.
    fn read_tlv(buf: &[u8]) -> Result<(Tlv<'_>, &[u8]), String> {
        let tag = *buf.first().ok_or_else(|| "unexpected end of data".to_string())?;
        if tag & 0x1F == 0x1F {
            return Err("multi-byte tags are not supported".to_string());
        }
        let first_len = *buf.get(1).ok_or_else(|| "truncated length".to_string())?;
        let (len, header_len) = match first_len {
            0x00..=0x7F => (first_len as usize, 2),
            0x80 => return Err("indefinite length is not valid DER".to_string()),
            0xFF => return Err("reserved length byte".to_string()),
            long => {
                let n = (long & 0x7F) as usize;
                if !(1..=4).contains(&n) {
                    return Err("length is too large".to_string());
                }
                let bytes = buf
                    .get(2..2 + n)
                    .ok_or_else(|| "truncated long length".to_string())?;
                if bytes[0] == 0 {
                    return Err("non-minimal length encoding".to_string());
                }
                let mut len = 0usize;
                for &b in bytes {
                    len = len
                        .checked_shl(8)
                        .and_then(|l| l.checked_add(b as usize))
                        .ok_or_else(|| "length overflow".to_string())?;
                }
                if len < 0x80 {
                    return Err("non-minimal length encoding".to_string());
                }
                (len, 2 + n)
            }
        };
        let total = header_len
            .checked_add(len)
            .filter(|&t| t <= buf.len())
            .ok_or_else(|| "truncated TLV content".to_string())?;
        Ok((
            Tlv {
                tag,
                content: &buf[header_len..total],
            },
            &buf[total..],
        ))
    }

    fn read_tagged<'a>(
        buf: &'a [u8],
        tag: u8,
        what: &str,
    ) -> Result<(Tlv<'a>, &'a [u8]), String> {
        let (tlv, rest) = read_tlv(buf).map_err(|e| format!("{what}: {e}"))?;
        if tlv.tag != tag {
            return Err(format!("expected {what} (tag 0x{tag:02X}), got 0x{:02X}", tlv.tag));
        }
        Ok((tlv, rest))
    }

    pub(super) fn parse(bytes: &[u8]) -> Result<Cert, String> {
        // Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm,
        // signatureValue BIT STRING } — and nothing trailing.
        let (cert, rest) = read_tagged(bytes, TAG_SEQUENCE, "Certificate SEQUENCE")?;
        if !rest.is_empty() {
            return Err(format!("{} trailing byte(s) after certificate", rest.len()));
        }

        let c = cert.content;
        let (tbs, c) = read_tagged(c, TAG_SEQUENCE, "tbsCertificate")?;
        let (_sig_alg, c) = read_tagged(c, TAG_SEQUENCE, "signatureAlgorithm")?;
        let (sig_value, c) = read_tlv(c)?;
        if sig_value.tag != TAG_BIT_STRING {
            return Err("signature must be a BIT STRING".to_string());
        }
        if sig_value.content.is_empty() || sig_value.content[0] > 7 {
            return Err("invalid signature BIT STRING".to_string());
        }
        if !c.is_empty() {
            return Err("trailing data in Certificate".to_string());
        }

        // TBSCertificate ::= SEQUENCE {
        //   version [0] EXPLICIT Version DEFAULT v1, serialNumber, signature,
        //   issuer, validity, subject, subjectPublicKeyInfo, ... }
        let t = tbs.content;
        let (first, t2) = read_tlv(t)?;
        let (version, t) = if first.tag == 0xA0 {
            let (ver, vrest) = read_tagged(first.content, TAG_INTEGER, "version")?;
            if !vrest.is_empty() || ver.content.len() != 1 || ver.content[0] > 2 {
                return Err("invalid certificate version".to_string());
            }
            (ver.content[0], t2)
        } else {
            // No explicit version: v1 certificate.
            (0u8, t)
        };

        let (serial, t3) = read_tagged(t, TAG_INTEGER, "serialNumber")?;
        let t = t3;
        let (_tbs_sig, t) = read_tagged(t, TAG_SEQUENCE, "tbs signatureAlgorithm")?;
        let (issuer, t) = read_tagged(t, TAG_SEQUENCE, "issuer")?;
        let (validity, t) = read_tagged(t, TAG_SEQUENCE, "validity")?;
        let (subject, t) = read_tagged(t, TAG_SEQUENCE, "subject")?;
        let (spki, t_after_spki) = read_tagged(t, TAG_SEQUENCE, "subjectPublicKeyInfo")?;
        check_spki(spki)?;
        // Optional tail of TBSCertificate, in a fixed order:
        //   issuerUniqueID  [1] IMPLICIT BIT STRING  (v2/v3, at most once)
        //   subjectUniqueID [2] IMPLICIT BIT STRING  (v2/v3, at most once)
        //   extensions      [3] EXPLICIT             (v3 only, at most once,
        //                                             and the last field)
        // Nothing else may follow subjectPublicKeyInfo: fields cannot be
        // skipped, reordered or dropped. The extensions interior is decoded in
        // full by `check_extensions`; the unique IDs carry BIT STRING content
        // directly (implicit tagging), validated by `check_bit_string`.
        let mut t = t_after_spki;
        let mut issuer_uid_seen = false;
        let mut subject_uid_seen = false;
        let mut extensions_present = false;
        while !t.is_empty() {
            let tag_hint = t.first().copied();
            let (field, rest) = read_tlv(t).map_err(|e| match tag_hint {
                Some(0x81) => format!("issuerUniqueID: {e}"),
                Some(0x82) => format!("subjectUniqueID: {e}"),
                // A truncated field whose tag is already readable can still
                // be located: tag 0xA3 is the extensions field.
                Some(0xA3) => format!("extensions field: {e}"),
                _ => e,
            })?;
            match field.tag {
                0x81 | 0x82 => {
                    let what = if field.tag == 0x81 {
                        "issuerUniqueID"
                    } else {
                        "subjectUniqueID"
                    };
                    if version < 1 {
                        return Err(format!(
                            "{what} is only allowed in v2 or v3 certificates, this is v1"
                        ));
                    }
                    if extensions_present {
                        return Err(format!(
                            "{what} must appear before the extensions field, \
                             which is the last field of tbsCertificate"
                        ));
                    }
                    if field.tag == 0x81 {
                        if issuer_uid_seen {
                            return Err("duplicate issuerUniqueID field".to_string());
                        }
                        if subject_uid_seen {
                            return Err(
                                "issuerUniqueID must appear before subjectUniqueID".to_string()
                            );
                        }
                        issuer_uid_seen = true;
                    } else if subject_uid_seen {
                        return Err("duplicate subjectUniqueID field".to_string());
                    } else {
                        subject_uid_seen = true;
                    }
                    check_bit_string(field.content, what)?;
                }
                0xA1 | 0xA2 => {
                    let what = if field.tag == 0xA1 {
                        "issuerUniqueID"
                    } else {
                        "subjectUniqueID"
                    };
                    return Err(format!(
                        "{what} must be an implicitly tagged primitive BIT STRING \
                         (tag 0x{:02X}); constructed tag 0x{:02X} is an explicit wrapper \
                         and is not allowed",
                        field.tag - 0x20,
                        field.tag
                    ));
                }
                0xA3 => {
                    if extensions_present {
                        return Err("duplicate extensions field".to_string());
                    }
                    if version != 2 {
                        let name = if version == 0 {
                            "v1"
                        } else {
                            "v2"
                        };
                        return Err(format!(
                            "extensions are only allowed in v3 certificates, this is {name}"
                        ));
                    }
                    check_extensions(field.content)?;
                    extensions_present = true;
                }
                other => {
                    return Err(format!(
                        "unexpected field after subjectPublicKeyInfo (tag 0x{other:02X}): \
                         only issuerUniqueID [1], subjectUniqueID [2] and extensions [3] \
                         are allowed there, and extensions must be last"
                    ));
                }
            }
            t = rest;
        }

        let serial_hex = format_serial(serial.content)?;
        let issuer = format_name(issuer)?;
        let subject = format_name(subject)?;

        let v = validity.content;
        let (nb, v2) = read_tlv(v)?;
        let (na, v3) = read_tlv(v2)?;
        if !v3.is_empty() {
            return Err("trailing data in validity".to_string());
        }
        let not_before = format_time(nb)?;
        let not_after = format_time(na)?;

        Ok(Cert {
            subject,
            issuer,
            serial_hex,
            not_before,
            not_after,
        })
    }

    /// Validate the content of the extensions field, i.e. the bytes wrapped
    /// by `[3] EXPLICIT`:
    ///
    /// ```text
    /// Extensions ::= SEQUENCE SIZE (1..MAX) OF Extension
    /// Extension  ::= SEQUENCE {
    ///   extnID    OBJECT IDENTIFIER,
    ///   critical  BOOLEAN DEFAULT FALSE,
    ///   extnValue OCTET STRING }
    /// ```
    ///
    /// The wrapper must contain exactly one non-empty Extensions SEQUENCE.
    /// Every extension must contain its OID and OCTET STRING in that order,
    /// with at most one (DER-encoded) BOOLEAN between them and nothing after.
    /// Extension contents are never interpreted: unknown but well-formed
    /// extension OIDs are accepted and `critical: TRUE` is not a failure.
    fn check_extensions(wrapper: &[u8]) -> Result<(), String> {
        let (list, rest) = read_tagged(wrapper, TAG_SEQUENCE, "extensions")?;
        if !rest.is_empty() {
            return Err("extensions wrapper contains data after the Extensions SEQUENCE".to_string());
        }
        if list.content.is_empty() {
            return Err("extensions SEQUENCE is empty".to_string());
        }

        let mut buf = list.content;
        while !buf.is_empty() {
            let (ext, rest) = read_tagged(buf, TAG_SEQUENCE, "extension")?;
            buf = rest;

            let (oid, after_oid) = read_tagged(ext.content, TAG_OID, "extension OID")?;
            parse_oid(oid.content).map_err(|e| format!("extension OID: {e}"))?;

            // critical is optional; whatever stands between the OID and the
            // OCTET STRING must be exactly one BOOLEAN if it is present.
            let (maybe_bool, after_critical) = read_tlv(after_oid)
                .map_err(|e| format!("extension after OID: {e}"))?;
            let (octets, after_octets) = if maybe_bool.tag == TAG_BOOLEAN {
                match maybe_bool.content {
                    [0xFF] => {}
                    [0x00] => {
                        return Err(
                            "extension critical: explicit DER encoding of DEFAULT FALSE is not allowed"
                                .to_string()
                        )
                    }
                    _ => {
                        return Err(
                            "extension critical: invalid DER encoding of BOOLEAN".to_string()
                        )
                    }
                }
                read_tagged(after_critical, TAG_OCTET_STRING, "extension value")?
            } else {
                // Not a BOOLEAN: this element must itself be the extnValue.
                if maybe_bool.tag != TAG_OCTET_STRING {
                    return Err(format!(
                        "extension must contain a critical BOOLEAN then an OCTET STRING, \
                         got tag 0x{:02X}",
                        maybe_bool.tag
                    ));
                }
                (maybe_bool, after_critical)
            };
            // An empty OCTET STRING is legal; its bytes are not interpreted.
            let _ = octets.content;
            if !after_octets.is_empty() {
                return Err("trailing data in extension".to_string());
            }
        }
        Ok(())
    }

    /// SubjectPublicKeyInfo ::= SEQUENCE {
    ///   algorithm        AlgorithmIdentifier,
    ///   subjectPublicKey BIT STRING }
    /// Nothing may be missing and no extra elements may follow.
    fn check_spki(spki: Tlv) -> Result<(), String> {
        let (alg, rest) = read_tagged(
            spki.content,
            TAG_SEQUENCE,
            "subjectPublicKeyInfo algorithm identifier",
        )?;
        check_algorithm_identifier(alg)?;
        let (key, rest) = read_tagged(rest, TAG_BIT_STRING, "subjectPublicKey")?;
        check_bit_string(key.content, "subjectPublicKey")?;
        if !rest.is_empty() {
            return Err("trailing data in subjectPublicKeyInfo".to_string());
        }
        Ok(())
    }

    /// AlgorithmIdentifier ::= SEQUENCE {
    ///   algorithm  OBJECT IDENTIFIER,
    ///   parameters ANY DEFINED BY algorithm OPTIONAL }
    /// The parameters, if present, must be exactly one complete DER TLV;
    /// absent parameters and an explicit NULL are both accepted. The
    /// parameter content is not interpreted for any particular algorithm.
    fn check_algorithm_identifier(alg: Tlv) -> Result<(), String> {
        let (oid, rest) = read_tagged(alg.content, TAG_OID, "algorithm OID")?;
        parse_oid(oid.content).map_err(|e| format!("algorithm OID: {e}"))?;
        if rest.is_empty() {
            return Ok(());
        }
        let (_params, after) =
            read_tlv(rest).map_err(|e| format!("algorithm parameters: {e}"))?;
        if !after.is_empty() {
            return Err("multiple algorithm parameters".to_string());
        }
        Ok(())
    }

    /// Validate the DER content of a BIT STRING: one leading byte counting
    /// unused bits (0..=7), and when the count is nonzero those low bits of
    /// the final data byte must all be zero. An empty bit string is only
    /// legal with a zero count.
    fn check_bit_string(content: &[u8], what: &str) -> Result<(), String> {
        let unused = *content
            .first()
            .ok_or_else(|| format!("{what} BIT STRING is missing the unused-bits byte"))?;
        if unused > 7 {
            return Err(format!("{what} BIT STRING has invalid unused-bits count {unused}"));
        }
        let data = &content[1..];
        if data.is_empty() {
            if unused != 0 {
                return Err(format!(
                    "{what} BIT STRING declares {unused} unused bits but has no data"
                ));
            }
            return Ok(());
        }
        if unused != 0 && data[data.len() - 1] & ((1u8 << unused) - 1) != 0 {
            return Err(format!(
                "{what} BIT STRING has set bits among the {unused} unused trailing bits"
            ));
        }
        Ok(())
    }

    /// Render the serial number as upper-case hex, matching the integer value
    /// (a DER sign-padding 0x00 byte is not part of the number).
    fn format_serial(content: &[u8]) -> Result<String, String> {
        if content.is_empty() {
            return Err("empty serial number".to_string());
        }
        if content.len() > 1 && content[0] == 0xFF && content[1] & 0x80 == 0 {
            return Err("non-minimal serial number encoding".to_string());
        }
        if content.len() > 1 && content[0] == 0x00 && content[1] & 0x80 == 0 {
            return Err("non-minimal serial number encoding".to_string());
        }
        if content[0] & 0x80 != 0 && !(content.len() > 1 && content[0] == 0x00) {
            return Err("negative serial number".to_string());
        }
        let digits = if content.len() > 1 && content[0] == 0x00 {
            &content[1..]
        } else {
            content
        };
        let mut s = String::with_capacity(digits.len() * 2);
        for b in digits {
            write!(s, "{b:02X}").unwrap();
        }
        if s.is_empty() {
            s.push_str("00");
        }
        Ok(s)
    }

    /// Name ::= SEQUENCE OF RDN; rendered per RFC 4514 (RDNs reversed,
    /// multi-valued RDNs joined with '+'). Values of known attribute types
    /// are emitted as real UTF-8 text (non-ASCII content preserved verbatim)
    /// when their tag has a text decoder; a known type carrying any other
    /// readable DER value (e.g. postalAddress as a SEQUENCE) keeps its short
    /// name and uses the '#' form carrying the value's full DER encoding in
    /// hex. Attributes without a known short name use their dotted OID with
    /// the same '#' form.
    fn format_name(name: Tlv) -> Result<String, String> {
        let mut rdn_buf = name.content;
        let mut rdns: Vec<String> = Vec::new();
        while !rdn_buf.is_empty() {
            let (set, rest) = read_tagged(rdn_buf, TAG_SET, "RelativeDistinguishedName")?;
            rdn_buf = rest;

            let mut atv_buf = set.content;
            let mut parts = Vec::new();
            while !atv_buf.is_empty() {
                let (atv, rest) =
                    read_tagged(atv_buf, TAG_SEQUENCE, "AttributeTypeAndValue")?;
                atv_buf = rest;

                let (oid, after_oid) = read_tagged(atv.content, TAG_OID, "attribute OID")?;
                let (value, after_value) = read_tlv(after_oid)?;
                if !after_value.is_empty() {
                    return Err("trailing data in attribute value".to_string());
                }

                let oid_arcs = parse_oid(oid.content)?;
                match oid_short_name(&oid_arcs) {
                    Some(label) => {
                        if is_text_value_tag(value.tag) {
                            // Supported string type: decode to escaped text.
                            // A decode failure (e.g. invalid UTF-8) means the
                            // certificate is corrupt; it must not be salvaged
                            // by falling back to the hex form.
                            let text = decode_attribute_value(value.tag, value.content)?;
                            parts.push(format!("{label}={}", escape_value(&text)));
                        } else {
                            // Known type but a legal value with no text decoder
                            // (SEQUENCE, OCTET STRING, ...): keep the short name
                            // and show the full original DER encoding as '#'-hex.
                            parts.push(format!("{label}=#{}", value_hex(after_oid, after_value)));
                        }
                    }
                    None => {
                        // Unknown attribute type: RFC 4514 requires the form
                        // OID=#hex, where the hex is the full DER encoding
                        // (tag, length and content) of the attribute value.
                        // The value is still validated: a UTF8String must
                        // hold valid UTF-8 even when shown as hex.
                        if value.tag == 0x0C {
                            std::str::from_utf8(value.content)
                                .map_err(|_| "invalid UTF-8 in UTF8String".to_string())?;
                        }
                        let hex = value_hex(after_oid, after_value);
                        parts.push(format!("{}=#{hex}", arcs_to_string(&oid_arcs)));
                    }
                }
            }
            if parts.is_empty() {
                return Err("empty RelativeDistinguishedName".to_string());
            }
            rdns.push(parts.join("+"));
        }
        rdns.reverse();
        Ok(rdns.join(","))
    }

    /// Uppercase hex of one complete value TLV, sliced straight from the
    /// input bytes (`around` ends at `rest`). The original tag and length
    /// bytes survive verbatim, including a legal long-form length; nothing is
    /// re-encoded and content alone is never emitted.
    fn value_hex(around: &[u8], rest: &[u8]) -> String {
        let tlv_len = around.len() - rest.len();
        let mut hex = String::with_capacity(tlv_len * 2);
        for b in &around[..tlv_len] {
            write!(hex, "{b:02X}").unwrap();
        }
        hex
    }

    /// Tags whose attribute values have a direct text decoding in
    /// `decode_attribute_value`. Every other tag that `read_tlv` accepts is a
    /// legal non-text value for display purposes and is rendered as '#'-hex.
    fn is_text_value_tag(tag: u8) -> bool {
        matches!(
            tag,
            0x0C | 0x12 | 0x13 | 0x14 | 0x16 | 0x1A | 0x1B | 0x1C | 0x1E
        )
    }

    fn parse_oid(content: &[u8]) -> Result<Vec<u64>, String> {
        if content.is_empty() {
            return Err("empty OID".to_string());
        }
        let mut arcs = Vec::new();
        // The first subidentifier encodes the first two arcs as
        // 40*first + second and is itself a base-128 quantity, so it can
        // span multiple content bytes (e.g. 2.999 encodes as 0x88 0x37).
        let mut i = 0;
        let first = read_oid_arc(content, &mut i)?;
        match first {
            0..=39 => {
                arcs.push(0);
                arcs.push(first);
            }
            40..=79 => {
                arcs.push(1);
                arcs.push(first - 40);
            }
            _ => {
                arcs.push(2);
                arcs.push(first - 80);
            }
        }
        while i < content.len() {
            arcs.push(read_oid_arc(content, &mut i)?);
        }
        Ok(arcs)
    }

    /// Read one base-128 subidentifier starting at `*i`. Rejects trailing
    /// continuation bytes and non-minimal encodings.
    fn read_oid_arc(content: &[u8], i: &mut usize) -> Result<u64, String> {
        let start = *i;
        let mut value = 0u64;
        loop {
            let b = *content
                .get(*i)
                .ok_or_else(|| "truncated OID arc".to_string())?;
            value = value
                .checked_mul(128)
                .and_then(|v| v.checked_add((b & 0x7F) as u64))
                .ok_or_else(|| "OID arc overflow".to_string())?;
            *i += 1;
            if b & 0x80 == 0 {
                break;
            }
        }
        if *i - start > 1 && content[start] == 0x80 {
            return Err("non-minimal OID arc encoding".to_string());
        }
        Ok(value)
    }

    fn arcs_to_string(arcs: &[u64]) -> String {
        arcs.iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }

    fn oid_short_name(arcs: &[u64]) -> Option<&'static str> {
        // X.520 / X.521 (2.5.4.*) and the commonly used LDAP/PKCS names.
        let name = match arcs {
            [2, 5, 4, 3] => "CN",
            [2, 5, 4, 4] => "SN",
            [2, 5, 4, 5] => "serialNumber",
            [2, 5, 4, 6] => "C",
            [2, 5, 4, 7] => "L",
            [2, 5, 4, 8] => "ST",
            [2, 5, 4, 9] => "STREET",
            [2, 5, 4, 10] => "O",
            [2, 5, 4, 11] => "OU",
            [2, 5, 4, 12] => "title",
            [2, 5, 4, 17] => "postalAddress",
            [2, 5, 4, 18] => "postalCode",
            [2, 5, 4, 20] => "telephoneNumber",
            [2, 5, 4, 42] => "givenName",
            [2, 5, 4, 43] => "initials",
            [2, 5, 4, 44] => "generationQualifier",
            [2, 5, 4, 46] => "dnQualifier",
            [2, 5, 4, 65] => "pseudonym",
            [0, 9, 2342, 19200300, 100, 1, 1] => "UID",
            [0, 9, 2342, 19200300, 100, 1, 25] => "DC",
            [1, 2, 840, 113549, 1, 9, 1] => "emailAddress",
            _ => return None,
        };
        Some(name)
    }

    /// Decode a DirectoryString-style attribute value into UTF-8 text.
    fn decode_attribute_value(tag: u8, content: &[u8]) -> Result<String, String> {
        let s = match tag {
            0x0C => std::str::from_utf8(content)
                .map_err(|_| "invalid UTF-8 in UTF8String".to_string())?
                .to_string(),
            // ASCII-family strings: every byte is also its Unicode code point.
            0x12 | 0x13 | 0x16 | 0x1A => content.iter().map(|&b| b as char).collect(),
            // T.61 / GeneralString are conventionally treated as Latin-1 here.
            0x14 | 0x1B => content.iter().map(|&b| b as char).collect(),
            0x1E => {
                if content.len() % 2 != 0 {
                    return Err("odd-length BMPString".to_string());
                }
                let units: Vec<u16> = content
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                char::decode_utf16(units.iter().copied())
                    .map(|r| r.map_err(|_| "unpaired surrogate in BMPString".to_string()))
                    .collect::<Result<String, _>>()?
            }
            0x1C => {
                if content.len() % 4 != 0 {
                    return Err("invalid UniversalString length".to_string());
                }
                let mut s = String::new();
                for c in content.chunks_exact(4) {
                    let cp = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
                    if (0xD800..=0xDFFF).contains(&cp) || cp > 0x10FFFF {
                        return Err("invalid code point in UniversalString".to_string());
                    }
                    s.push(char::from_u32(cp).unwrap());
                }
                s
            }
            _ => {
                return Err(format!(
                    "unsupported attribute value string type (tag 0x{tag:02X})"
                ))
            }
        };
        Ok(s)
    }

    /// RFC 4514 value escaping; everything above the ASCII control range
    /// (including CJK text) passes through unchanged.
    fn escape_value(value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        let last = value.chars().count().saturating_sub(1);
        for (i, c) in value.chars().enumerate() {
            let at_edge = i == 0 || i == last;
            let needs_backslash = matches!(c, ',' | '+' | '"' | '\\' | '<' | '>' | ';')
                || (at_edge && (c == ' ' || c == '#'));
            if needs_backslash {
                out.push('\\');
                out.push(c);
            } else if ('\0'..='\x1F').contains(&c) || c == '\x7F' {
                let mut buf = [0u8; 4];
                let b = c.encode_utf8(&mut buf).as_bytes()[0];
                write!(out, "\\{b:02X}").unwrap();
            } else {
                out.push(c);
            }
        }
        out
    }

    fn format_time(tlv: Tlv) -> Result<String, String> {
        let (y, mo, d, h, mi, se) = match tlv.tag {
            TAG_UTC_TIME => {
                let c = tlv.content;
                if c.len() != 13 || c[12] != b'Z' {
                    return Err("UTCTime must be YYMMDDHHMMSSZ".to_string());
                }
                let digits = &c[..12];
                if !digits.iter().all(|b| b.is_ascii_digit()) {
                    return Err("non-numeric UTCTime".to_string());
                }
                let n = |a: usize, b: usize| -> u32 {
                    digits[a..=b].iter().fold(0, |acc, x| acc * 10 + (x - b'0') as u32)
                };
                let yy = n(0, 1);
                let year = if yy < 50 { 2000 + yy } else { 1900 + yy };
                (year as i32, n(2, 3), n(4, 5), n(6, 7), n(8, 9), n(10, 11))
            }
            TAG_GENERALIZED_TIME => {
                let c = tlv.content;
                if c.len() != 15 || c[14] != b'Z' {
                    return Err(
                        "GeneralizedTime must be YYYYMMDDHHMMSSZ in UTC".to_string()
                    );
                }
                let digits = &c[..14];
                if !digits.iter().all(|b| b.is_ascii_digit()) {
                    return Err("non-numeric GeneralizedTime".to_string());
                }
                let n = |a: usize, b: usize| -> u32 {
                    digits[a..=b].iter().fold(0, |acc, x| acc * 10 + (x - b'0') as u32)
                };
                (n(0, 3) as i32, n(4, 5), n(6, 7), n(8, 9), n(10, 11), n(12, 13))
            }
            other => return Err(format!("invalid time tag 0x{other:02X}")),
        };
        validate_datetime(y, mo, d, h, mi, se)?;
        Ok(format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{se:02}Z"))
    }

    fn validate_datetime(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> Result<(), String> {
        if !(1..=12).contains(&month) {
            return Err("invalid month in certificate time".to_string());
        }
        if hour > 23 || minute > 59 || second > 59 {
            return Err("invalid time of day in certificate time".to_string());
        }
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        if day < 1 || day > days[(month - 1) as usize] {
            return Err("invalid day in certificate time".to_string());
        }
        Ok(())
    }
}
