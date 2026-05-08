use std::{path::Path, sync::Arc};

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResult, Content, Implementation, JsonObject,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
        ToolAnnotations,
    },
    service::{RequestContext, ServerInitializeError},
    transport::stdio,
};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use thiserror::Error;

use crate::generation::{
    ApplyRequest, CoreError, GenerationCore, GenerationRequest, ValidateConfigRequest,
};

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("failed to initialize MCP server: {0}")]
    Initialize(#[from] Box<ServerInitializeError>),
    #[error("MCP server task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("failed to create async runtime: {0}")]
    Runtime(#[from] std::io::Error),
}

pub fn serve_stdio(root: impl AsRef<Path>, read_only: bool) -> Result<(), ServerError> {
    let core = GenerationCore::with_read_only(root, read_only)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;

    runtime.block_on(async move {
        let service = GenifyMcpServer::new(core)
            .serve(stdio())
            .await
            .map_err(Box::new)?;
        service.waiting().await?;
        Ok(())
    })
}

#[derive(Debug, Clone)]
struct GenifyMcpServer {
    core: GenerationCore,
}

impl GenifyMcpServer {
    fn new(core: GenerationCore) -> Self {
        Self { core }
    }

    fn call_genify_tool(&self, request: CallToolRequestParams) -> Result<CallToolResult, McpError> {
        let arguments = request
            .arguments
            .map(JsonValue::Object)
            .unwrap_or_else(|| JsonValue::Object(Default::default()));

        match request.name.as_ref() {
            "genify_plan" => Ok(self.tool_call(arguments, false, |core, args| {
                let input: GenerationRequest = parse_arguments(args)?;
                let output = core.plan(input).map_err(core_error_payload)?;
                serde_json::to_value(output).map_err(serialization_error)
            })),
            "genify_diff" => Ok(self.tool_call(arguments, false, |core, args| {
                let input: GenerationRequest = parse_arguments(args)?;
                let output = core.diff(input).map_err(core_error_payload)?;
                let is_error = !output.errors.is_empty();
                let value = serde_json::to_value(output).map_err(serialization_error)?;
                Ok((value, is_error))
            })),
            "genify_apply" => Ok(self.tool_call(arguments, false, |core, args| {
                let input: ApplyRequest = parse_arguments(args)?;
                let output = core.apply(input).map_err(core_error_payload)?;
                let is_error = !output.errors.is_empty();
                let value = serde_json::to_value(output).map_err(serialization_error)?;
                Ok((value, is_error))
            })),
            "genify_validate_config" => Ok(self.tool_call(arguments, false, |core, args| {
                let input: ValidateConfigRequest = parse_arguments(args)?;
                let output = core.validate_config(input).map_err(core_error_payload)?;
                let is_error = !output.valid;
                let value = serde_json::to_value(output).map_err(serialization_error)?;
                Ok((value, is_error))
            })),
            "genify_list_templates" => Ok(self.tool_call(arguments, false, |core, args| {
                parse_empty_arguments(args)?;
                let output = core.list_templates().map_err(core_error_payload)?;
                serde_json::to_value(output).map_err(serialization_error)
            })),
            name => Err(McpError::invalid_params(
                format!("Unknown tool: {name}"),
                None,
            )),
        }
    }

    fn tool_call<T>(
        &self,
        arguments: JsonValue,
        default_is_error: bool,
        f: impl FnOnce(&GenerationCore, JsonValue) -> Result<T, JsonValue>,
    ) -> CallToolResult
    where
        T: Into<JsonValueOrErrorFlag>,
    {
        match f(&self.core, arguments) {
            Ok(value) => match value.into() {
                JsonValueOrErrorFlag::Value(value) => tool_result(value, default_is_error),
                JsonValueOrErrorFlag::ValueWithErrorFlag { value, is_error } => {
                    tool_result(value, is_error)
                }
            },
            Err(error_payload) => tool_result(error_payload, true),
        }
    }
}

impl ServerHandler for GenifyMcpServer {
    fn get_info(&self) -> ServerInfo {
        let mut server_info = Implementation::new("genify", env!("CARGO_PKG_VERSION"));
        server_info.title = Some("genify".to_string());

        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(server_info)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.call_genify_tool(request)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|tool| tool.name == name)
    }
}

enum JsonValueOrErrorFlag {
    Value(JsonValue),
    ValueWithErrorFlag { value: JsonValue, is_error: bool },
}

impl From<JsonValue> for JsonValueOrErrorFlag {
    fn from(value: JsonValue) -> Self {
        Self::Value(value)
    }
}

impl From<(JsonValue, bool)> for JsonValueOrErrorFlag {
    fn from((value, is_error): (JsonValue, bool)) -> Self {
        Self::ValueWithErrorFlag { value, is_error }
    }
}

fn parse_arguments<T>(arguments: JsonValue) -> Result<T, JsonValue>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(arguments).map_err(|err| {
        json!({
            "error": {
                "kind": "invalid_arguments",
                "message": err.to_string()
            }
        })
    })
}

