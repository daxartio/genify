use std::{
    io::{self, BufRead, Write},
    path::Path,
};

use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use thiserror::Error;

use crate::generation::{
    ApplyRequest, CoreError, GenerationCore, GenerationRequest, ValidateConfigRequest,
};

const PROTOCOL_VERSION: &str = "2025-11-25";
const SUPPORTED_PROTOCOLS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("MCP I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("MCP JSON serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn serve_stdio(root: impl AsRef<Path>, read_only: bool) -> Result<(), ServerError> {
    let core = GenerationCore::with_read_only(root, read_only)?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve(stdin.lock(), stdout.lock(), core)
}

pub fn serve<R, W>(reader: R, writer: W, core: GenerationCore) -> Result<(), ServerError>
where
    R: BufRead,
    W: Write,
{
    McpServer::new(core).serve(reader, writer)
}

#[derive(Debug, Clone)]
struct McpServer {
    core: GenerationCore,
}

impl McpServer {
    fn new(core: GenerationCore) -> Self {
        Self { core }
    }

    fn serve<R, W>(&self, reader: R, mut writer: W) -> Result<(), ServerError>
    where
        R: BufRead,
        W: Write,
    {
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let response = match serde_json::from_str::<JsonValue>(&line) {
                Ok(message) => self.handle_message(message),
                Err(err) => Some(error_response(
                    JsonValue::Null,
                    -32700,
                    "Parse error",
                    Some(json!({ "message": err.to_string() })),
                )),
            };

            if let Some(response) = response {
                serde_json::to_writer(&mut writer, &response)?;
                writer.write_all(b"\n")?;
                writer.flush()?;
            }
        }

        Ok(())
    }

    fn handle_message(&self, message: JsonValue) -> Option<JsonValue> {
        match message {
            JsonValue::Array(messages) => {
                if messages.is_empty() {
                    return Some(error_response(
                        JsonValue::Null,
                        -32600,
                        "Invalid Request",
                        Some(json!({ "message": "empty JSON-RPC batch" })),
                    ));
                }
                let responses = messages
                    .into_iter()
                    .filter_map(|message| self.handle_single_message(message))
                    .collect::<Vec<_>>();
                if responses.is_empty() {
                    None
                } else {
                    Some(JsonValue::Array(responses))
                }
            }
            message => self.handle_single_message(message),
        }
    }

    fn handle_single_message(&self, message: JsonValue) -> Option<JsonValue> {
        let JsonValue::Object(object) = message else {
            return Some(error_response(
                JsonValue::Null,
                -32600,
                "Invalid Request",
                Some(json!({ "message": "message must be a JSON object" })),
            ));
        };

        if object.contains_key("result") || object.contains_key("error") {
            return None;
        }

        let is_notification = !object.contains_key("id");
        let id = object.get("id").cloned().unwrap_or(JsonValue::Null);
        let Some(method) = object.get("method").and_then(JsonValue::as_str) else {
            if is_notification {
                return None;
            }
            return Some(error_response(
                id,
                -32600,
                "Invalid Request",
                Some(json!({ "message": "request method must be a string" })),
            ));
        };
        let params = object.get("params").cloned().unwrap_or(JsonValue::Null);

        if is_notification {
            self.handle_notification(method);
            return None;
        }

        Some(match method {
            "initialize" => success_response(id, self.initialize(params)),
            "ping" => success_response(id, json!({})),
            "tools/list" => success_response(id, json!({ "tools": tools() })),
            "tools/call" => match self.call_tool(params) {
                Ok(result) => success_response(id, result),
                Err(err) => error_response(
                    id,
                    -32602,
                    "Invalid params",
                    Some(json!({ "message": err })),
                ),
            },
            "resources/list" => success_response(id, json!({ "resources": [] })),
            "prompts/list" => success_response(id, json!({ "prompts": [] })),
            _ => error_response(
                id,
                -32601,
                "Method not found",
                Some(json!({ "method": method })),
            ),
        })
    }

    fn handle_notification(&self, method: &str) {
        if method != "notifications/initialized" {
            eprintln!("genify mcp: ignored notification {method}");
        }
    }

    fn initialize(&self, params: JsonValue) -> JsonValue {
        let requested = params
            .get("protocolVersion")
            .and_then(JsonValue::as_str)
            .unwrap_or(PROTOCOL_VERSION);
        let protocol_version = if SUPPORTED_PROTOCOLS.contains(&requested) {
            requested
        } else {
            PROTOCOL_VERSION
        };

        json!({
            "protocolVersion": protocol_version,
            "capabilities": {
                "tools": {
                    "listChanged": false
                }
            },
            "serverInfo": {
                "name": "genify",
                "title": "genify",
                "version": env!("CARGO_PKG_VERSION")
            }
        })
    }

    fn call_tool(&self, params: JsonValue) -> Result<JsonValue, String> {
        let params: ToolCallParams = serde_json::from_value(params)
            .map_err(|err| format!("tools/call params are invalid: {err}"))?;
        let arguments = match params.arguments {
            JsonValue::Null => JsonValue::Object(Default::default()),
            value => value,
        };

        match params.name.as_str() {
            "genify_plan" => self.tool_call(arguments, false, |core, args| {
                let input: GenerationRequest = parse_arguments(args)?;
                let output = core.plan(input).map_err(core_error_payload)?;
                serde_json::to_value(output).map_err(serialization_error)
            }),
            "genify_diff" => self.tool_call(arguments, false, |core, args| {
                let input: GenerationRequest = parse_arguments(args)?;
                let output = core.diff(input).map_err(core_error_payload)?;
                let is_error = !output.errors.is_empty();
                let value = serde_json::to_value(output).map_err(serialization_error)?;
                Ok((value, is_error))
            }),
            "genify_apply" => self.tool_call(arguments, false, |core, args| {
                let input: ApplyRequest = parse_arguments(args)?;
                let output = core.apply(input).map_err(core_error_payload)?;
                let is_error = !output.errors.is_empty();
                let value = serde_json::to_value(output).map_err(serialization_error)?;
                Ok((value, is_error))
            }),
            "genify_validate_config" => self.tool_call(arguments, false, |core, args| {
                let input: ValidateConfigRequest = parse_arguments(args)?;
                let output = core.validate_config(input).map_err(core_error_payload)?;
                let is_error = !output.valid;
                let value = serde_json::to_value(output).map_err(serialization_error)?;
                Ok((value, is_error))
            }),
            "genify_list_templates" => self.tool_call(arguments, false, |core, args| {
                parse_empty_arguments(args)?;
                let output = core.list_templates().map_err(core_error_payload)?;
                serde_json::to_value(output).map_err(serialization_error)
            }),
            _ => Err(format!("Unknown tool: {}", params.name)),
        }
    }

    fn tool_call<T>(
        &self,
        arguments: JsonValue,
        default_is_error: bool,
        f: impl FnOnce(&GenerationCore, JsonValue) -> Result<T, JsonValue>,
    ) -> Result<JsonValue, String>
    where
        T: Into<JsonValueOrErrorFlag>,
    {
        match f(&self.core, arguments) {
            Ok(value) => match value.into() {
                JsonValueOrErrorFlag::Value(value) => Ok(tool_result(value, default_is_error)),
                JsonValueOrErrorFlag::ValueWithErrorFlag { value, is_error } => {
                    Ok(tool_result(value, is_error))
                }
            },
            Err(error_payload) => Ok(tool_result(error_payload, true)),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ToolCallParams {
    name: String,
    #[serde(default)]
    arguments: JsonValue,
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

fn tool_result(structured_content: JsonValue, is_error: bool) -> JsonValue {
    let text = serde_json::to_string_pretty(&structured_content)
        .unwrap_or_else(|_| "{\"error\":\"failed to serialize tool result\"}".to_string());
    json!({
        "content": [
            {
                "type": "text",
                "text": text
            }
        ],
        "structuredContent": structured_content,
        "isError": is_error
    })
}

fn success_response(id: JsonValue, result: JsonValue) -> JsonValue {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn error_response(
    id: JsonValue,
    code: i64,
    message: &'static str,
    data: Option<JsonValue>,
) -> JsonValue {
    let mut error = json!({
        "code": code,
        "message": message
    });
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error
    })
}

fn tools() -> Vec<JsonValue> {
    vec![
        json!({
            "name": "genify_plan",
            "title": "Plan genify changes",
            "description": tool_description("Return the file operations genify would perform without modifying disk."),
            "inputSchema": generation_input_schema(),
            "outputSchema": plan_output_schema(),
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false
            }
        }),
        json!({
            "name": "genify_diff",
            "title": "Diff genify changes",
            "description": tool_description("Render the JSON genify config in dry-run mode and return a unified diff."),
            "inputSchema": generation_input_schema(),
            "outputSchema": diff_output_schema(),
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false
            }
        }),
        json!({
            "name": "genify_apply",
            "title": "Apply genify changes",
            "description": tool_description("Apply generated changes to disk. Requires explicit_approval=true or confirm_token=\"apply\"."),
            "inputSchema": apply_input_schema(),
            "outputSchema": apply_output_schema(),
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": true
            }
        }),
        json!({
            "name": "genify_validate_config",
            "title": "Validate genify config",
            "description": tool_description("Validate a JSON genify config and rendered output paths without applying changes. Invalid input returns hint.minimal_config and operation examples."),
            "inputSchema": validate_config_input_schema(),
            "outputSchema": validate_config_output_schema(),
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false
            }
        }),
        json!({
            "name": "genify_list_templates",
            "title": "List genify templates",
            "description": "List genify config/template files discovered under the MCP root.",
            "inputSchema": no_input_schema(),
            "outputSchema": list_templates_output_schema(),
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false
            }
        }),
    ]
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
                "description": "Generation rules. Supported types: replace, append, prepend, file.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "type": {
                            "type": "string",
                            "enum": ["replace", "append", "prepend", "file"]
                        },
                        "path": {
                            "type": "string"
                        },
                        "replace": {
                            "type": "string",
                            "description": "Regex used only for replace rules."
                        },
                        "content": {
                            "type": "string"
                        }
                    },
                    "required": ["type", "path", "content"]
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
        "{summary}\n\nPass config directly as JSON in tool arguments; no config/template file is required.\n\nMinimal replace config:\n{}\n\nAppend example:\n{}\n\nPrepend example:\n{}\n\nFile example:\n{}",
        r#"{"config":{"rules":[{"type":"replace","path":"src/application.rs","replace":"old text","content":"new text"}]}}"#,
        r#"{"config":{"rules":[{"type":"append","path":"README.md","content":"..."}]}}"#,
        r#"{"config":{"rules":[{"type":"prepend","path":"README.md","content":"..."}]}}"#,
        r#"{"config":{"rules":[{"type":"file","path":"new/file.rs","content":"..."}]}}"#,
    )
}
