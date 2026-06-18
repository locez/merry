use crate::{McpError, McpResult};
use merry_core::ToolName;
use std::collections::{BTreeMap, BTreeSet};

/// Mapping between one raw MCP tool name and its provider-visible Merry name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolNameMapping {
    pub raw_name: String,
    pub merry_name: ToolName,
}

/// Maps raw MCP tool names into provider-visible Merry tool names.
///
/// Merry tool names are capped at 64 ASCII characters and cannot contain dots.
/// MCP tool names can be longer and broader. This mapping namespaces every
/// tool by server id and resolves slug collisions deterministically by raw
/// name, not by discovery order.
pub fn map_mcp_tool_names(
    server_id: &str,
    raw_names: &[&str],
) -> McpResult<Vec<McpToolNameMapping>> {
    let server_slug = slug_component(server_id);
    if server_slug.is_empty() {
        return Err(McpError::InvalidServerId {
            id: server_id.to_owned(),
            message: "server id does not contain any tool-name characters".to_owned(),
        });
    }

    let mut base_by_raw = BTreeMap::new();
    let mut raws_by_base: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for raw_name in raw_names {
        let tool_slug = slug_component(raw_name);
        if tool_slug.is_empty() {
            return Err(McpError::InvalidToolName {
                server_id: server_id.to_owned(),
                name: (*raw_name).to_owned(),
                message: "tool name does not contain any tool-name characters".to_owned(),
            });
        }
        let base = compact_tool_name(&format!("mcp_{server_slug}_{tool_slug}"), 64);
        base_by_raw.insert((*raw_name).to_owned(), base.clone());
        raws_by_base
            .entry(base)
            .or_default()
            .insert((*raw_name).to_owned());
    }

    let mut mapped_by_raw = BTreeMap::new();
    for (raw, base) in &base_by_raw {
        let collision_group = raws_by_base
            .get(base)
            .expect("base was inserted with raw name");
        let candidate = if collision_group.len() > 1 {
            append_hash_suffix(base, raw)
        } else {
            base.clone()
        };
        let merry_name = ToolName::new(&candidate).map_err(|error| McpError::InvalidToolName {
            server_id: server_id.to_owned(),
            name: raw.clone(),
            message: error.to_string(),
        })?;
        mapped_by_raw.insert(raw.clone(), merry_name);
    }

    raw_names
        .iter()
        .map(|raw_name| {
            let raw = (*raw_name).to_owned();
            let merry_name = mapped_by_raw
                .get(&raw)
                .expect("raw name was inserted")
                .clone();
            Ok(McpToolNameMapping {
                raw_name: raw,
                merry_name,
            })
        })
        .collect()
}

fn slug_component(value: &str) -> String {
    let mut slug = String::new();
    let mut previous_separator = false;

    for byte in value.bytes() {
        let ch = if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' {
            byte as char
        } else {
            '-'
        };

        if ch == '-' {
            if slug.is_empty() || previous_separator {
                continue;
            }
            previous_separator = true;
        } else {
            previous_separator = false;
        }
        slug.push(ch);
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    slug
}

fn compact_tool_name(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        return value.to_owned();
    }

    value.chars().take(max_len).collect()
}

fn append_hash_suffix(base: &str, raw: &str) -> String {
    const HASH_LEN: usize = 8;
    let suffix = format!("-{:08x}", fnv1a32(raw.as_bytes()));
    let prefix_len = 64usize.saturating_sub(HASH_LEN + 1);
    let prefix = compact_tool_name(base, prefix_len);
    format!("{prefix}{suffix}")
}

fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash = 0x811c9dc5u32;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_simple_names_with_server_prefix() {
        let mappings = map_mcp_tool_names("context7", &["resolve-library-id"]).unwrap();

        assert_eq!(mappings[0].raw_name, "resolve-library-id");
        assert_eq!(
            mappings[0].merry_name.as_str(),
            "mcp_context7_resolve-library-id"
        );
    }

    #[test]
    fn maps_dotted_and_spaced_names_to_valid_tool_names() {
        let mappings = map_mcp_tool_names("docs.server", &["read.file", "search docs"]).unwrap();

        assert_eq!(mappings[0].merry_name.as_str(), "mcp_docs-server_read-file");
        assert_eq!(
            mappings[1].merry_name.as_str(),
            "mcp_docs-server_search-docs"
        );
    }

    #[test]
    fn maps_long_names_under_merry_limit() {
        let mappings = map_mcp_tool_names(
            "very-long-server-name",
            &["extremely-long-tool-name-that-would-overflow-the-merry-tool-name-limit"],
        )
        .unwrap();

        let name = mappings[0].merry_name.as_str();
        assert!(name.starts_with("mcp_very-long-server-name_"));
        assert!(name.len() <= 64);
    }

    #[test]
    fn maps_collision_groups_deterministically_by_raw_name() {
        let forward = map_mcp_tool_names("server", &["read.file", "read/file"]).unwrap();
        let reverse = map_mcp_tool_names("server", &["read/file", "read.file"]).unwrap();

        let forward_read_file = forward
            .iter()
            .find(|mapping| mapping.raw_name == "read.file")
            .unwrap()
            .merry_name
            .as_str()
            .to_owned();
        let reverse_read_file = reverse
            .iter()
            .find(|mapping| mapping.raw_name == "read.file")
            .unwrap()
            .merry_name
            .as_str()
            .to_owned();
        let forward_read_slash = forward
            .iter()
            .find(|mapping| mapping.raw_name == "read/file")
            .unwrap()
            .merry_name
            .as_str()
            .to_owned();
        let reverse_read_slash = reverse
            .iter()
            .find(|mapping| mapping.raw_name == "read/file")
            .unwrap()
            .merry_name
            .as_str()
            .to_owned();

        assert_eq!(forward_read_file, reverse_read_file);
        assert_eq!(forward_read_slash, reverse_read_slash);
        assert_ne!(forward_read_file, forward_read_slash);
    }
}