fn parse_empty_arguments(arguments: JsonValue) -> Result<(), JsonValue> {
    match arguments {
        JsonValue::Object(object) if object.is_empty() => Ok(()),
        JsonValue::Null => Ok(()),
        _ => Err(json!({
            "error": {
                "kind": "invalid_arguments",
                "message": "this tool does not accept arguments"
            }
        })),
    }
}

fn serialization_error(err: serde_json::Error) -> JsonValue {
    json!({
        "error": {
            "kind": "serialization",
            "message": err.to_string()
        }
    })
}

fn core_error_payload(err: CoreError) -> JsonValue {
    json!({
        "error": {
            "kind": core_error_kind(&err),
            "message": err.to_string()
        }
    })
}

fn core_error_kind(err: &CoreError) -> &'static str {
    match err {
        CoreError::InvalidRoot { .. } => "invalid_root",
        CoreError::PathOutsideRoot { .. } => "path_outside_root",
        CoreError::InvalidPath { .. } => "invalid_path",
        CoreError::InvalidConfig { .. } => "invalid_config",
        CoreError::MissingConfig => "missing_config",
        CoreError::NotAFile { .. } => "not_a_file",
        CoreError::NotADirectory { .. } => "not_a_directory",
        CoreError::ReadFile { .. } => "read_file",
        CoreError::WriteFile { .. } => "write_file",
        CoreError::CreateDirectory { .. } => "create_directory",
        CoreError::ParseToml { .. } => "parse_toml",
        CoreError::Render(_) => "render",
        CoreError::ApprovalRequired => "approval_required",
        CoreError::ReadOnly => "read_only",
    }
}

fn tool_result(structured_content: JsonValue, is_error: bool) -> CallToolResult {
    let text = serde_json::to_string_pretty(&structured_content)
        .unwrap_or_else(|_| "{\"error\":\"failed to serialize tool result\"}".to_string());

    let mut result = if is_error {
        CallToolResult::error(vec![Content::text(text)])
    } else {
        CallToolResult::success(vec![Content::text(text)])
    };
    result.structured_content = Some(structured_content);
    result
}

fn tools() -> Vec<Tool> {
    vec![
        tool(
            "genify_plan",
            "Plan genify changes",
            tool_description(
                "Return the file operations genify would perform without modifying disk.",
            ),
            generation_input_schema(),
            Some(plan_output_schema()),
            ToolAnnotations::new().read_only(true).destructive(false),
        ),
        tool(
            "genify_diff",
            "Diff genify changes",
            tool_description(
                "Render the JSON genify config in dry-run mode and return a unified diff.",
            ),
            generation_input_schema(),
            Some(diff_output_schema()),
            ToolAnnotations::new().read_only(true).destructive(false),
        ),
        tool(
            "genify_apply",
            "Apply genify changes",
            tool_description(
                "Apply generated changes to disk. Requires explicit_approval=true or confirm_token=\"apply\".",
            ),
            apply_input_schema(),
            Some(apply_output_schema()),
            ToolAnnotations::new().read_only(false).destructive(true),
        ),
        tool(
            "genify_validate_config",
            "Validate genify config",
            tool_description(
                "Validate a JSON genify config and rendered output paths without applying changes. Invalid input returns hint.minimal_config and operation examples.",
            ),
            validate_config_input_schema(),
            Some(validate_config_output_schema()),
            ToolAnnotations::new().read_only(true).destructive(false),
        ),
        tool(
            "genify_list_templates",
            "List genify templates",
            "List genify config/template files discovered under the MCP root.",
            no_input_schema(),
            Some(list_templates_output_schema()),
            ToolAnnotations::new().read_only(true).destructive(false),
        ),
    ]
}

fn tool(
    name: &'static str,
    title: &'static str,
    description: impl Into<std::borrow::Cow<'static, str>>,
    input_schema: JsonValue,
    output_schema: Option<JsonValue>,
    annotations: ToolAnnotations,
) -> Tool {
    let tool = Tool::new(name, description, Arc::new(schema_object(input_schema)))
        .with_title(title)
        .with_annotations(annotations);

    if let Some(output_schema) = output_schema {
        tool.with_raw_output_schema(Arc::new(schema_object(output_schema)))
    } else {
        tool
    }
}

fn schema_object(schema: JsonValue) -> JsonObject {
    match schema {
        JsonValue::Object(object) => object,
        _ => JsonObject::new(),
    }
}

fn generation_input_schema() -> JsonValue {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "config": config_schema(),
            "root": {
                "type": "string",
                "description": "Optional generation root, constrained to the MCP server --root."
            }
        },
        "required": ["config"]
    })
}

