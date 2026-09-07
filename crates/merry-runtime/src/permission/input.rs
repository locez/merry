use super::{
    DEFAULT_PERMISSION_STDERR_LIMIT_BYTES, DEFAULT_PERMISSION_STDOUT_LIMIT_BYTES,
    MAX_PERMISSION_REASON_BYTES, REQUEST_PERMISSIONS_TOOL_NAME,
    permission_invalid_arguments_outcome,
};
use super::{
    HostIntegration, PermissionAdmissionError, PermissionRequest, PermissionReviewContextEntry,
    PermissionedAction, RequestedCapability, RequestedPathCapability,
};
use crate::{
    MAX_PROCESS_ARG_BYTES, MAX_PROCESS_CWD_BYTES, PathAccess, ProcessActionIntent,
    ProcessEnvPolicy, RegisteredTool, ToolActionKind, ToolExecutionContext, ToolExecutionError,
    ToolExecutor, ToolExecutorFuture,
};
use merry_core::{PendingToolCall, ToolName};
use merry_tools_macros::tool;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, sync::Arc};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestedPathInput {
    #[schemars(
        description = "Path requested for additional filesystem access.",
        length(min = 1)
    )]
    pub(crate) path: String,
    #[schemars(description = "Requested access for this path: ro, rw, or deny.")]
    pub(crate) access: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestedCapabilitiesInput {
    #[serde(default)]
    #[schemars(description = "Set true to request network capability for the exact action.")]
    pub(crate) network: bool,
    #[serde(default)]
    #[schemars(
        description = "Exact filesystem paths to authorize. Configured review_paths stay hidden until explicitly requested and are approved only for this action, as are Git metadata writes. Ordinary path grants may be retained by a session-aware backend. Each item specifies a path and ro, rw, or deny access; approval never overrides a configured read-only ceiling or denial."
    )]
    pub(crate) paths: Vec<RequestedPathInput>,
    #[serde(default)]
    #[schemars(
        description = "Explicitly configured host integrations: ssh-agent, dbus, or gpg-agent. SSH includes its agent and read-only known_hosts; GPG includes read-only public keys and its native agent, not the SSH socket. File sources still obey path restrictions; neither integration authorizes networking. dbus is the session bus used by keyring clients."
    )]
    pub(crate) host_integrations: Vec<String>,
}

#[derive(Debug, Clone, JsonSchema)]
pub(crate) struct PermissionedProcessInput {
    #[schemars(
        description = "Exact shell command to run if approved. Newline and tab are allowed; other control characters are rejected. JSON strings must escape embedded control characters.",
        length(min = 1, max = MAX_PROCESS_ARG_BYTES)
    )]
    pub(crate) command: String,
    #[serde(default)]
    #[schemars(
        schema_with = "process_cwd_schema_for_schemars",
        description = "Optional workspace-relative working directory for process actions. Omit it, use \".\", or use null for the current workspace directory; an empty string is accepted as the same root default for compatibility."
    )]
    pub(crate) cwd: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionedProcessInputWire {
    #[serde(deserialize_with = "crate::tool_input::deserialize_non_empty_process_command")]
    command: String,
    #[serde(default)]
    cwd: Option<String>,
}

impl<'de> Deserialize<'de> for PermissionedProcessInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let input = PermissionedProcessInputWire::deserialize(deserializer)?;
        Ok(Self {
            command: input.command,
            cwd: input.cwd.unwrap_or_default(),
        })
    }
}

#[tool(
    crate = "crate",
    name = "request_permissions",
    description = "Request additional filesystem, network, or explicitly configured host-integration capability for one exact planned action."
)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RequestPermissionsInput {
    #[serde(default)]
    #[schemars(
        schema_with = "permission_reason_schema_for_schemars",
        description = "Optional short explanation of why the current task needs the requested capability. Null is treated as omitted; a provided string must be non-blank and within the byte limit."
    )]
    reason: Option<String>,
    #[schemars(schema_with = "requested_capabilities_schema_for_schemars")]
    requested: RequestedCapabilitiesInput,
    #[schemars(
        schema_with = "permissioned_process_schema_for_schemars",
        description = "The exact process action that will run after admission. It does not grant access to paths that are not listed in requested."
    )]
    for_action: PermissionedProcessInput,
}
pub fn request_permissions_tool() -> Result<RegisteredTool, PermissionAdmissionError> {
    let spec = RequestPermissionsInput::tool_spec_with(
        REQUEST_PERMISSIONS_TOOL_NAME,
        "Request additional filesystem, network, or explicitly configured host-integration capability for one exact planned action. A configured session-aware process backend retains approved paths and host integrations for later actions in the current runtime session, but network access must be requested again for every action. When one command needs multiple capabilities, request them together.",
    )
    .map_err(|error| PermissionAdmissionError::Core {
        source: error.into(),
    })?;
    Ok(RegisteredTool::new(
        spec,
        Arc::new(RequestPermissionsToolExecutor),
        ToolActionKind::RuntimeControl,
    ))
}

