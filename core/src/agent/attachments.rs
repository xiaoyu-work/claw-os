//! Bounded image attachments carried by one Agent task.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize};

pub const MAX_ATTACHMENTS: usize = 4;
pub const MAX_TOTAL_BYTES: usize = 256 * 1024;
pub const MAX_BASE64_BYTES: usize = ((MAX_TOTAL_BYTES + 2) / 3) * 4;
pub const MAX_NAME_BYTES: usize = 256;
const MAX_NAME_CHARS: usize = 128;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentInput {
    pub name: String,
    pub media_type: String,
    pub data: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ImageAttachment {
    pub name: String,
    pub media_type: String,
    pub data: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedAttachment {
    name: String,
    media_type: String,
    data: String,
    bytes: u64,
    sha256: String,
}

impl<'de> Deserialize<'de> for ImageAttachment {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let persisted = PersistedAttachment::deserialize(deserializer)?;
        let normalized = normalize_one(AttachmentInput {
            name: persisted.name,
            media_type: persisted.media_type,
            data: persisted.data,
        })
        .map_err(serde::de::Error::custom)?;
        if normalized.bytes != persisted.bytes || normalized.sha256 != persisted.sha256 {
            return Err(serde::de::Error::custom(
                "image attachment metadata does not match its content",
            ));
        }
        Ok(normalized)
    }
}

pub fn normalize(inputs: Vec<AttachmentInput>) -> Result<Vec<ImageAttachment>, String> {
    if inputs.len() > MAX_ATTACHMENTS {
        return Err(format!(
            "image attachment count exceeds the maximum of {MAX_ATTACHMENTS}"
        ));
    }
    let mut total = 0usize;
    let mut attachments = Vec::with_capacity(inputs.len());
    for input in inputs {
        let attachment = normalize_one(input)?;
        total = total
            .checked_add(attachment.bytes as usize)
            .ok_or_else(|| "image attachment size overflow".to_string())?;
        if total > MAX_TOTAL_BYTES {
            return Err(format!(
                "image attachments exceed the {MAX_TOTAL_BYTES}-byte total limit"
            ));
        }
        attachments.push(attachment);
    }
    Ok(attachments)
}

pub fn recorded_prompt(prompt: &str, attachments: &[ImageAttachment]) -> String {
    if attachments.is_empty() {
        return prompt.to_string();
    }
    let mut recorded = prompt.to_string();
    recorded.push_str("\n\n[Attached images:");
    for attachment in attachments {
        recorded.push_str(&format!(
            "\n- {} bytes={} {}",
            attachment.media_type, attachment.bytes, attachment.sha256
        ));
    }
    recorded.push_str("\n]");
    recorded
}

fn normalize_one(input: AttachmentInput) -> Result<ImageAttachment, String> {
    validate_name(&input.name)?;
    if input.data.len() > MAX_BASE64_BYTES {
        return Err(format!(
            "image attachment `{}` exceeds the encoded size limit",
            input.name
        ));
    }
    let bytes = STANDARD.decode(input.data.as_bytes()).map_err(|error| {
        format!(
            "image attachment `{}` is not valid base64: {error}",
            input.name
        )
    })?;
    if bytes.is_empty() {
        return Err(format!("image attachment `{}` is empty", input.name));
    }
    if bytes.len() > MAX_TOTAL_BYTES {
        return Err(format!(
            "image attachment `{}` exceeds the decoded size limit",
            input.name
        ));
    }
    if STANDARD.encode(&bytes) != input.data {
        return Err(format!(
            "image attachment `{}` is not canonical base64",
            input.name
        ));
    }
    validate_magic(&input.media_type, &bytes)
        .map_err(|error| format!("image attachment `{}` {error}", input.name))?;
    Ok(ImageAttachment {
        name: input.name,
        media_type: input.media_type,
        data: input.data,
        bytes: bytes.len() as u64,
        sha256: format!("sha256:{}", crate::crypto::sha256_hex(&bytes)),
    })
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
        || name.contains('/')
        || name.contains('\\')
    {
        return Err("image attachment name is invalid".to_string());
    }
    Ok(())
}

fn validate_magic(media_type: &str, bytes: &[u8]) -> Result<(), String> {
    let valid = match media_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]) && bytes.ends_with(&[0xff, 0xd9]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        _ => return Err(format!("has unsupported media type `{media_type}`")),
    };
    if valid {
        Ok(())
    } else {
        Err(format!("does not match declared media type `{media_type}`"))
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/attachments.rs"
    ));
}
