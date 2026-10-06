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
    const TAG_PRINTABLE_STRING: u8 = 0x13;
    const TAG_UNIVERSAL_STRING: u8 = 0x1C;
    const TAG_BMP_STRING: u8 = 0x1E;

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
        let (outer_alg, c) = read_tagged(c, TAG_SEQUENCE, "outer signatureAlgorithm")?;
        let outer_alg = parse_signature_algorithm(outer_alg, "outer signatureAlgorithm")?;
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
        let (tbs_alg, t) = read_tagged(t, TAG_SEQUENCE, "tbs signatureAlgorithm")?;
        let tbs_alg = parse_signature_algorithm(tbs_alg, "tbs signatureAlgorithm")?;
        if tbs_alg != outer_alg {
            return Err(mismatch_message(&outer_alg, &tbs_alg));
        }
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
        let issuer = format_name(issuer, "issuer")?;
        let subject = format_name(subject, "subject")?;

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
    ///
    /// Each extension type (its complete OID) may occur at most once in the
    /// list. Identity is the OID alone — critical flag, value content and
    /// value length never distinguish two entries — so a duplicate is neither
    /// picked between, merged nor dropped: the whole certificate is rejected,
    /// whether the duplicates are adjacent or separated by other extensions.
    fn check_extensions(wrapper: &[u8]) -> Result<(), String> {
        let (list, rest) = read_tagged(wrapper, TAG_SEQUENCE, "extensions")?;
        if !rest.is_empty() {
            return Err("extensions wrapper contains data after the Extensions SEQUENCE".to_string());
        }
        if list.content.is_empty() {
            return Err("extensions SEQUENCE is empty".to_string());
        }

        // Complete OIDs (parsed arcs) already seen in this list. DER mandates
        // one shortest encoding per OID and `parse_oid` enforces it, so equal
        // arcs mean the same OID regardless of raw bytes; the list is short,
        // a linear scan keeps the representation simple.
        let mut seen_oids: Vec<Vec<u64>> = Vec::new();
        let mut buf = list.content;
        while !buf.is_empty() {
            let (ext, rest) = read_tagged(buf, TAG_SEQUENCE, "extension")?;
            buf = rest;

            let (oid, after_oid) = read_tagged(ext.content, TAG_OID, "extension OID")?;
            let oid_arcs =
                parse_oid(oid.content).map_err(|e| format!("extension OID: {e}"))?;
            if seen_oids.contains(&oid_arcs) {
                return Err(format!(
                    "duplicate extension type OID {}: an extension OID may appear at most once \
                     in the extensions list, regardless of its critical flag or extension value \
                     (duplicate entries are not selected between, merged or removed)",
                    arcs_to_string(&oid_arcs)
                ));
            }
            seen_oids.push(oid_arcs);

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

    /// Why the content of an AlgorithmIdentifier SEQUENCE failed the shared
    /// structural check in `split_algorithm_identifier`. Each variant carries
    /// only the raw facts; every reader maps them onto its own established
    /// error wording (the public-key position names "algorithm OID" and
    /// "algorithm parameters", the signature algorithm positions name their
    /// own location).
    enum AlgIdError {
        /// The leading OID element could not be read as one complete TLV.
        OidRead(String),
        /// The first element is present but is not an OID.
        OidTag(u8),
        /// The OID element exists but its content is not a valid OID
        /// (empty, truncated or non-minimal base-128 arcs).
        OidContent(String),
        /// A parameter element follows the OID but cannot be read as one
        /// complete TLV.
        ParamsRead(String),
        /// Bytes remain after the parameter element: parameters are one
        /// OPTIONAL element, never several.
        ExtraAfterParams,
    }

    /// Split the content of an AlgorithmIdentifier SEQUENCE into its
    /// algorithm OID and optional parameters, enforcing the structure every
    /// reader of an algorithm identifier shares:
    ///
    /// ```text
    /// AlgorithmIdentifier ::= SEQUENCE {
    ///   algorithm  OBJECT IDENTIFIER,
    ///   parameters ANY DEFINED BY algorithm OPTIONAL }
    /// ```
    ///
    /// The content must start with one complete, shortest-encoded OID,
    /// followed by either nothing or exactly one complete DER parameter
    /// value, and nothing after that. Absent parameters and an explicit NULL
    /// are both accepted and the parameter content is never interpreted for
    /// any particular algorithm; an unknown but well-formed OID is accepted.
    /// The parameter TLV is returned as its full original encoding (tag,
    /// length and content), sliced straight from the input, so callers that
    /// compare two identifiers can do so at the DER byte level.
    fn split_algorithm_identifier(
        content: &[u8],
    ) -> Result<(Tlv<'_>, Option<&[u8]>), AlgIdError> {
        let (oid, rest) = read_tlv(content).map_err(AlgIdError::OidRead)?;
        if oid.tag != TAG_OID {
            return Err(AlgIdError::OidTag(oid.tag));
        }
        parse_oid(oid.content).map_err(AlgIdError::OidContent)?;
        let params = if rest.is_empty() {
            None
        } else {
            let (_params, after) = read_tlv(rest).map_err(AlgIdError::ParamsRead)?;
            if !after.is_empty() {
                return Err(AlgIdError::ExtraAfterParams);
            }
            // Keep the full parameter TLV (tag, length and content), sliced
            // straight from the enclosing content; `read_tlv` already proved
            // it is a complete value.
            Some(&rest[..rest.len() - after.len()])
        };
        Ok((oid, params))
    }

    /// Validate the AlgorithmIdentifier inside SubjectPublicKeyInfo: the
    /// shared structure from `split_algorithm_identifier`, reported with the
    /// public-key position's own wording.
    fn check_algorithm_identifier(alg: Tlv) -> Result<(), String> {
        split_algorithm_identifier(alg.content)
            .map(|_| ())
            .map_err(|e| match e {
                AlgIdError::OidRead(e) => format!("algorithm OID: {e}"),
                AlgIdError::OidTag(tag) => {
                    format!("expected algorithm OID (tag 0x06), got 0x{tag:02X}")
                }
                AlgIdError::OidContent(e) => format!("algorithm OID: {e}"),
                AlgIdError::ParamsRead(e) => format!("algorithm parameters: {e}"),
                AlgIdError::ExtraAfterParams => "multiple algorithm parameters".to_string(),
            })
    }

    /// One of the certificate's two signature algorithm descriptions, kept as
    /// raw slices so two copies can be compared at the DER byte level: the OID
    /// content (arcs in their shortest encoding) and the complete parameter
    /// TLV (tag, length and content), or no parameter at all. An absent
    /// parameter and an explicit NULL are different representations.
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct SignatureAlgorithm<'a> {
        oid_content: &'a [u8],
        params: Option<&'a [u8]>,
    }

    /// Parse and fully validate one signatureAlgorithm SEQUENCE, applying the
    /// shared AlgorithmIdentifier structure from `split_algorithm_identifier`
    /// and keeping the raw pieces for the byte-level comparison of the two
    /// copies. `loc` names which of the two copies is being checked ("outer
    /// signatureAlgorithm" or the copy inside tbsCertificate) and prefixes
    /// every error message.
    fn parse_signature_algorithm<'a>(
        alg: Tlv<'a>,
        loc: &str,
    ) -> Result<SignatureAlgorithm<'a>, String> {
        let (oid, params) = split_algorithm_identifier(alg.content).map_err(|e| match e {
            AlgIdError::OidRead(e) => format!("{loc}: {e}"),
            AlgIdError::OidTag(tag) => format!(
                "{loc}: algorithm must start with an OID (tag 0x06), got tag 0x{tag:02X}"
            ),
            AlgIdError::OidContent(e) => format!("{loc}: {e}"),
            AlgIdError::ParamsRead(e) => format!("{loc}: algorithm parameters: {e}"),
            AlgIdError::ExtraAfterParams => {
                format!("{loc}: extra element after the algorithm parameters")
            }
        })?;
        Ok(SignatureAlgorithm {
            oid_content: oid.content,
            params,
        })
    }

    /// Describe the disagreement between the outer signatureAlgorithm and the
    /// copy inside tbsCertificate. Both copies parse on their own; they merely
    /// fail to name the same algorithm or the same parameter representation.
    fn mismatch_message(outer: &SignatureAlgorithm<'_>, inner: &SignatureAlgorithm<'_>) -> String {
        const OUTER: &str = "the outer signatureAlgorithm";
        const INNER: &str = "the signatureAlgorithm in tbsCertificate";
        if outer.oid_content != inner.oid_content {
            let outer_oid = parse_oid(outer.oid_content)
                .map(|arcs| arcs_to_string(&arcs))
                .unwrap_or_else(|_| "?".to_string());
            let inner_oid = parse_oid(inner.oid_content)
                .map(|arcs| arcs_to_string(&arcs))
                .unwrap_or_else(|_| "?".to_string());
            return format!(
                "signature algorithm mismatch: {OUTER} names OID {outer_oid}, but {INNER} \
                 names OID {inner_oid}"
            );
        }
        match (outer.params, inner.params) {
            (Some(outer_params), Some(inner_params)) if outer_params != inner_params => {
                format!(
                    "signature algorithm mismatch: both copies name the same algorithm OID but \
                     their parameters have different DER encodings ({OUTER} has {}, {INNER} has {})",
                    hex_of(outer_params),
                    hex_of(inner_params),
                )
            }
            (Some(_), None) => format!(
                "signature algorithm mismatch: {OUTER} carries parameters but {INNER} omits them \
                 (an absent parameter and an explicit value are different representations)"
            ),
            (None, Some(_)) => format!(
                "signature algorithm mismatch: {INNER} carries parameters but {OUTER} omits them \
                 (an absent parameter and an explicit value are different representations)"
            ),
            // PartialEq already proved the copies unequal; reaching any other
            // arm here would be a logic error rather than a malformed cert.
            _ => "signature algorithm mismatch between the two signatureAlgorithm copies"
                .to_string(),
        }
    }

    fn hex_of(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            write!(s, "{b:02X}").unwrap();
        }
        s
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
    ///
    /// Each RDN is a DER SET OF, so its complete AttributeTypeAndValue
    /// encodings must be sorted by their full encoding bytes (tag, then
    /// length, then content — the encoding, never the display name, dotted
    /// OID text or decoded value). The check is per RDN; it never reorders
    /// the members itself. `what` names the name being checked ("subject" or
    /// "issuer") so an ordering failure identifies it in the error message.
    fn format_name(name: Tlv, what: &str) -> Result<String, String> {
        let mut rdn_buf = name.content;
        let mut rdns: Vec<String> = Vec::new();
        while !rdn_buf.is_empty() {
            let (set, rest) = read_tagged(rdn_buf, TAG_SET, "RelativeDistinguishedName")?;
            rdn_buf = rest;

            let mut atv_buf = set.content;
            let mut parts = Vec::new();
            // Raw full encoding (tag, length and content) of each member of
            // this SET OF, straight from the certificate: DER SET OF ordering
            // compares the complete member encodings byte for byte. Equal
            // encodings may legitimately repeat and stay adjacent.
            let mut members: Vec<&[u8]> = Vec::new();
            while !atv_buf.is_empty() {
                let atv_start = atv_buf;
                let (atv, rest) =
                    read_tagged(atv_buf, TAG_SEQUENCE, "AttributeTypeAndValue")?;
                atv_buf = rest;
                members.push(&atv_start[..atv_start.len() - atv_buf.len()]);

                let (oid, after_oid) = read_tagged(atv.content, TAG_OID, "attribute OID")?;
                let (value, after_value) = read_tlv(after_oid)?;
                if !after_value.is_empty() {
                    return Err("trailing data in attribute value".to_string());
                }

                let oid_arcs = parse_oid(oid.content)?;

                // The value's own type rules apply before any display decision:
                // a PrintableString must hold only the RFC 5280 PrintableString
                // alphabet, a BMPString must hold whole 16-bit BMP characters
                // and a UniversalString must hold whole 32-bit Unicode code
                // points, whether the attribute later renders as text for a
                // known short name or as '#'-hex for an unknown type. The hex
                // form must never let an illegal byte or code point through,
                // and illegal content cannot be dropped or replaced. An empty
                // value stays legal. `what` names the offending name
                // ("subject"/"issuer").
                if value.tag == TAG_PRINTABLE_STRING
                    && !is_printable_string(value.content)
                {
                    return Err(format!(
                        "{what} name: PrintableString attribute value contains an illegal \
                         character (only A-Z, a-z, 0-9, space and ' ( ) + , - . / : = ? are \
                         allowed)"
                    ));
                }
                if value.tag == TAG_BMP_STRING {
                    check_bmp_string(value.content, what)?;
                }
                if value.tag == TAG_UNIVERSAL_STRING {
                    check_universal_string(value.content, what)?;
                }

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
                        // Showing the value as hex does not bypass its type
                        // rules: UTF8String content must be valid UTF-8, a
                        // PrintableString must keep to its alphabet, a
                        // BMPString must hold legal BMP characters and a
                        // UniversalString must hold legal Unicode code points,
                        // checks already performed above before this match.
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
            check_rdn_der_order(&members, what)?;
            rdns.push(parts.join("+"));
        }
        rdns.reverse();
        Ok(rdns.join(","))
    }

    /// Validate one RDN as a DER SET OF: every member's complete
    /// AttributeTypeAndValue encoding must be less than or equal to the next
    /// one under ordinary byte ordering of tag, length and content. The
    /// comparison runs on the raw encodings only — CN/O short names, dotted
    /// OID text and decoded values never enter into it — and equal adjacent
    /// members (duplicate attributes) are allowed. The members are never
    /// reordered to make an unsorted input pass. `what` is "subject" or
    /// "issuer" and identifies the offending name in the error message.
    fn check_rdn_der_order(members: &[&[u8]], what: &str) -> Result<(), String> {
        for pair in members.windows(2) {
            if pair[0] > pair[1] {
                return Err(format!(
                    "{what} name: attributes of a multi-valued RelativeDistinguishedName \
                     are not encoded in DER SET OF order (the members must be sorted by \
                     their complete DER encoding bytes: tag, then length, then content)"
                ));
            }
        }
        Ok(())
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

    /// Whether every byte belongs to the RFC 5280 PrintableString alphabet:
    /// A-Z, a-z, 0-9, space, and ' ( ) + , - . / : = ?. Every other byte
    /// (including @, _, tabs, newlines, NUL and 0x80..=0xFF) is illegal.
    /// An empty value passes, preserving the existing acceptance of empty
    /// contents.
    fn is_printable_string(content: &[u8]) -> bool {
        content.iter().all(|&b| matches!(
            b,
            b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b' '
                | b'\''
                | b'('
                | b')'
                | b'+'
                | b','
                | b'-'
                | b'.'
                | b'/'
                | b':'
                | b'='
                | b'?'
        ))
    }

    /// Validate BMPString content: an even number of bytes read as big-endian
    /// 16-bit characters, each of which must be a Basic Multilingual Plane
    /// code point (U+0000..=U+FFFF) and never a UTF-16 surrogate
    /// (U+D800..=U+DFFF). Surrogates are not characters, so a lone high or
    /// low surrogate is corrupt, and so is an adjacent pair that UTF-16
    /// rules would combine into an astral-plane character — the pair must
    /// not be merged and displayed. An illegal value fails the whole
    /// certificate; bytes are never dropped or replaced to salvage it.
    /// Empty content stays legal, preserving the existing acceptance of
    /// empty values. `what` names the name being checked ("subject" or
    /// "issuer") so the error identifies the offending name.
    fn check_bmp_string(content: &[u8], what: &str) -> Result<(), String> {
        if content.len() % 2 != 0 {
            return Err(format!(
                "{what} name: BMPString attribute value has an odd length of {} byte(s) \
                 (the content must be a sequence of 16-bit characters)",
                content.len()
            ));
        }
        for pair in content.chunks_exact(2) {
            let unit = u16::from_be_bytes([pair[0], pair[1]]);
            if (0xD800..=0xDFFF).contains(&unit) {
                return Err(format!(
                    "{what} name: BMPString attribute value contains the surrogate code \
                     point U+{unit:04X}, which is not a legal character (U+D800..=U+DFFF \
                     are not characters and cannot appear, alone or in pairs)"
                ));
            }
        }
        Ok(())
    }

    /// Validate UniversalString content: the bytes must form whole big-endian
    /// 32-bit Unicode code points, so the length must be a multiple of four,
    /// and every code point must lie in U+0000..=U+10FFFF and never be a
    /// UTF-16 surrogate (U+D800..=U+DFFF). A trailing incomplete unit, a lone
    /// surrogate (or a pair that UTF-16 rules would combine), or a value
    /// above the Unicode ceiling is corrupt; the whole certificate fails and
    /// no unit is dropped, replaced or merely hex-displayed to salvage it.
    /// Empty content stays legal, preserving the existing acceptance of empty
    /// values. `what` names the name being checked ("subject" or "issuer")
    /// so the error identifies the offending name.
    fn check_universal_string(content: &[u8], what: &str) -> Result<(), String> {
        if content.len() % 4 != 0 {
            return Err(format!(
                "{what} name: UniversalString attribute value has a length of {} byte(s), \
                 which is not a multiple of four (the content must be a sequence of \
                 big-endian 32-bit Unicode code points)",
                content.len()
            ));
        }
        for unit in content.chunks_exact(4) {
            let cp = u32::from_be_bytes([unit[0], unit[1], unit[2], unit[3]]);
            if (0xD800..=0xDFFF).contains(&cp) {
                return Err(format!(
                    "{what} name: UniversalString attribute value contains the surrogate code \
                     point U+{cp:04X}, which is not a legal character (U+D800..=U+DFFF are \
                     not characters and cannot appear, alone or in pairs)"
                ));
            }
            if cp > 0x10FFFF {
                return Err(format!(
                    "{what} name: UniversalString attribute value contains the code point \
                     U+{cp:04X}, which is above the Unicode maximum U+10FFFF"
                ));
            }
        }
        Ok(())
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
                // A BMPString character is one big-endian 16-bit BMP code
                // point, never a UTF-16 surrogate: surrogate pairs must not
                // be merged into astral-plane characters.
                let mut s = String::with_capacity(content.len() / 2);
                for c in content.chunks_exact(2) {
                    let unit = u16::from_be_bytes([c[0], c[1]]);
                    let ch = char::from_u32(unit as u32)
                        .ok_or_else(|| "surrogate code point in BMPString".to_string())?;
                    s.push(ch);
                }
                s
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