pub(crate) fn is_request_permissions_tool(tool_name: &ToolName) -> bool {
    tool_name.as_str() == REQUEST_PERMISSIONS_TOOL_NAME
}

pub(crate) fn permission_request_from_call(
    call: &PendingToolCall,
    review_context: Vec<PermissionReviewContextEntry>,
) -> Result<PermissionRequest, PermissionAdmissionError> {
    let input = call
        .arguments()
        .deserialize_as::<RequestPermissionsInput>()
        .map_err(|error| PermissionAdmissionError::InvalidArguments {
            message: format!(
                "request_permissions arguments must match the declared input schema: {error}"
            ),
        })?;
    let requested = requested_capabilities(&input.requested)?;
    let action = permissioned_action(&input.for_action)?;
    PermissionRequest::new(call, input.reason, requested, action, review_context)
}

pub(crate) fn permission_request_from_process_call(
    call: &PendingToolCall,
    intent: ProcessActionIntent,
    review_context: Vec<PermissionReviewContextEntry>,
) -> Result<PermissionRequest, PermissionAdmissionError> {
    let arguments = call.arguments().as_object();
    let reason = optional_string(arguments.get("reason"), "reason")?;
    let Some(permissions) = arguments.get("permissions") else {
        return Err(PermissionAdmissionError::InvalidArguments {
            message: "run_process.permissions is required when run_process.reason is provided"
                .to_owned(),
        });
    };
    let requested = requested_capabilities_from_value(permissions)?;
    PermissionRequest::new(
        call,
        reason,
        requested,
        PermissionedAction::Process(intent),
        review_context,
    )
}

/// Parses a permission request tool call without additional review context.
///
/// Runtime-owned adapters may use this boundary when they need to inspect the
/// same normalized request type as runtime admission. Review context is added
/// only by the runtime execution path and is intentionally not exposed here.
pub fn parse_permission_request(
    call: &PendingToolCall,
) -> Result<PermissionRequest, PermissionAdmissionError> {
    permission_request_from_call(call, Vec::new())
}

#[derive(Debug)]
struct RequestPermissionsToolExecutor;

impl ToolExecutor for RequestPermissionsToolExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if let Err(error) = permission_request_from_call(&call, Vec::new()) {
                return Ok(permission_invalid_arguments_outcome(
                    call.name().as_str(),
                    error,
                ));
            }
            Err(ToolExecutionError::infrastructure(
                "request_permissions must be executed through runtime permission admission",
            ))
        })
    }
}

fn permission_reason_schema_json() -> Value {
    json!({
        "description": "Optional short explanation of why the current task needs the requested capability. Null is treated as omitted; a provided string must be non-blank and within the byte limit.",
        "anyOf": [
            { "type": "null" },
            {
                "type": "string",
                "minLength": 1,
                "maxLength": MAX_PERMISSION_REASON_BYTES
            }
        ]
    })
}

fn requested_capabilities_schema_json() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "description": "Capabilities to add for this exact action after approval. Use network for network access, paths for filesystem paths, or host_integrations for SSH agent, native GPG agent, or D-Bus access. A session-aware backend may retain ordinary path and host-integration grants, but network, configured review_paths, and Git metadata writes require approval for every action. Include every capability the same command needs in one request.",
        "properties": {
            "network": {
                "type": "boolean",
                "description": "Set true to request network capability for the exact action."
            },
            "paths": {
                "type": "array",
                "description": "Exact filesystem paths to authorize. Configured review_paths stay hidden until explicitly requested and are approved only for this action, as are Git metadata writes. Ordinary path grants may be retained by a session-aware backend. Each item specifies a path and ro, rw, or deny access; approval never overrides a configured read-only ceiling or denial.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "path": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Path requested for additional filesystem access."
                        },
                        "access": {
                            "type": "string",
                            "enum": ["ro", "rw", "deny"],
                            "description": "Requested access for this path: ro, rw, or deny."
                        }
                    },
                    "required": ["path", "access"]
                }
            },
            "host_integrations": {
                "type": "array",
                "description": "Explicitly configured host integrations: ssh-agent, dbus, or gpg-agent. SSH includes its agent and read-only known_hosts; GPG includes read-only public keys and its native agent, not the SSH socket. File sources still obey path restrictions; neither integration authorizes networking. dbus is the session bus used by keyring clients.",
                "items": {
                    "type": "string",
                    "enum": ["ssh-agent", "dbus", "gpg-agent"]
                },
                "minItems": 1
            }
        },
        "anyOf": [
            {
                "required": ["network"],
                "properties": {
                    "network": {
                        "const": true,
                        "description": "Set true when requesting network capability; this branch makes requested non-empty."
                    }
                }
            },
            {
                "required": ["paths"],
                "properties": {
                    "paths": {
                        "minItems": 1,
                        "description": "Provide at least one filesystem path when network capability is not requested."
                    }
                }
            },
            {
                "required": ["host_integrations"],
                "properties": {
                    "host_integrations": {
                        "minItems": 1,
                        "description": "Provide at least one configured host integration when network and paths are not requested."
                    }
                }
            }
        ]
    })
}

