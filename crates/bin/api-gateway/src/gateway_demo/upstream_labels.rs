pub(super) fn model_label(value: Option<&str>) -> Option<&str> {
    metadata_label(value, 160).filter(|value| {
        value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
        })
    })
}

pub(super) fn provider_label(value: Option<&str>) -> Option<&str> {
    metadata_label(value, 120).filter(|value| {
        value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b' ' | b'_' | b'-' | b'.' | b'&' | b'(' | b')' | b'+' | b'/'
                )
        })
    })
}

fn metadata_label(value: Option<&str>, limit: usize) -> Option<&str> {
    let value = value?.trim();
    (!value.is_empty()
        && value.len() <= limit
        && !value.contains("//")
        && !resembles_credential(value))
    .then_some(value)
}

pub(super) fn resembles_credential(value: &str) -> bool {
    value.starts_with("sk-")
        || value.starts_with("foundation_api_")
        || value.starts_with("tc_project_")
}