fn config_schema() -> JsonValue {
    json!({
        "type": "object",
        "additionalProperties": false,
                "description": "Inline genify config as JSON. Minimum: {\"rules\":[{\"type\":\"replace\",\"path\":\"src/application.rs\",\"replace\":\"old text\",\"content\":\"new text\"}]}",
        "properties": {
            "props": {
                "type": "object",
                "description": "Optional template props used by Tera expressions in paths and content."
            },
            "rules": {
                "type": "array",
                "description": "Generation rules. Supported types: write, delete, rename, move, copy, mkdir, chmod, append, append_once, prepend, insert_before, insert_after, replace, replace_or_append, managed_block.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "type": {
                            "type": "string",
                            "enum": [
                                "write",
                                "delete",
                                "rename",
                                "move",
                                "copy",
                                "mkdir",
                                "chmod",
                                "append",
                                "append_once",
                                "prepend",
                                "insert_before",
                                "insert_after",
                                "replace",
                                "replace_or_append",
                                "managed_block"
                            ]
                        },
                        "path": {
                            "type": "string"
                        },
                        "from": {
                            "type": "string",
                            "description": "Source path for rename, move, and copy."
                        },
                        "to": {
                            "type": "string",
                            "description": "Target path for rename, move, and copy."
                        },
                        "mode": {
                            "type": "string",
                            "description": "Unix chmod mode, for example \"755\" or \"644\"."
                        },
                        "if_exists": {
                            "type": "string",
                            "enum": [
                                "overwrite",
                                "error",
                                "skip"
                            ],
                            "description": "Required for write. overwrite replaces an existing file, error fails when the target exists, skip leaves an existing file unchanged."
                        },
                        "marker": {
                            "type": "string",
                            "description": "Marker text for insert_before and insert_after."
                        },
                        "start_marker": {
                            "type": "string",
                            "description": "Start marker for managed_block."
                        },
                        "end_marker": {
                            "type": "string",
                            "description": "End marker for managed_block."
                        },
                        "replace": {
                            "type": "string",
                            "description": "Regex used for replace and replace_or_append rules."
                        },
                        "replace_all": {
                            "type": "boolean",
                            "description": "When true, replace every regex match. Defaults to false. When false, replace fails unless the match count equals expected_matches, default 1."
                        },
                        "expected_matches": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "Expected regex match count for replace and replace_or_append. Defaults to 1 when replace_all is false."
                        },
                        "content": {
                            "type": "string"
                        }
                    },
                    "required": ["type"]
                }
            }
        },
        "required": ["rules"]
    })
}

fn apply_input_schema() -> JsonValue {
    let mut schema = generation_input_schema();
    schema["properties"]["confirm_token"] = json!({
        "type": "string",
        "description": "Set to \"apply\" to approve disk changes."
    });
    schema["properties"]["explicit_approval"] = json!({
        "type": "boolean",
        "description": "Set true to approve disk changes."
    });
    schema
}

fn validate_config_input_schema() -> JsonValue {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "config": config_schema()
        },
        "required": ["config"]
    })
}

fn no_input_schema() -> JsonValue {
    json!({
        "type": "object",
        "additionalProperties": false
    })
}

fn plan_output_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "operations": { "type": "array" },
            "affected_paths": { "type": "array", "items": { "type": "string" } },
            "warnings": { "type": "array" },
            "errors": { "type": "array" }
        },
        "required": ["operations", "affected_paths", "warnings", "errors"]
    })
}

fn diff_output_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "diff": { "type": "string" },
            "summary": { "type": "object" },
            "warnings": { "type": "array" },
            "errors": { "type": "array" }
        },
        "required": ["diff", "summary", "warnings", "errors"]
    })
}

fn apply_output_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "changed_files": { "type": "array", "items": { "type": "string" } },
            "summary": { "type": "string" },
            "warnings": { "type": "array" },
            "errors": { "type": "array" }
        },
        "required": ["changed_files", "summary", "warnings", "errors"]
    })
}

fn validate_config_output_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "valid": { "type": "boolean" },
            "diagnostics": { "type": "array" },
            "hint": { "type": "object" }
        },
        "required": ["valid", "diagnostics"]
    })
}

fn list_templates_output_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "items": { "type": "array" }
        },
        "required": ["items"]
    })
}

fn tool_description(summary: &str) -> String {
    format!(
        "{summary}\n\nPass config directly as JSON in tool arguments; no config/template file is required.\n\nMinimal replace config:\n{}\n\nAppend once example:\n{}\n\nManaged block example:\n{}\n\nInsert after marker example:\n{}\n\nMove example:\n{}\n\nWrite example:\n{}",
        r#"{"config":{"rules":[{"type":"replace","path":"src/application.rs","replace":"old text","content":"new text"}]}}"#,
        r#"{"config":{"rules":[{"type":"append_once","path":"README.md","content":"..."}]}}"#,
        r#"{"config":{"rules":[{"type":"managed_block","path":"README.md","start_marker":"<!-- genify:start -->","end_marker":"<!-- genify:end -->","content":"..."}]}}"#,
        "{\"config\":{\"rules\":[{\"type\":\"insert_after\",\"path\":\"README.md\",\"marker\":\"## Usage\",\"content\":\"...\"}]}}",
        r#"{"config":{"rules":[{"type":"move","from":"old.rs","to":"new.rs"}]}}"#,
        r#"{"config":{"rules":[{"type":"write","path":"new/file.rs","content":"...","if_exists":"error"}]}}"#,
    )
}