pub(crate) fn permission_reason_schema_for_schemars(_: &mut SchemaGenerator) -> Schema {
    Schema::try_from(permission_reason_schema_json()).expect("permission reason schema is valid")
}

pub(crate) fn requested_capabilities_schema_for_schemars(_: &mut SchemaGenerator) -> Schema {
    Schema::try_from(requested_capabilities_schema_json())
        .expect("requested capabilities schema is valid")
}

pub(crate) fn process_cwd_schema_for_schemars(_: &mut SchemaGenerator) -> Schema {
    Schema::try_from(process_cwd_schema_json()).expect("process cwd schema is valid")
}

pub(crate) fn permissioned_process_schema_for_schemars(_: &mut SchemaGenerator) -> Schema {
    Schema::try_from(json!({
        "type": "object",
        "additionalProperties": false,
        "description": "The exact process action that will run after admission. It does not grant access to paths that are not listed in requested.",
        "properties": {
            "command": {
                "type": "string",
                "minLength": 1,
                "maxLength": MAX_PROCESS_ARG_BYTES,
                "description": "Exact shell command to run if approved. Newline and tab are allowed; other control characters are rejected. JSON strings must escape embedded control characters."
            },
            "cwd": process_cwd_schema_json()
        },
        "required": ["command"]
    }))
    .expect("permissioned process schema is valid")
}

fn process_cwd_schema_json() -> Value {
    json!({
        "description": "Optional workspace-relative working directory for process actions. Omit it, use \".\", or use null for the current workspace directory; an empty string is accepted as the same root default for compatibility.",
        "default": ".",
        "anyOf": [
            { "type": "null" },
            {
                "type": "string",
                "minLength": 1,
                "maxLength": MAX_PROCESS_CWD_BYTES
            }
        ]
    })
}

fn permissioned_action(
    input: &PermissionedProcessInput,
) -> Result<PermissionedAction, PermissionAdmissionError> {
    let argv = crate::process::shell_command_argv(&input.command);
    let cwd = (!input.cwd.is_empty()).then(|| input.cwd.clone());
    let intent = ProcessActionIntent::new(
        argv,
        cwd,
        ProcessEnvPolicy::empty(),
        None,
        DEFAULT_PERMISSION_STDOUT_LIMIT_BYTES,
        DEFAULT_PERMISSION_STDERR_LIMIT_BYTES,
    )
    .map_err(|error| PermissionAdmissionError::InvalidArguments {
        message: error.to_string(),
    })?;
    Ok(PermissionedAction::Process(intent))
}

fn requested_capabilities(
    input: &RequestedCapabilitiesInput,
) -> Result<Vec<RequestedCapability>, PermissionAdmissionError> {
    let mut requested = Vec::new();
    if input.network {
        requested.push(RequestedCapability::Network);
    }

    for path in &input.paths {
        let access = parse_path_access(&path.access)?;
        requested.push(RequestedCapability::Path(RequestedPathCapability::new(
            path.path.clone(),
            access,
        )?));
    }

    for integration in &input.host_integrations {
        requested.push(RequestedCapability::HostIntegration(
            parse_host_integration(integration)?,
        ));
    }

    if requested.is_empty() {
        return Err(PermissionAdmissionError::InvalidArguments {
            message: "requested must include network=true, at least one path, or at least one host integration".to_owned(),
        });
    }

    Ok(requested)
}

