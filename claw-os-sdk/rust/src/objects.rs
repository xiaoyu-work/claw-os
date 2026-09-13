//! Pure identifiers for App-owned objects. These helpers never resolve data,
//! discover Apps, open storage, or grant permission.

use crate::generated::ObjectRef;

const MAX_URI_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid App object reference: {0}")]
pub struct ObjectRefError(&'static str);

pub fn validate(value: &crate::generated::ObjectRef) -> Result<(), ObjectRefError> {
    if !component(&value.app_id, 128) {
        return Err(ObjectRefError(
            "app_id must be a lowercase ASCII component of at most 128 bytes",
        ));
    }
    if !component(&value.object_type, 64) {
        return Err(ObjectRefError(
            "object_type must be a lowercase ASCII component of at most 64 bytes",
        ));
    }
    if !opaque(&value.object_id, 1024) {
        return Err(ObjectRefError(
            "object_id must contain 1..=1024 UTF-8 bytes without control characters",
        ));
    }
    if let Some(revision) = &value.revision {
        if !opaque(revision, 128) {
            return Err(ObjectRefError(
                "revision must contain 1..=128 UTF-8 bytes without control characters",
            ));
        }
    }
    Ok(())
}

pub fn format_reference(value: &ObjectRef) -> Result<String, ObjectRefError> {
    validate(value)?;
    let mut uri = format!(
        "app://{}/{}?id={}",
        value.app_id,
        value.object_type,
        encode(&value.object_id)
    );
    if let Some(revision) = &value.revision {
        uri.push_str("&revision=");
        uri.push_str(&encode(revision));
    }
    if uri.len() > MAX_URI_BYTES {
        return Err(ObjectRefError("URI exceeds 4096 UTF-8 bytes"));
    }
    Ok(uri)
}

pub fn parse_reference(value: &str) -> Result<ObjectRef, ObjectRefError> {
    if value.len() > MAX_URI_BYTES {
        return Err(ObjectRefError("URI exceeds 4096 UTF-8 bytes"));
    }
    let rest = value
        .strip_prefix("app://")
        .ok_or(ObjectRefError("URI must use the exact app:// scheme"))?;
    let (address, query) = rest
        .split_once('?')
        .ok_or(ObjectRefError("URI requires an id query parameter"))?;
    let (app_id, object_type) = address
        .split_once('/')
        .ok_or(ObjectRefError("URI requires one App and one object type"))?;
    let mut object_id = None;
    let mut revision = None;
    for parameter in query.split('&') {
        let (key, encoded) = parameter
            .split_once('=')
            .ok_or(ObjectRefError("invalid query parameter"))?;
        match key {
            "id" if object_id.is_none() => object_id = Some(decode(encoded)?),
            "revision" if revision.is_none() => revision = Some(decode(encoded)?),
            _ => return Err(ObjectRefError("unknown or repeated query parameter")),
        }
    }
    let reference = ObjectRef {
        app_id: app_id.to_string(),
        object_type: object_type.to_string(),
        object_id: object_id.ok_or(ObjectRefError("URI requires an id query parameter"))?,
        revision,
    };
    if format_reference(&reference)? != value {
        return Err(ObjectRefError("URI is not canonically spelled"));
    }
    Ok(reference)
}

fn component(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn opaque(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

fn encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        if unreserved(byte) {
            output.push(char::from(byte));
        } else {
            output.push('%');
            output.push(char::from(HEX[usize::from(byte >> 4)]));
            output.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
    output
}

fn decode(value: &str) -> Result<String, ObjectRefError> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if unreserved(byte) {
            decoded.push(byte);
        } else if byte == b'%' {
            let high = bytes.next().and_then(hex);
            let low = bytes.next().and_then(hex);
            match (high, low) {
                (Some(high), Some(low)) => decoded.push((high << 4) | low),
                _ => return Err(ObjectRefError("invalid percent escape")),
            }
        } else {
            return Err(ObjectRefError(
                "query values must use RFC3986 percent encoding",
            ));
        }
    }
    String::from_utf8(decoded).map_err(|_| ObjectRefError("query value is not valid UTF-8"))
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/objects.rs"));
}
