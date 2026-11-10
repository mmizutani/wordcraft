#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Failure(pub u16, pub &'static str);

pub(super) const INVALID: Failure = Failure(400, "invalid_metadata");
pub(super) const TOO_LARGE: Failure = Failure(413, "body_too_large");
const PRECONDITION: Failure = Failure(428, "revision_required");
pub(super) const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_NAME_BYTES: usize = 255;
// D1 returns numbers as JavaScript numbers.
pub(super) const MAX_REVISION: u64 = (1 << 53) - 1;

pub(super) fn filename(encoded: Option<&str>) -> Result<String, Failure> {
    let encoded = encoded.filter(|encoded| encoded.len() <= 3 * MAX_NAME_BYTES).ok_or(INVALID)?;
    let mut decoded = Vec::with_capacity(encoded.len().min(MAX_NAME_BYTES));
    let mut bytes = encoded.bytes();
    while let Some(byte) = bytes.next() {
        decoded.push(match byte {
            b'%' => {
                let hi = hex(bytes.next().ok_or(INVALID)?).ok_or(INVALID)?;
                let lo = hex(bytes.next().ok_or(INVALID)?).ok_or(INVALID)?;
                hi * 16 + lo
            }
            byte if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')') => byte,
            _ => return Err(INVALID),
        });
    }

    let name = String::from_utf8(decoded).map_err(|_| INVALID)?;
    let (stem, extension) = name.rsplit_once('.').ok_or(INVALID)?;
    if name.len() > MAX_NAME_BYTES
        || name.trim() != name
        || stem.trim().is_empty()
        || !extension.eq_ignore_ascii_case("docx")
        || name.chars().any(|ch| ch.is_control() || matches!(ch, '/' | '\\'))
    {
        return Err(INVALID);
    }
    Ok(name)
}

pub(super) fn revision(input: Option<&str>) -> Result<u64, Failure> {
    let value = input.ok_or(PRECONDITION)?;
    let digits = value.strip_prefix('"').and_then(|value| value.strip_suffix('"')).ok_or(INVALID)?;
    if digits.starts_with('0') || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(INVALID);
    }
    digits.parse::<u64>().ok().filter(|value| *value <= MAX_REVISION).ok_or(INVALID)
}

pub(super) fn size(declared: Option<&str>, content_length: Option<&str>) -> Result<u64, Failure> {
    let declared = decimal_length(declared.ok_or(INVALID)?)?;
    if declared == 0 {
        return Err(INVALID);
    }
    if let Some(content_length) = content_length
        && decimal_length(content_length)? != declared
    {
        return Err(INVALID);
    }
    Ok(declared)
}

fn decimal_length(input: &str) -> Result<u64, Failure> {
    if input.is_empty() || !input.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(INVALID);
    }
    let length = input.parse::<u64>().map_err(|_| TOO_LARGE)?;
    if length > MAX_BYTES {
        return Err(TOO_LARGE);
    }
    Ok(length)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn digest(input: Option<&str>) -> Result<Vec<u8>, Failure> {
    let input = input.filter(|input| input.len() == 64).ok_or(INVALID)?;
    input.as_bytes().as_chunks::<2>().0.iter().map(|[hi, lo]| Ok(hex(*hi).ok_or(INVALID)? * 16 + hex(*lo).ok_or(INVALID)?)).collect()
}

#[cfg(target_arch = "wasm32")]
pub(super) fn opaque_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(target_arch = "wasm32")]
pub(super) fn operation_id(input: Option<&str>) -> Result<String, Failure> {
    let input = input.filter(|input| input.len() == 36).ok_or(INVALID)?;
    if input.bytes().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => byte == b'-',
        _ => byte.is_ascii_hexdigit(),
    }) {
        Ok(input.to_ascii_lowercase())
    } else {
        Err(INVALID)
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn attachment(name: &str) -> String {
    use std::fmt::Write;
    let mut header = String::from("attachment; filename=\"document.docx\"; filename*=UTF-8''");
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            header.push(char::from(byte));
        } else {
            let _ = write!(header, "%{byte:02X}");
        }
    }
    header
}

#[cfg(test)]
mod tests {
    use super::{INVALID, MAX_BYTES, PRECONDITION, TOO_LARGE, filename, revision, size};

    #[test]
    fn accepts_only_a_single_safe_docx_display_filename() {
        assert_eq!(filename(Some("R%C3%A9sum%C3%A9%20%2B%20notes.DOCX")), Ok("Résumé + notes.DOCX".into()));
        for input in [
            None,
            Some(""),
            Some(".docx"),
            Some("a.docx "),
            Some("a%2fb.docx"),
            Some("a b.docx"),
            Some("a,b.docx"),
            Some("a%5Cb.docx"),
            Some("a%0db.docx"),
            Some("a%ff.docx"),
            Some("a%Q0.docx"),
        ] {
            assert_eq!(filename(input), Err(INVALID), "{input:?}");
        }
        assert_eq!(filename(Some(&format!("{}.docx", "x".repeat(251)))), Err(INVALID));
    }

    #[test]
    fn revision_requires_a_single_canonical_strong_validator() {
        assert_eq!(revision(None), Err(PRECONDITION));
        assert_eq!(revision(Some("\"12\"")), Ok(12));
        for invalid in ["*", "W/\"1\"", "\"1\", \"2\"", "\"0\"", "\"01\"", "\"+1\"", "\"9007199254740992\""] {
            assert_eq!(revision(Some(invalid)), Err(INVALID), "{invalid}");
        }
    }

    #[test]
    fn accepts_the_cap_and_requires_lengths_to_agree() {
        assert_eq!(size(Some("16777216"), None), Ok(MAX_BYTES));
        assert_eq!(size(Some("16777217"), None), Err(TOO_LARGE));
        assert_eq!(size(Some("999999999999999999999"), None), Err(TOO_LARGE));
        assert_eq!(size(Some("12"), Some("13")), Err(INVALID));
        for invalid in [None, Some(""), Some("0"), Some("+1"), Some("1, 1")] {
            assert_eq!(size(invalid, None), Err(INVALID));
        }
    }
}