fn requested_capabilities_from_value(
    value: &Value,
) -> Result<Vec<RequestedCapability>, PermissionAdmissionError> {
    let input =
        serde_json::from_value::<RequestedCapabilitiesInput>(value.clone()).map_err(|error| {
            PermissionAdmissionError::InvalidArguments {
                message: format!("requested must match the declared input schema: {error}"),
            }
        })?;
    requested_capabilities(&input)
}

fn parse_host_integration(value: &str) -> Result<HostIntegration, PermissionAdmissionError> {
    match value {
        "ssh-agent" => Ok(HostIntegration::SshAgent),
        "dbus" => Ok(HostIntegration::SessionBus),
        "gpg-agent" => Ok(HostIntegration::GpgAgent),
        actual => Err(PermissionAdmissionError::InvalidArguments {
            message: format!("host integration must be ssh-agent|dbus|gpg-agent, got {actual:?}"),
        }),
    }
}

fn parse_path_access(value: &str) -> Result<PathAccess, PermissionAdmissionError> {
    match value {
        "ro" => Ok(PathAccess::ReadOnly),
        "rw" => Ok(PathAccess::ReadWrite),
        "deny" => Ok(PathAccess::Deny),
        actual => Err(PermissionAdmissionError::InvalidArguments {
            message: format!("path access must be ro|rw|deny, got {actual:?}"),
        }),
    }
}

fn optional_string(
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<String>, PermissionAdmissionError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            validate_optional_reason(text)?;
            Ok(Some(text.clone()))
        }
        Some(_) => Err(PermissionAdmissionError::InvalidArguments {
            message: format!("{field} must be a string when provided"),
        }),
    }
}

pub(crate) fn validate_optional_reason(reason: &str) -> Result<(), PermissionAdmissionError> {
    validate_non_blank("reason", reason)?;
    if reason.len() > MAX_PERMISSION_REASON_BYTES {
        return Err(PermissionAdmissionError::InvalidArguments {
            message: "reason exceeds the byte limit".to_owned(),
        });
    }
    if reason
        .chars()
        .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(PermissionAdmissionError::InvalidArguments {
            message: "reason must not contain control characters other than newline or tab"
                .to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn validate_non_blank(
    field: &'static str,
    value: &str,
) -> Result<(), PermissionAdmissionError> {
    if value.trim().is_empty() {
        return Err(PermissionAdmissionError::InvalidArguments {
            message: format!("{field} must not be blank"),
        });
    }
    Ok(())
}

pub(crate) fn normalize_requested_capabilities(
    requested: Vec<RequestedCapability>,
) -> Result<Vec<RequestedCapability>, PermissionAdmissionError> {
    let mut network = false;
    let mut paths = BTreeMap::new();
    let mut integrations = std::collections::BTreeSet::new();
    for capability in requested {
        match capability {
            RequestedCapability::Network => network = true,
            RequestedCapability::Path(path) => {
                if let Some(previous) = paths.insert(path.path.clone(), path.access)
                    && previous != path.access
                {
                    return Err(PermissionAdmissionError::InvalidArguments {
                        message: format!(
                            "requested paths contain conflicting access for normalized path {:?}",
                            path.path
                        ),
                    });
                }
            }
            RequestedCapability::HostIntegration(integration) => {
                integrations.insert(integration);
            }
        }
    }

    let mut normalized =
        Vec::with_capacity(paths.len() + integrations.len() + usize::from(network));
    if network {
        normalized.push(RequestedCapability::Network);
    }
    normalized.extend(
        paths.into_iter().map(|(path, access)| {
            RequestedCapability::Path(RequestedPathCapability { path, access })
        }),
    );
    normalized.extend(
        integrations
            .into_iter()
            .map(RequestedCapability::HostIntegration),
    );
    Ok(normalized)
}

pub(crate) fn normalize_requested_path(value: &str) -> Result<String, PermissionAdmissionError> {
    validate_non_blank("requested path", value)?;
    if value.chars().any(char::is_control) {
        return Err(PermissionAdmissionError::InvalidArguments {
            message: "requested path must not contain control characters".to_owned(),
        });
    }

    let path = Path::new(value);
    let absolute = path.is_absolute();
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::ParentDir => {
                if !normalized.pop() || (absolute && normalized.as_os_str().is_empty()) {
                    return Err(PermissionAdmissionError::InvalidArguments {
                        message: format!(
                            "requested path {value:?} would escape the workspace root"
                        ),
                    });
                }
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        normalized.push(".");
    }
    Ok(normalized.to_string_lossy().into_owned())
}
