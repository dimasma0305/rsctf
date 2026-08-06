/// Build an RFC 6266 attachment value with a conservative ASCII fallback and
/// an RFC 5987 UTF-8 filename. The result is always visible ASCII and can be
/// inserted into either an HTTP header or S3 object metadata without header
/// injection or rejecting international filenames.
pub(crate) fn attachment(filename: &str) -> String {
    let mut fallback = String::with_capacity(filename.len().min(255));
    for character in filename.chars() {
        if fallback.len() >= 255 {
            break;
        }
        fallback.push(match character {
            value if value.is_ascii_alphanumeric() => value,
            ' ' | '.' | '-' | '_' | '(' | ')' | '[' | ']' => character,
            _ => '_',
        });
    }
    if fallback
        .trim_matches(|character| matches!(character, ' ' | '.' | '_'))
        .is_empty()
    {
        fallback = "download".to_string();
    }

    let mut encoded = String::with_capacity(filename.len());
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in filename.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(*byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }

    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

#[cfg(test)]
mod tests {
    use super::attachment;

    #[test]
    fn disposition_preserves_ascii_and_encodes_unicode() {
        assert_eq!(
            attachment("challenge file.zip"),
            "attachment; filename=\"challenge file.zip\"; filename*=UTF-8''challenge%20file.zip"
        );
        assert_eq!(
            attachment("音楽.zip"),
            "attachment; filename=\"__.zip\"; filename*=UTF-8''%E9%9F%B3%E6%A5%BD.zip"
        );
    }

    #[test]
    fn disposition_cannot_inject_headers_or_paths() {
        let value = attachment("../challenge\"\r\nX-Evil: yes\\payload.zip");
        assert!(!value.contains('\r'));
        assert!(!value.contains('\n'));
        assert_eq!(value.matches("attachment;").count(), 1);
        assert!(
            value.starts_with("attachment; filename=\".._challenge___X-Evil_ yes_payload.zip\"")
        );
    }
}
