use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use similar::{ChangeTag, TextDiff};
use thiserror::Error;

use crate::{
    Config, Error as GenifyError, IfExists, Rule, Value, parse_toml, render_config_props,
    render_config_rules,
};

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("root must be an existing directory: {path}")]
    InvalidRoot {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("path is outside the configured root: {path}")]
    PathOutsideRoot { path: String },
    #[error("path is invalid: {path}")]
    InvalidPath { path: String },
    #[error("config is required")]
    MissingConfig,
    #[error("invalid config in {label}: {message}")]
    InvalidConfig { label: String, message: String },
    #[error("path is not a file: {path}")]
    NotAFile { path: String },
    #[error("path is not a directory: {path}")]
    NotADirectory { path: String },
    #[error("failed to read {path}: {source}")]
    ReadFile {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("failed to write {path}: {source}")]
    WriteFile {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("failed to create directory {path}: {source}")]
    CreateDirectory {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse TOML in {path}: {source}")]
    ParseToml {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to render config: {0}")]
    Render(#[from] GenifyError),
    #[error("genify_apply requires explicit approval")]
    ApprovalRequired,
    #[error("server is running in read-only mode")]
    ReadOnly,
}

#[derive(Debug, Clone)]
pub struct GenerationCore {
    sandbox: PathSandbox,
    read_only: bool,
}

impl GenerationCore {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, CoreError> {
        Self::with_read_only(root, false)
    }

    pub fn with_read_only(root: impl AsRef<Path>, read_only: bool) -> Result<Self, CoreError> {
        Ok(Self {
            sandbox: PathSandbox::new(root)?,
            read_only,
        })
    }

    pub fn root(&self) -> &Path {
        self.sandbox.root()
    }

    pub fn plan(&self, input: GenerationRequest) -> Result<PlanOutput, CoreError> {
        let prepared = self.prepare(input)?;
        Ok(prepared.plan_output())
    }

    pub fn diff(&self, input: GenerationRequest) -> Result<DiffOutput, CoreError> {
        let prepared = self.prepare(input)?;
        let plan = prepared.plan_output();
        if !plan.errors.is_empty() {
            return Ok(DiffOutput {
                diff: String::new(),
                summary: ChangeSummary::empty(),
                warnings: plan.warnings,
                errors: plan.errors,
            });
        }

        let simulation = self.simulate(&prepared)?;
        Ok(simulation.diff_output(plan.warnings))
    }

    pub fn apply(&self, input: ApplyRequest) -> Result<ApplyOutput, CoreError> {
        if self.read_only {
            return Err(CoreError::ReadOnly);
        }
        if !input.is_approved() {
            return Err(CoreError::ApprovalRequired);
        }

        let prepared = self.prepare(input.generation_request())?;
        let plan = prepared.plan_output();
        if !plan.errors.is_empty() {
            return Ok(ApplyOutput {
                changed_files: Vec::new(),
                summary: "No files changed because generation has errors.".to_string(),
                warnings: plan.warnings,
                errors: plan.errors,
            });
        }

        let simulation = self.simulate(&prepared)?;
        if !simulation.errors.is_empty() {
            return Ok(ApplyOutput {
                changed_files: Vec::new(),
                summary: "No files changed because generation has errors.".to_string(),
                warnings: plan.warnings,
                errors: simulation.errors,
            });
        }
        let changed_files = simulation.changed_relative_paths();

        for change in simulation.changed_files() {
            if change.deleted {
                if change.path.exists() {
                    fs::remove_file(&change.path).map_err(|source| CoreError::WriteFile {
                        path: path_to_string(&change.path),
                        source,
                    })?;
                }
                continue;
            }
            if let Some(parent) = change.path.parent() {
                fs::create_dir_all(parent).map_err(|source| CoreError::CreateDirectory {
                    path: path_to_string(parent),
                    source,
                })?;
            }
            fs::write(&change.path, change.current.as_bytes()).map_err(|source| {
                CoreError::WriteFile {
                    path: path_to_string(&change.path),
                    source,
                }
            })?;
        }
        for change in &simulation.metadata_changes {
            match change.kind {
                MetadataChangeKind::Mkdir => {
                    fs::create_dir_all(&change.path).map_err(|source| {
                        CoreError::CreateDirectory {
                            path: path_to_string(&change.path),
                            source,
                        }
                    })?
                }
                MetadataChangeKind::Chmod => {
                    let Some(mode) = change.mode else {
                        continue;
                    };
                    set_mode(&change.path, mode)?;
                }
            }
        }

        let summary = summarize_changed_files(changed_files.len());
        Ok(ApplyOutput {
            changed_files,
            summary,
            warnings: plan.warnings,
            errors: Vec::new(),
        })
    }

    pub fn validate_config(
        &self,
        input: ValidateConfigRequest,
    ) -> Result<ValidateConfigOutput, CoreError> {
        let reference = match input.config_reference() {
            Ok(reference) => reference,
            Err(err) => {
                return Ok(invalid_config_output(
                    "missing_config",
                    err.to_string(),
                    None::<String>,
                ));
            }
        };

        let source = match self.load_config(reference) {
            Ok(source) => source,
            Err(CoreError::InvalidConfig { label, message }) => {
                return Ok(invalid_config_output(
                    "invalid_config",
                    format!("invalid config in {label}: {message}"),
                    None::<String>,
                ));
            }
            Err(CoreError::ParseToml { path, source }) => {
                return Ok(invalid_config_output(
                    "parse_toml",
                    format!("failed to parse TOML in {path}: {source}"),
                    None::<String>,
                ));
            }
            Err(err) => return Err(err),
        };
        let config = source.config;
        let rendered = render_config_props(config).and_then(render_config_rules)?;

        let mut diagnostics = Vec::new();
        if rendered.rules.is_empty() {
            diagnostics.push(Diagnostic::error(
                "missing_rules",
                "config must contain at least one rule in the rules array",
                None::<String>,
            ));
        }
        let effective_root = self.sandbox.root().to_path_buf();
        for rule in &rendered.rules {
            let raw_path = rule_path(rule);
            if let Err(err) = self
                .sandbox
                .resolve_generated_path(&effective_root, raw_path)
            {
                diagnostics.push(Diagnostic::error(
                    "invalid_path",
                    err.to_string(),
                    Some(raw_path),
                ));
            }
        }

        let valid = diagnostics
            .iter()
            .all(|d| d.severity != DiagnosticSeverity::Error);
        Ok(ValidateConfigOutput {
            valid,
            diagnostics,
            hint: (!valid).then(config_hint),
        })
    }

    pub fn list_templates(&self) -> Result<ListTemplatesOutput, CoreError> {
        let mut items = Vec::new();
        self.collect_templates(self.sandbox.root(), &mut items)?;
        items.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(ListTemplatesOutput { items })
    }

    fn collect_templates(
        &self,
        dir: &Path,
        items: &mut Vec<TemplateInfo>,
    ) -> Result<(), CoreError> {
        let entries = fs::read_dir(dir).map_err(|source| CoreError::ReadFile {
            path: path_to_string(dir),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| CoreError::ReadFile {
                path: path_to_string(dir),
                source,
            })?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if matches!(name.as_str(), ".git" | "target" | "tmp") {
                    continue;
                }
                self.collect_templates(&path, items)?;
                continue;
            }

            let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            let Ok(canonical) = path.canonicalize() else {
                continue;
            };
            if !canonical.starts_with(self.sandbox.root()) {
                continue;
            }
            match extension {
                "toml" => {
                    let metadata = self.config_metadata(&path);
                    items.push(TemplateInfo {
                        name,
                        path: self.sandbox.display_path(&path),
                        kind: TemplateKind::Config,
                        metadata,
                    });
                }
                "hbs" | "tera" | "tpl" => items.push(TemplateInfo {
                    name,
                    path: self.sandbox.display_path(&path),
                    kind: TemplateKind::Template,
                    metadata: JsonValue::Null,
                }),
                _ => {}
            }
        }
        Ok(())
    }

    fn config_metadata(&self, path: &Path) -> JsonValue {
        let Ok(raw) = fs::read_to_string(path) else {
            return json!({ "readable": false });
        };
        match parse_toml(&raw) {
            Ok(config) => json!({
                "readable": true,
                "props_count": config.props.len(),
                "rules_count": config.rules.len()
            }),
            Err(err) => json!({
                "readable": true,
                "parse_error": err.to_string()
            }),
        }
    }

    fn prepare(&self, input: GenerationRequest) -> Result<PreparedGeneration, CoreError> {
        let source = self.load_config(input.config_reference()?)?;
        let effective_root = self
            .sandbox
            .resolve_existing_dir(input.root.as_deref().unwrap_or("."))?;
        let config = source.config;
        let rendered = render_config_props(config).and_then(render_config_rules)?;
        let operations = self.operations_from_config(&effective_root, &rendered)?;

        Ok(PreparedGeneration { operations })
    }

    fn load_config(&self, reference: ConfigReference<'_>) -> Result<ConfigSource, CoreError> {
        let config =
            parse_json_config(reference.value).map_err(|message| CoreError::InvalidConfig {
                label: "inline config".to_string(),
                message,
            })?;
        Ok(ConfigSource { config })
    }

    fn operations_from_config(
        &self,
        effective_root: &Path,
        config: &Config,
    ) -> Result<Vec<PreparedOperation>, CoreError> {
        let mut operations = Vec::with_capacity(config.rules.len());
        for rule in &config.rules {
            let operation = match rule {
                Rule::Write {
                    content, if_exists, ..
                } => PreparedOperation {
                    kind: FileOperationKind::Write,
                    path: self.resolve_rule_path(effective_root, rule_path(rule))?,
                    relative_path: self.relative_rule_path(effective_root, rule_path(rule))?,
                    source_path: None,
                    source_relative_path: None,
                    target_path: None,
                    target_relative_path: None,
                    content: Some(content.clone()),
                    replace: None,
                    replace_all: false,
                    expected_matches: None,
                    marker: None,
                    start_marker: None,
                    end_marker: None,
                    mode: None,
                    if_exists: Some(*if_exists),
                },
                Rule::Delete { .. } => PreparedOperation {
                    kind: FileOperationKind::Delete,
                    path: self.resolve_rule_path(effective_root, rule_path(rule))?,
                    relative_path: self.relative_rule_path(effective_root, rule_path(rule))?,
                    source_path: None,
                    source_relative_path: None,
                    target_path: None,
                    target_relative_path: None,
                    content: None,
                    replace: None,
                    replace_all: false,
                    expected_matches: None,
                    marker: None,
                    start_marker: None,
                    end_marker: None,
                    mode: None,
                    if_exists: None,
                },
                Rule::Rename { from, to } | Rule::Move { from, to } | Rule::Copy { from, to } => {
                    let from_path = self.resolve_rule_path(effective_root, from)?;
                    let to_path = self.resolve_rule_path(effective_root, to)?;
                    PreparedOperation {
                        kind: match rule {
                            Rule::Rename { .. } => FileOperationKind::Rename,
                            Rule::Move { .. } => FileOperationKind::Move,
                            Rule::Copy { .. } => FileOperationKind::Copy,
                            _ => unreachable!(),
                        },
                        path: to_path.clone(),
                        relative_path: self.sandbox.display_path(&to_path),
                        source_path: Some(from_path.clone()),
                        source_relative_path: Some(self.sandbox.display_path(&from_path)),
                        target_path: Some(to_path.clone()),
                        target_relative_path: Some(self.sandbox.display_path(&to_path)),
                        content: None,
                        replace: None,
                        replace_all: false,
                        expected_matches: None,
                        marker: None,
                        start_marker: None,
                        end_marker: None,
                        mode: None,
                        if_exists: None,
                    }
                }
                Rule::Mkdir { .. } => PreparedOperation {
                    kind: FileOperationKind::Mkdir,
                    path: self.resolve_rule_path(effective_root, rule_path(rule))?,
                    relative_path: self.relative_rule_path(effective_root, rule_path(rule))?,
                    source_path: None,
                    source_relative_path: None,
                    target_path: None,
                    target_relative_path: None,
                    content: None,
                    replace: None,
                    replace_all: false,
                    expected_matches: None,
                    marker: None,
                    start_marker: None,
                    end_marker: None,
                    mode: None,
                    if_exists: None,
                },
                Rule::Chmod { mode, .. } => PreparedOperation {
                    kind: FileOperationKind::Chmod,
                    path: self.resolve_rule_path(effective_root, rule_path(rule))?,
                    relative_path: self.relative_rule_path(effective_root, rule_path(rule))?,
                    source_path: None,
                    source_relative_path: None,
                    target_path: None,
                    target_relative_path: None,
                    content: None,
                    replace: None,
                    replace_all: false,
                    expected_matches: None,
                    marker: None,
                    start_marker: None,
                    end_marker: None,
                    mode: Some(
                        parse_mode(mode).map_err(|message| CoreError::InvalidConfig {
                            label: "inline config".to_string(),
                            message,
                        })?,
                    ),
                    if_exists: None,
                },
                Rule::Append { content, .. } => PreparedOperation {
                    kind: FileOperationKind::Append,
                    path: self.resolve_rule_path(effective_root, rule_path(rule))?,
                    relative_path: self.relative_rule_path(effective_root, rule_path(rule))?,
                    source_path: None,
                    source_relative_path: None,
                    target_path: None,
                    target_relative_path: None,
                    content: Some(content.clone()),
                    replace: None,
                    replace_all: false,
                    expected_matches: None,
                    marker: None,
                    start_marker: None,
                    end_marker: None,
                    mode: None,
                    if_exists: None,
                },
                Rule::AppendOnce { content, .. } => PreparedOperation {
                    kind: FileOperationKind::AppendOnce,
                    path: self.resolve_rule_path(effective_root, rule_path(rule))?,
                    relative_path: self.relative_rule_path(effective_root, rule_path(rule))?,
                    source_path: None,
                    source_relative_path: None,
                    target_path: None,
                    target_relative_path: None,
                    content: Some(content.clone()),
                    replace: None,
                    replace_all: false,
                    expected_matches: None,
                    marker: None,
                    start_marker: None,
                    end_marker: None,
                    mode: None,
                    if_exists: None,
                },
                Rule::Prepend { content, .. } => PreparedOperation {
                    kind: FileOperationKind::Prepend,
                    path: self.resolve_rule_path(effective_root, rule_path(rule))?,
                    relative_path: self.relative_rule_path(effective_root, rule_path(rule))?,
                    source_path: None,
                    source_relative_path: None,
                    target_path: None,
                    target_relative_path: None,
                    content: Some(content.clone()),
                    replace: None,
                    replace_all: false,
                    expected_matches: None,
                    marker: None,
                    start_marker: None,
                    end_marker: None,
                    mode: None,
                    if_exists: None,
                },
                Rule::InsertBefore {
                    marker, content, ..
                } => PreparedOperation {
                    kind: FileOperationKind::InsertBefore,
                    path: self.resolve_rule_path(effective_root, rule_path(rule))?,
                    relative_path: self.relative_rule_path(effective_root, rule_path(rule))?,
                    source_path: None,
                    source_relative_path: None,
                    target_path: None,
                    target_relative_path: None,
                    content: Some(content.clone()),
                    replace: None,
                    replace_all: false,
                    expected_matches: None,
                    marker: Some(marker.clone()),
                    start_marker: None,
                    end_marker: None,
                    mode: None,
                    if_exists: None,
                },
                Rule::InsertAfter {
                    marker, content, ..
                } => PreparedOperation {
                    kind: FileOperationKind::InsertAfter,
                    path: self.resolve_rule_path(effective_root, rule_path(rule))?,
                    relative_path: self.relative_rule_path(effective_root, rule_path(rule))?,
                    source_path: None,
                    source_relative_path: None,
                    target_path: None,
                    target_relative_path: None,
                    content: Some(content.clone()),
                    replace: None,
                    replace_all: false,
                    expected_matches: None,
                    marker: Some(marker.clone()),
                    start_marker: None,
                    end_marker: None,
                    mode: None,
                    if_exists: None,
                },
                Rule::Replace {
                    replace,
                    content,
                    replace_all,
                    expected_matches,
                    ..
                } => PreparedOperation {
                    kind: FileOperationKind::Replace,
                    path: self.resolve_rule_path(effective_root, rule_path(rule))?,
                    relative_path: self.relative_rule_path(effective_root, rule_path(rule))?,
                    source_path: None,
                    source_relative_path: None,
                    target_path: None,
                    target_relative_path: None,
                    content: Some(content.clone()),
                    replace: Some(replace.clone()),
                    replace_all: *replace_all,
                    expected_matches: *expected_matches,
                    marker: None,
                    start_marker: None,
                    end_marker: None,
                    mode: None,
                    if_exists: None,
                },
                Rule::ReplaceOrAppend {
                    replace,
                    content,
                    replace_all,
                    expected_matches,
                    ..
                } => PreparedOperation {
                    kind: FileOperationKind::ReplaceOrAppend,
                    path: self.resolve_rule_path(effective_root, rule_path(rule))?,
                    relative_path: self.relative_rule_path(effective_root, rule_path(rule))?,
                    source_path: None,
                    source_relative_path: None,
                    target_path: None,
                    target_relative_path: None,
                    content: Some(content.clone()),
                    replace: Some(replace.clone()),
                    replace_all: *replace_all,
                    expected_matches: *expected_matches,
                    marker: None,
                    start_marker: None,
                    end_marker: None,
                    mode: None,
                    if_exists: None,
                },
                Rule::ManagedBlock {
                    start_marker,
                    end_marker,
                    content,
                    ..
                } => PreparedOperation {
                    kind: FileOperationKind::ManagedBlock,
                    path: self.resolve_rule_path(effective_root, rule_path(rule))?,
                    relative_path: self.relative_rule_path(effective_root, rule_path(rule))?,
                    source_path: None,
                    source_relative_path: None,
                    target_path: None,
                    target_relative_path: None,
                    content: Some(content.clone()),
                    replace: None,
                    replace_all: false,
                    expected_matches: None,
                    marker: None,
                    start_marker: Some(start_marker.clone()),
                    end_marker: Some(end_marker.clone()),
                    mode: None,
                    if_exists: None,
                },
            };
            operations.push(operation);
        }
        Ok(operations)
    }

    fn resolve_rule_path(&self, effective_root: &Path, raw: &str) -> Result<PathBuf, CoreError> {
        self.sandbox.resolve_generated_path(effective_root, raw)
    }

    fn relative_rule_path(&self, effective_root: &Path, raw: &str) -> Result<String, CoreError> {
        let path = self.resolve_rule_path(effective_root, raw)?;
        Ok(self.sandbox.display_path(&path))
    }

    fn simulate(&self, prepared: &PreparedGeneration) -> Result<Simulation, CoreError> {
        let mut files: BTreeMap<PathBuf, SimulatedFile> = BTreeMap::new();
        let mut warnings = Vec::new();
        let mut errors = Vec::new();
        let mut metadata_changes = Vec::new();

        for operation in &prepared.operations {
            operation.simulate(
                &mut files,
                &mut metadata_changes,
                &mut warnings,
                &mut errors,
            )?;
        }

        Ok(Simulation {
            files,
            metadata_changes,
            warnings,
            errors,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct ConfigReference<'a> {
    value: &'a JsonValue,
}

#[derive(Debug, Clone)]
struct ConfigSource {
    config: Config,
}

#[derive(Debug, Clone)]
pub struct PathSandbox {
    root: PathBuf,
}

impl PathSandbox {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, CoreError> {
        let root_ref = root.as_ref();
        let root = root_ref
            .canonicalize()
            .map_err(|source| CoreError::InvalidRoot {
                path: path_to_string(root_ref),
                source,
            })?;
        if !root.is_dir() {
            return Err(CoreError::NotADirectory {
                path: path_to_string(&root),
            });
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve_existing_file(&self, raw: &str) -> Result<PathBuf, CoreError> {
        let path = self.resolve_existing(raw)?;
        if !path.is_file() {
            return Err(CoreError::NotAFile {
                path: self.display_path(&path),
            });
        }
        Ok(path)
    }

    pub fn resolve_existing_dir(&self, raw: &str) -> Result<PathBuf, CoreError> {
        let path = self.resolve_existing(raw)?;
        if !path.is_dir() {
            return Err(CoreError::NotADirectory {
                path: self.display_path(&path),
            });
        }
        Ok(path)
    }

    pub fn resolve_generated_path(
        &self,
        effective_root: &Path,
        raw: &str,
    ) -> Result<PathBuf, CoreError> {
        if raw.trim().is_empty() {
            return Err(CoreError::InvalidPath {
                path: raw.to_string(),
            });
        }

        let raw_path = Path::new(raw);
        let candidate = if raw_path.is_absolute() {
            raw_path.to_path_buf()
        } else {
            effective_root.join(raw_path)
        };
        self.resolve_for_write(&candidate)
    }

    pub fn display_path(&self, path: &Path) -> String {
        let path = normalize_path(path).unwrap_or_else(|_| path.to_path_buf());
        let relative = path.strip_prefix(&self.root).unwrap_or(&path);
        if relative.as_os_str().is_empty() {
            return ".".to_string();
        }
        relative.to_string_lossy().replace('\\', "/")
    }

    fn resolve_existing(&self, raw: &str) -> Result<PathBuf, CoreError> {
        if raw.trim().is_empty() {
            return Err(CoreError::InvalidPath {
                path: raw.to_string(),
            });
        }
        let raw_path = Path::new(raw);
        let candidate = if raw_path.is_absolute() {
            raw_path.to_path_buf()
        } else {
            self.root.join(raw_path)
        };
        let normalized = normalize_path(&candidate)?;
        self.ensure_lexically_inside_root(&normalized)?;
        let canonical = normalized
            .canonicalize()
            .map_err(|source| CoreError::ReadFile {
                path: path_to_string(&normalized),
                source,
            })?;
        self.ensure_lexically_inside_root(&canonical)?;
        Ok(canonical)
    }

    fn resolve_for_write(&self, candidate: &Path) -> Result<PathBuf, CoreError> {
        let normalized = normalize_path(candidate)?;
        self.ensure_lexically_inside_root(&normalized)?;

        if normalized.exists() {
            let canonical = normalized
                .canonicalize()
                .map_err(|source| CoreError::ReadFile {
                    path: path_to_string(&normalized),
                    source,
                })?;
            self.ensure_lexically_inside_root(&canonical)?;
            return Ok(normalized);
        }

        let mut ancestor = normalized.parent();
        while let Some(path) = ancestor {
            if path.exists() {
                let canonical = path.canonicalize().map_err(|source| CoreError::ReadFile {
                    path: path_to_string(path),
                    source,
                })?;
                self.ensure_lexically_inside_root(&canonical)?;
                return Ok(normalized);
            }
            ancestor = path.parent();
        }

        Err(CoreError::PathOutsideRoot {
            path: path_to_string(&normalized),
        })
    }

    fn ensure_lexically_inside_root(&self, path: &Path) -> Result<(), CoreError> {
        if path.starts_with(&self.root) {
            Ok(())
        } else {
            Err(CoreError::PathOutsideRoot {
                path: path_to_string(path),
            })
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct GenerationRequest {
    #[serde(default)]
    pub config: Option<JsonValue>,
    #[serde(default)]
    pub root: Option<String>,
}

impl GenerationRequest {
    fn config_reference(&self) -> Result<ConfigReference<'_>, CoreError> {
        self.config
            .as_ref()
            .map(|value| ConfigReference { value })
            .ok_or(CoreError::MissingConfig)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ApplyRequest {
    #[serde(default)]
    pub config: Option<JsonValue>,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub confirm_token: Option<String>,
    #[serde(default)]
    pub explicit_approval: bool,
}

impl ApplyRequest {
    fn is_approved(&self) -> bool {
        self.explicit_approval || self.confirm_token.as_deref() == Some("apply")
    }

    fn generation_request(&self) -> GenerationRequest {
        GenerationRequest {
            config: self.config.clone(),
            root: self.root.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ValidateConfigRequest {
    #[serde(default)]
    pub config: Option<JsonValue>,
}

impl ValidateConfigRequest {
    fn config_reference(&self) -> Result<ConfigReference<'_>, CoreError> {
        self.config
            .as_ref()
            .map(|value| ConfigReference { value })
            .ok_or(CoreError::MissingConfig)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanOutput {
    pub operations: Vec<PlannedFileOperation>,
    pub affected_paths: Vec<String>,
    pub warnings: Vec<Diagnostic>,
    pub errors: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffOutput {
    pub diff: String,
    pub summary: ChangeSummary,
    pub warnings: Vec<Diagnostic>,
    pub errors: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyOutput {
    pub changed_files: Vec<String>,
    pub summary: String,
    pub warnings: Vec<Diagnostic>,
    pub errors: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidateConfigOutput {
    pub valid: bool,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<ConfigHint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigHint {
    pub summary: String,
    pub minimal_config: JsonValue,
    pub examples: Vec<ConfigExample>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigExample {
    pub operation: FileOperationKind,
    pub config: JsonValue,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListTemplatesOutput {
    pub items: Vec<TemplateInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TemplateInfo {
    pub name: String,
    pub path: String,
    pub kind: TemplateKind,
    pub metadata: JsonValue,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateKind {
    Config,
    Template,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedFileOperation {
    pub operation: FileOperationKind,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_path: Option<String>,
    pub exists: bool,
    pub will_create: bool,
    pub will_modify: bool,
    pub will_delete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOperationKind {
    Write,
    Delete,
    Rename,
    Move,
    Copy,
    Mkdir,
    Chmod,
    Append,
    AppendOnce,
    Prepend,
    InsertBefore,
    InsertAfter,
    Replace,
    ReplaceOrAppend,
    ManagedBlock,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl Diagnostic {
    fn warning(
        code: impl Into<String>,
        message: impl Into<String>,
        path: Option<impl Into<String>>,
    ) -> Self {
        Self {
            severity: DiagnosticSeverity::Warning,
            code: code.into(),
            message: message.into(),
            path: path.map(Into::into),
        }
    }

    fn error(
        code: impl Into<String>,
        message: impl Into<String>,
        path: Option<impl Into<String>>,
    ) -> Self {
        Self {
            severity: DiagnosticSeverity::Error,
            code: code.into(),
            message: message.into(),
            path: path.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangeSummary {
    pub files_changed: usize,
    pub changed_files: Vec<String>,
    pub additions: usize,
    pub deletions: usize,
}

impl ChangeSummary {
    fn empty() -> Self {
        Self {
            files_changed: 0,
            changed_files: Vec::new(),
            additions: 0,
            deletions: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct PreparedGeneration {
    operations: Vec<PreparedOperation>,
}

impl PreparedGeneration {
    fn plan_output(&self) -> PlanOutput {
        let mut affected_paths = BTreeSet::new();
        let mut operations = Vec::with_capacity(self.operations.len());
        let mut warnings = Vec::new();
        let mut errors = Vec::new();

        for operation in &self.operations {
            let exists = operation.path.exists();
            if operation.kind == FileOperationKind::Write
                && operation.if_exists == Some(IfExists::Error)
                && exists
            {
                errors.push(Diagnostic::error(
                    "file_exists",
                    "write rule cannot write an existing file when if_exists is error",
                    Some(operation.relative_path.clone()),
                ));
            }
            if operation.kind == FileOperationKind::Copy && exists {
                errors.push(Diagnostic::error(
                    "file_exists",
                    "operation cannot create a target that already exists",
                    Some(operation.relative_path.clone()),
                ));
            }
            if operation.kind == FileOperationKind::Replace && !exists {
                warnings.push(Diagnostic::warning(
                    "missing_replace_target",
                    "replace rule target does not exist and will be created if applied",
                    Some(operation.relative_path.clone()),
                ));
            }

            affected_paths.insert(operation.relative_path.clone());
            if let Some(source) = &operation.source_relative_path {
                affected_paths.insert(source.clone());
            }
            if let Some(target) = &operation.target_relative_path {
                affected_paths.insert(target.clone());
            }
            operations.push(PlannedFileOperation {
                operation: operation.kind,
                path: operation.relative_path.clone(),
                source_path: operation.source_relative_path.clone(),
                target_path: operation.target_relative_path.clone(),
                exists,
                will_create: operation.will_create(exists),
                will_modify: operation.will_modify(exists),
                will_delete: matches!(
                    operation.kind,
                    FileOperationKind::Delete | FileOperationKind::Rename | FileOperationKind::Move
                ),
            });
        }

        PlanOutput {
            operations,
            affected_paths: affected_paths.into_iter().collect(),
            warnings,
            errors,
        }
    }
}

#[derive(Debug, Clone)]
struct PreparedOperation {
    kind: FileOperationKind,
    path: PathBuf,
    relative_path: String,
    source_path: Option<PathBuf>,
    source_relative_path: Option<String>,
    target_path: Option<PathBuf>,
    target_relative_path: Option<String>,
    content: Option<String>,
    replace: Option<regex::Regex>,
    replace_all: bool,
    expected_matches: Option<usize>,
    marker: Option<String>,
    start_marker: Option<String>,
    end_marker: Option<String>,
    mode: Option<u32>,
    if_exists: Option<IfExists>,
}

impl PreparedOperation {
    fn will_create(&self, exists: bool) -> bool {
        if self.kind == FileOperationKind::Write {
            return !exists;
        }
        matches!(
            self.kind,
            FileOperationKind::Copy
                | FileOperationKind::Rename
                | FileOperationKind::Move
                | FileOperationKind::Mkdir
        ) && !exists
    }

    fn will_modify(&self, exists: bool) -> bool {
        if self.kind == FileOperationKind::Write {
            return exists && self.if_exists == Some(IfExists::Overwrite);
        }
        matches!(
            self.kind,
            FileOperationKind::Append
                | FileOperationKind::AppendOnce
                | FileOperationKind::Prepend
                | FileOperationKind::InsertBefore
                | FileOperationKind::InsertAfter
                | FileOperationKind::Replace
                | FileOperationKind::ReplaceOrAppend
                | FileOperationKind::ManagedBlock
                | FileOperationKind::Chmod
        ) && exists
    }

    fn simulate(
        &self,
        files: &mut BTreeMap<PathBuf, SimulatedFile>,
        metadata_changes: &mut Vec<MetadataChange>,
        warnings: &mut Vec<Diagnostic>,
        errors: &mut Vec<Diagnostic>,
    ) -> Result<(), CoreError> {
        match self.kind {
            FileOperationKind::Write => {
                let file = simulated_file(files, &self.path, &self.relative_path)?;
                let if_exists = self.if_exists()?;
                if file.existed && !file.deleted {
                    match if_exists {
                        IfExists::Error => {
                            errors.push(Diagnostic::error(
                                "file_exists",
                                "write rule cannot write an existing file when if_exists is error",
                                Some(file.relative_path.clone()),
                            ));
                            return Ok(());
                        }
                        IfExists::Skip => return Ok(()),
                        IfExists::Overwrite => {}
                    }
                }
                file.current = format!("{}\n", self.content()?.trim_end());
                file.deleted = false;
                file.existed = true;
            }
            FileOperationKind::Delete => {
                let file = simulated_file(files, &self.path, &self.relative_path)?;
                if !file.existed {
                    warnings.push(Diagnostic::warning(
                        "missing_delete_target",
                        "delete target does not exist",
                        Some(file.relative_path.clone()),
                    ));
                }
                file.current.clear();
                file.deleted = true;
            }
            FileOperationKind::Rename | FileOperationKind::Move => {
                let source_path = self.source_path()?;
                let source_relative_path = self.source_relative_path()?;
                let target_path = self.target_path()?;
                let target_relative_path = self.target_relative_path()?;
                let source_content = {
                    let source = simulated_file(files, source_path, source_relative_path)?;
                    if !source.existed || source.deleted {
                        errors.push(Diagnostic::error(
                            "missing_source",
                            "move source does not exist",
                            Some(source.relative_path.clone()),
                        ));
                        return Ok(());
                    }
                    source.current.clone()
                };
                {
                    let target = simulated_file(files, target_path, target_relative_path)?;
                    if target.existed && !target.deleted {
                        errors.push(Diagnostic::error(
                            "target_exists",
                            "move target already exists",
                            Some(target.relative_path.clone()),
                        ));
                        return Ok(());
                    }
                    target.current = source_content;
                    target.deleted = false;
                    target.existed = true;
                }
                let source = simulated_file(files, source_path, source_relative_path)?;
                source.current.clear();
                source.deleted = true;
            }
            FileOperationKind::Copy => {
                let source_path = self.source_path()?;
                let source_relative_path = self.source_relative_path()?;
                let target_path = self.target_path()?;
                let target_relative_path = self.target_relative_path()?;
                let source_content = {
                    let source = simulated_file(files, source_path, source_relative_path)?;
                    if !source.existed || source.deleted {
                        errors.push(Diagnostic::error(
                            "missing_source",
                            "copy source does not exist",
                            Some(source.relative_path.clone()),
                        ));
                        return Ok(());
                    }
                    source.current.clone()
                };
                let target = simulated_file(files, target_path, target_relative_path)?;
                if target.existed && !target.deleted {
                    errors.push(Diagnostic::error(
                        "target_exists",
                        "copy target already exists",
                        Some(target.relative_path.clone()),
                    ));
                    return Ok(());
                }
                target.current = source_content;
                target.deleted = false;
                target.existed = true;
            }
            FileOperationKind::Mkdir => {
                metadata_changes.push(MetadataChange {
                    kind: MetadataChangeKind::Mkdir,
                    path: self.path.clone(),
                    relative_path: self.relative_path.clone(),
                    mode: None,
                });
            }
            FileOperationKind::Chmod => {
                metadata_changes.push(MetadataChange {
                    kind: MetadataChangeKind::Chmod,
                    path: self.path.clone(),
                    relative_path: self.relative_path.clone(),
                    mode: self.mode,
                });
            }
            FileOperationKind::Append => {
                let file = simulated_file(files, &self.path, &self.relative_path)?;
                file.current
                    .push_str(&format!("{}\n", self.content()?.trim_end()));
                file.deleted = false;
                file.existed = true;
            }
            FileOperationKind::AppendOnce => {
                let file = simulated_file(files, &self.path, &self.relative_path)?;
                let content = self.content()?.trim_end();
                if !file.current.contains(content) {
                    file.current.push_str(&format!("{content}\n"));
                    file.deleted = false;
                    file.existed = true;
                }
            }
            FileOperationKind::Prepend => {
                let file = simulated_file(files, &self.path, &self.relative_path)?;
                file.current = format!("{}\n{}\n", self.content()?, file.current.trim_end());
                file.deleted = false;
                file.existed = true;
            }
            FileOperationKind::InsertBefore => {
                let file = simulated_file(files, &self.path, &self.relative_path)?;
                let Some(updated) = insert_before(&file.current, self.marker()?, self.content()?)
                else {
                    errors.push(Diagnostic::error(
                        "missing_marker",
                        "insert_before marker was not found",
                        Some(file.relative_path.clone()),
                    ));
                    return Ok(());
                };
                file.current = updated;
                file.deleted = false;
                file.existed = true;
            }
            FileOperationKind::InsertAfter => {
                let file = simulated_file(files, &self.path, &self.relative_path)?;
                let Some(updated) = insert_after(&file.current, self.marker()?, self.content()?)
                else {
                    errors.push(Diagnostic::error(
                        "missing_marker",
                        "insert_after marker was not found",
                        Some(file.relative_path.clone()),
                    ));
                    return Ok(());
                };
                file.current = updated;
                file.deleted = false;
                file.existed = true;
            }
            FileOperationKind::Replace => {
                let file = simulated_file(files, &self.path, &self.relative_path)?;
                if !file.existed {
                    warnings.push(Diagnostic::warning(
                        "missing_replace_target",
                        "replace rule target does not exist and will be created if applied",
                        Some(file.relative_path.clone()),
                    ));
                }
                let Some(replace) = &self.replace else {
                    errors.push(Diagnostic::error(
                        "missing_replace_regex",
                        "replace rule is missing a regex",
                        Some(file.relative_path.clone()),
                    ));
                    return Ok(());
                };
                let replaced = match replace_content(
                    &file.current,
                    replace,
                    self.content()?,
                    self.replace_all,
                    self.expected_matches,
                ) {
                    Ok(replaced) => replaced.into_owned(),
                    Err(message) => {
                        errors.push(Diagnostic::error(
                            "replace_match_count",
                            message,
                            Some(file.relative_path.clone()),
                        ));
                        return Ok(());
                    }
                };
                file.current = format!("{}\n", replaced.trim_end());
                file.deleted = false;
                file.existed = true;
            }
            FileOperationKind::ReplaceOrAppend => {
                let file = simulated_file(files, &self.path, &self.relative_path)?;
                let Some(replace) = &self.replace else {
                    errors.push(Diagnostic::error(
                        "missing_replace_regex",
                        "replace_or_append rule is missing a regex",
                        Some(file.relative_path.clone()),
                    ));
                    return Ok(());
                };
                if replace.is_match(&file.current) {
                    let replaced = match replace_content(
                        &file.current,
                        replace,
                        self.content()?,
                        self.replace_all,
                        self.expected_matches,
                    ) {
                        Ok(replaced) => replaced.into_owned(),
                        Err(message) => {
                            errors.push(Diagnostic::error(
                                "replace_match_count",
                                message,
                                Some(file.relative_path.clone()),
                            ));
                            return Ok(());
                        }
                    };
                    file.current = format!("{}\n", replaced.trim_end());
                } else {
                    file.current
                        .push_str(&format!("{}\n", self.content()?.trim_end()));
                }
                file.deleted = false;
                file.existed = true;
            }
            FileOperationKind::ManagedBlock => {
                let file = simulated_file(files, &self.path, &self.relative_path)?;
                let updated = match upsert_managed_block(
                    &file.current,
                    self.start_marker()?,
                    self.end_marker()?,
                    self.content()?,
                ) {
                    Ok(updated) => updated,
                    Err(message) => {
                        errors.push(Diagnostic::error(
                            "invalid_managed_block",
                            message,
                            Some(file.relative_path.clone()),
                        ));
                        return Ok(());
                    }
                };
                file.current = format!("{}\n", updated.trim_end());
                file.deleted = false;
                file.existed = true;
            }
        }
        Ok(())
    }

    fn content(&self) -> Result<&str, CoreError> {
        self.content
            .as_deref()
            .ok_or_else(|| CoreError::InvalidConfig {
                label: "inline config".to_string(),
                message: "operation is missing content".to_string(),
            })
    }

    fn if_exists(&self) -> Result<IfExists, CoreError> {
        self.if_exists.ok_or_else(|| CoreError::InvalidConfig {
            label: "inline config".to_string(),
            message: "write operation is missing if_exists".to_string(),
        })
    }

    fn marker(&self) -> Result<&str, CoreError> {
        self.marker
            .as_deref()
            .ok_or_else(|| CoreError::InvalidConfig {
                label: "inline config".to_string(),
                message: "operation is missing marker".to_string(),
            })
    }

    fn start_marker(&self) -> Result<&str, CoreError> {
        self.start_marker
            .as_deref()
            .ok_or_else(|| CoreError::InvalidConfig {
                label: "inline config".to_string(),
                message: "operation is missing start_marker".to_string(),
            })
    }

    fn end_marker(&self) -> Result<&str, CoreError> {
        self.end_marker
            .as_deref()
            .ok_or_else(|| CoreError::InvalidConfig {
                label: "inline config".to_string(),
                message: "operation is missing end_marker".to_string(),
            })
    }

    fn source_path(&self) -> Result<&Path, CoreError> {
        self.source_path
            .as_deref()
            .ok_or_else(|| CoreError::InvalidConfig {
                label: "inline config".to_string(),
                message: "operation is missing source path".to_string(),
            })
    }

    fn source_relative_path(&self) -> Result<&str, CoreError> {
        self.source_relative_path
            .as_deref()
            .ok_or_else(|| CoreError::InvalidConfig {
                label: "inline config".to_string(),
                message: "operation is missing source path".to_string(),
            })
    }

    fn target_path(&self) -> Result<&Path, CoreError> {
        self.target_path
            .as_deref()
            .ok_or_else(|| CoreError::InvalidConfig {
                label: "inline config".to_string(),
                message: "operation is missing target path".to_string(),
            })
    }

    fn target_relative_path(&self) -> Result<&str, CoreError> {
        self.target_relative_path
            .as_deref()
            .ok_or_else(|| CoreError::InvalidConfig {
                label: "inline config".to_string(),
                message: "operation is missing target path".to_string(),
            })
    }
}

#[derive(Debug, Clone)]
struct SimulatedFile {
    path: PathBuf,
    relative_path: String,
    original: String,
    current: String,
    existed: bool,
    deleted: bool,
}

#[derive(Debug, Clone)]
struct Simulation {
    files: BTreeMap<PathBuf, SimulatedFile>,
    metadata_changes: Vec<MetadataChange>,
    warnings: Vec<Diagnostic>,
    errors: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
struct MetadataChange {
    kind: MetadataChangeKind,
    path: PathBuf,
    relative_path: String,
    mode: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
enum MetadataChangeKind {
    Mkdir,
    Chmod,
}

impl Simulation {
    fn changed_files(&self) -> Vec<&SimulatedFile> {
        self.files
            .values()
            .filter(|file| file.original != file.current || (file.existed && file.deleted))
            .collect()
    }

    fn changed_relative_paths(&self) -> Vec<String> {
        let mut paths = self
            .changed_files()
            .into_iter()
            .map(|file| file.relative_path.clone())
            .collect::<Vec<_>>();
        paths.extend(
            self.metadata_changes
                .iter()
                .map(|change| change.relative_path.clone()),
        );
        paths.sort();
        paths.dedup();
        paths
    }

    fn diff_output(&self, mut plan_warnings: Vec<Diagnostic>) -> DiffOutput {
        plan_warnings.extend(self.warnings.clone());
        if !self.errors.is_empty() {
            return DiffOutput {
                diff: String::new(),
                summary: ChangeSummary::empty(),
                warnings: plan_warnings,
                errors: self.errors.clone(),
            };
        }

        let mut diff = String::new();
        let mut additions = 0;
        let mut deletions = 0;
        let changed_files = self.changed_files();
        let changed_paths = changed_files
            .iter()
            .map(|file| file.relative_path.clone())
            .collect::<Vec<_>>();

        for file in changed_files {
            let text_diff = TextDiff::from_lines(&file.original, &file.current);
            additions += text_diff
                .iter_all_changes()
                .filter(|change| change.tag() == ChangeTag::Insert)
                .count();
            deletions += text_diff
                .iter_all_changes()
                .filter(|change| change.tag() == ChangeTag::Delete)
                .count();
            diff.push_str(
                &text_diff
                    .unified_diff()
                    .header(
                        &format!("a/{}", file.relative_path),
                        &format!("b/{}", file.relative_path),
                    )
                    .to_string(),
            );
        }

        DiffOutput {
            diff,
            summary: ChangeSummary {
                files_changed: changed_paths.len(),
                changed_files: changed_paths,
                additions,
                deletions,
            },
            warnings: plan_warnings,
            errors: Vec::new(),
        }
    }
}

fn rule_path(rule: &Rule) -> &str {
    match rule {
        Rule::Write { path, .. }
        | Rule::Delete { path }
        | Rule::Mkdir { path }
        | Rule::Chmod { path, .. }
        | Rule::Append { path, .. }
        | Rule::AppendOnce { path, .. }
        | Rule::Prepend { path, .. }
        | Rule::InsertBefore { path, .. }
        | Rule::InsertAfter { path, .. }
        | Rule::Replace { path, .. }
        | Rule::ReplaceOrAppend { path, .. }
        | Rule::ManagedBlock { path, .. } => path,
        Rule::Rename { to, .. } | Rule::Move { to, .. } | Rule::Copy { to, .. } => to,
    }
}

fn simulated_file<'a>(
    files: &'a mut BTreeMap<PathBuf, SimulatedFile>,
    path: &Path,
    relative_path: &str,
) -> Result<&'a mut SimulatedFile, CoreError> {
    if !files.contains_key(path) {
        let original = read_optional_string(path)?;
        let current = original.clone().unwrap_or_default();
        files.insert(
            path.to_path_buf(),
            SimulatedFile {
                path: path.to_path_buf(),
                relative_path: relative_path.to_string(),
                original: current.clone(),
                current,
                existed: original.is_some(),
                deleted: false,
            },
        );
    }
    files.get_mut(path).ok_or_else(|| CoreError::InvalidConfig {
        label: "simulation".to_string(),
        message: "failed to load simulated file".to_string(),
    })
}

fn replace_content<'a>(
    file_content: &'a str,
    replace: &regex::Regex,
    content: &str,
    replace_all: bool,
    expected_matches: Option<usize>,
) -> Result<Cow<'a, str>, String> {
    let matches = replace.find_iter(file_content).count();
    let expected = expected_matches.unwrap_or(1);
    if !replace_all && matches != expected {
        return Err(format!("expected {expected} matches, found {matches}"));
    }

    if replace_all {
        Ok(replace.replace_all(file_content, content))
    } else {
        Ok(replace.replacen(file_content, 1, content))
    }
}

fn insert_before(file_content: &str, marker: &str, content: &str) -> Option<String> {
    let index = file_content.find(marker)?;
    let mut updated = String::with_capacity(file_content.len() + content.len() + 1);
    updated.push_str(&file_content[..index]);
    updated.push_str(content.trim_end());
    updated.push('\n');
    updated.push_str(&file_content[index..]);
    Some(updated)
}

fn insert_after(file_content: &str, marker: &str, content: &str) -> Option<String> {
    let index = file_content.find(marker)? + marker.len();
    let mut updated = String::with_capacity(file_content.len() + content.len() + 1);
    updated.push_str(&file_content[..index]);
    updated.push('\n');
    updated.push_str(content.trim_end());
    updated.push_str(&file_content[index..]);
    Some(updated)
}

fn upsert_managed_block(
    file_content: &str,
    start_marker: &str,
    end_marker: &str,
    content: &str,
) -> Result<String, String> {
    let block = format!("{start_marker}\n{}\n{end_marker}", content.trim_end());
    let start_count = file_content.matches(start_marker).count();
    let end_count = file_content.matches(end_marker).count();
    if start_count > 1 || end_count > 1 {
        return Err("managed block markers must be unique".to_string());
    }

    match (
        file_content.find(start_marker),
        file_content.find(end_marker),
    ) {
        (Some(start), Some(end)) if start <= end => {
            let end_index = end + end_marker.len();
            Ok(format!(
                "{}{}{}",
                &file_content[..start],
                block,
                &file_content[end_index..]
            ))
        }
        (None, None) => Ok(format!("{}{}\n", file_content, block)),
        _ => Err("managed block requires both start_marker and end_marker".to_string()),
    }
}

fn parse_mode(mode: &str) -> Result<u32, String> {
    u32::from_str_radix(mode.trim_start_matches("0o"), 8)
        .map_err(|err| format!("invalid chmod mode `{mode}`: {err}"))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), CoreError> {
    use std::os::unix::fs::PermissionsExt;

    let permissions = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, permissions).map_err(|source| CoreError::WriteFile {
        path: path_to_string(path),
        source,
    })
}

#[cfg(not(unix))]
fn set_mode(path: &Path, _mode: u32) -> Result<(), CoreError> {
    Err(CoreError::InvalidConfig {
        label: path_to_string(path),
        message: "chmod is only supported on Unix platforms".to_string(),
    })
}

fn parse_json_config(value: &JsonValue) -> Result<Config, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "config must be a JSON object".to_string())?;

    for key in object.keys() {
        if key != "props" && key != "rules" {
            return Err(format!("unsupported config field `{key}`"));
        }
    }

    let props = match object.get("props") {
        None | Some(JsonValue::Null) => Vec::new(),
        Some(JsonValue::Object(props)) => {
            let mut converted = Vec::with_capacity(props.len());
            for (key, value) in props {
                converted.push((
                    key.clone(),
                    Value::try_from(value.clone())
                        .map_err(|err| format!("invalid prop `{key}`: {err}"))?,
                ));
            }
            converted
        }
        Some(_) => return Err("config.props must be a JSON object when provided".to_string()),
    };

    let rules_value = object
        .get("rules")
        .cloned()
        .unwrap_or_else(|| JsonValue::Array(Vec::new()));
    if !rules_value.is_array() {
        return Err("config.rules must be a JSON array".to_string());
    }
    let rules = serde_json::from_value::<Vec<Rule>>(rules_value)
        .map_err(|err| format!("invalid config.rules: {err}"))?;

    Ok(Config { props, rules })
}

fn normalize_path(path: &Path) -> Result<PathBuf, CoreError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(CoreError::PathOutsideRoot {
                        path: path_to_string(path),
                    });
                }
            }
        }
    }
    Ok(normalized)
}

fn read_optional_string(path: &Path) -> Result<Option<String>, CoreError> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(Some(value)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(CoreError::ReadFile {
            path: path_to_string(path),
            source,
        }),
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn summarize_changed_files(count: usize) -> String {
    match count {
        0 => "No files changed.".to_string(),
        1 => "Changed 1 file.".to_string(),
        count => format!("Changed {count} files."),
    }
}

fn invalid_config_output(
    code: impl Into<String>,
    message: impl Into<String>,
    path: Option<impl Into<String>>,
) -> ValidateConfigOutput {
    ValidateConfigOutput {
        valid: false,
        diagnostics: vec![Diagnostic::error(code, message, path)],
        hint: Some(config_hint()),
    }
}

fn config_hint() -> ConfigHint {
    ConfigHint {
        summary: "Pass a JSON config object directly in the MCP tool arguments.".to_string(),
        minimal_config: json!({
            "rules": [
                {
                    "type": "replace",
                    "path": "src/application.rs",
                    "replace": "old text",
                    "content": "new text"
                }
            ]
        }),
        examples: vec![
            ConfigExample {
                operation: FileOperationKind::Replace,
                config: json!({
                    "rules": [
                        {
                            "type": "replace",
                            "path": "src/application.rs",
                            "replace": "old text",
                            "content": "new text"
                        }
                    ]
                }),
            },
            ConfigExample {
                operation: FileOperationKind::Append,
                config: json!({
                    "rules": [
                        {
                            "type": "append",
                            "path": "README.md",
                            "content": "..."
                        }
                    ]
                }),
            },
            ConfigExample {
                operation: FileOperationKind::AppendOnce,
                config: json!({
                    "rules": [
                        {
                            "type": "append_once",
                            "path": "README.md",
                            "content": "..."
                        }
                    ]
                }),
            },
            ConfigExample {
                operation: FileOperationKind::Prepend,
                config: json!({
                    "rules": [
                        {
                            "type": "prepend",
                            "path": "README.md",
                            "content": "..."
                        }
                    ]
                }),
            },
            ConfigExample {
                operation: FileOperationKind::InsertAfter,
                config: json!({
                    "rules": [
                        {
                            "type": "insert_after",
                            "path": "README.md",
                            "marker": "## Usage",
                            "content": "..."
                        }
                    ]
                }),
            },
            ConfigExample {
                operation: FileOperationKind::ManagedBlock,
                config: json!({
                    "rules": [
                        {
                            "type": "managed_block",
                            "path": "README.md",
                            "start_marker": "<!-- genify:start -->",
                            "end_marker": "<!-- genify:end -->",
                            "content": "..."
                        }
                    ]
                }),
            },
            ConfigExample {
                operation: FileOperationKind::Write,
                config: json!({
                    "rules": [
                        {
                            "type": "write",
                            "path": "new/file.rs",
                            "content": "...",
                            "if_exists": "error"
                        }
                    ]
                }),
            },
            ConfigExample {
                operation: FileOperationKind::Move,
                config: json!({
                    "rules": [
                        {
                            "type": "move",
                            "from": "old/file.rs",
                            "to": "new/file.rs"
                        }
                    ]
                }),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn rejects_path_traversal_outside_root() {
        let root = temp_root("path-traversal");

        let core = GenerationCore::new(&root).expect("root should be valid");
        let err = core
            .plan(GenerationRequest {
                config: Some(json!({
                    "rules": [
                        {
                            "type": "write",
                            "path": "../outside.txt",
                            "content": "nope",
                            "if_exists": "error"
                        }
                    ]
                })),
                ..GenerationRequest::default()
            })
            .expect_err("path traversal should be rejected");

        assert!(matches!(err, CoreError::PathOutsideRoot { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn diff_does_not_write_files() {
        let root = temp_root("dry-run");

        let core = GenerationCore::new(&root).expect("root should be valid");
        let output = core
            .diff(GenerationRequest {
                config: Some(json!({
                    "props": {
                        "name": "demo"
                    },
                    "rules": [
                        {
                            "type": "write",
                            "path": "out.txt",
                            "content": "Hello {{ name }}",
                            "if_exists": "error"
                        }
                    ]
                })),
                ..GenerationRequest::default()
            })
            .expect("diff should be generated");

        assert!(output.diff.contains("+Hello demo"));
        assert_eq!(output.summary.changed_files, vec!["out.txt"]);
        assert!(!root.join("out.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn apply_requires_explicit_approval() {
        let root = temp_root("apply-approval");

        let core = GenerationCore::new(&root).expect("root should be valid");
        let err = core
            .apply(ApplyRequest {
                config: Some(json!({
                    "rules": [
                        {
                            "type": "write",
                            "path": "out.txt",
                            "content": "Hello",
                            "if_exists": "error"
                        }
                    ]
                })),
                ..ApplyRequest::default()
            })
            .expect_err("apply should require approval");

        assert!(matches!(err, CoreError::ApprovalRequired));
        assert!(!root.join("out.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_if_exists_overwrite_replaces_existing_file() {
        let root = temp_root("write-overwrite");
        fs::write(root.join("out.txt"), "old\n").expect("test file should be written");
        let core = GenerationCore::new(&root).expect("root should be valid");

        let output = core
            .apply(ApplyRequest {
                config: Some(json!({
                    "rules": [
                        {
                            "type": "write",
                            "path": "out.txt",
                            "content": "new",
                            "if_exists": "overwrite"
                        }
                    ]
                })),
                explicit_approval: true,
                ..ApplyRequest::default()
            })
            .expect("write overwrite should apply");

        assert_eq!(output.changed_files, vec!["out.txt"]);
        assert_eq!(
            fs::read_to_string(root.join("out.txt")).expect("file should exist"),
            "new\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_if_exists_skip_keeps_existing_file() {
        let root = temp_root("write-skip");
        fs::write(root.join("out.txt"), "old\n").expect("test file should be written");
        let core = GenerationCore::new(&root).expect("root should be valid");

        let output = core
            .apply(ApplyRequest {
                config: Some(json!({
                    "rules": [
                        {
                            "type": "write",
                            "path": "out.txt",
                            "content": "new",
                            "if_exists": "skip"
                        }
                    ]
                })),
                explicit_approval: true,
                ..ApplyRequest::default()
            })
            .expect("write skip should apply");

        assert!(output.changed_files.is_empty());
        assert_eq!(
            fs::read_to_string(root.join("out.txt")).expect("file should exist"),
            "old\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_if_exists_error_reports_existing_file() {
        let root = temp_root("write-error");
        fs::write(root.join("out.txt"), "old\n").expect("test file should be written");
        let core = GenerationCore::new(&root).expect("root should be valid");

        let output = core
            .diff(GenerationRequest {
                config: Some(json!({
                    "rules": [
                        {
                            "type": "write",
                            "path": "out.txt",
                            "content": "new",
                            "if_exists": "error"
                        }
                    ]
                })),
                ..GenerationRequest::default()
            })
            .expect("write error should return structured output");

        assert_eq!(output.errors[0].code, "file_exists");
        assert_eq!(
            fs::read_to_string(root.join("out.txt")).expect("file should exist"),
            "old\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn validate_config_returns_hint_for_empty_config() {
        let root = temp_root("validate-hint");
        let core = GenerationCore::new(&root).expect("root should be valid");

        let output = core
            .validate_config(ValidateConfigRequest {
                config: Some(json!({})),
            })
            .expect("validation should return structured output");

        assert!(!output.valid);
        assert_eq!(output.diagnostics[0].code, "missing_rules");
        assert_eq!(
            output.hint.expect("hint should be returned").minimal_config["rules"][0]["type"],
            "replace"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn append_once_is_idempotent() {
        let root = temp_root("append-once");
        let core = GenerationCore::new(&root).expect("root should be valid");
        let config = json!({
            "rules": [
                {
                    "type": "append_once",
                    "path": "README.md",
                    "content": "managed line"
                }
            ]
        });

        let first = core
            .apply(ApplyRequest {
                config: Some(config.clone()),
                explicit_approval: true,
                ..ApplyRequest::default()
            })
            .expect("first apply should succeed");
        let second = core
            .apply(ApplyRequest {
                config: Some(config),
                explicit_approval: true,
                ..ApplyRequest::default()
            })
            .expect("second apply should succeed");

        assert_eq!(first.changed_files, vec!["README.md"]);
        assert!(second.changed_files.is_empty());
        assert_eq!(
            fs::read_to_string(root.join("README.md")).expect("file should exist"),
            "managed line\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn replace_expected_matches_reports_error() {
        let root = temp_root("replace-expected");
        fs::write(root.join("app.rs"), "old\nold\n").expect("test file should be written");
        let core = GenerationCore::new(&root).expect("root should be valid");

        let output = core
            .diff(GenerationRequest {
                config: Some(json!({
                    "rules": [
                        {
                            "type": "replace",
                            "path": "app.rs",
                            "replace": "old",
                            "content": "new",
                            "expected_matches": 1
                        }
                    ]
                })),
                ..GenerationRequest::default()
            })
            .expect("diff should return structured errors");

        assert_eq!(output.errors[0].code, "replace_match_count");
        assert!(output.errors[0].message.contains("expected 1 matches"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn managed_block_is_idempotent() {
        let root = temp_root("managed-block");
        fs::write(root.join("README.md"), "Intro\n").expect("test file should be written");
        let core = GenerationCore::new(&root).expect("root should be valid");
        let config = json!({
            "rules": [
                {
                    "type": "managed_block",
                    "path": "README.md",
                    "start_marker": "<!-- genify:start -->",
                    "end_marker": "<!-- genify:end -->",
                    "content": "Generated"
                }
            ]
        });

        core.apply(ApplyRequest {
            config: Some(config.clone()),
            explicit_approval: true,
            ..ApplyRequest::default()
        })
        .expect("first apply should succeed");
        let second = core
            .apply(ApplyRequest {
                config: Some(config),
                explicit_approval: true,
                ..ApplyRequest::default()
            })
            .expect("second apply should succeed");

        assert!(second.changed_files.is_empty());
        assert_eq!(
            fs::read_to_string(root.join("README.md")).expect("file should exist"),
            "Intro\n<!-- genify:start -->\nGenerated\n<!-- genify:end -->\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn copy_move_delete_and_mkdir_apply() {
        let root = temp_root("file-ops");
        fs::write(root.join("source.txt"), "source\n").expect("test file should be written");
        let core = GenerationCore::new(&root).expect("root should be valid");

        let output = core
            .apply(ApplyRequest {
                config: Some(json!({
                    "rules": [
                        {
                            "type": "copy",
                            "from": "source.txt",
                            "to": "copy.txt"
                        },
                        {
                            "type": "move",
                            "from": "copy.txt",
                            "to": "moved.txt"
                        },
                        {
                            "type": "delete",
                            "path": "source.txt"
                        },
                        {
                            "type": "mkdir",
                            "path": "nested/dir"
                        }
                    ]
                })),
                explicit_approval: true,
                ..ApplyRequest::default()
            })
            .expect("file operations should apply");

        assert!(output.errors.is_empty());
        assert!(!root.join("source.txt").exists());
        assert!(!root.join("copy.txt").exists());
        assert_eq!(
            fs::read_to_string(root.join("moved.txt")).expect("moved file should exist"),
            "source\n"
        );
        assert!(root.join("nested/dir").is_dir());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn insert_before_and_after_marker() {
        let root = temp_root("insert-marker");
        fs::write(root.join("README.md"), "A\nMARK\nZ\n").expect("test file should be written");
        let core = GenerationCore::new(&root).expect("root should be valid");

        core.apply(ApplyRequest {
            config: Some(json!({
                "rules": [
                    {
                        "type": "insert_before",
                        "path": "README.md",
                        "marker": "MARK",
                        "content": "before"
                    },
                    {
                        "type": "insert_after",
                        "path": "README.md",
                        "marker": "MARK",
                        "content": "after"
                    }
                ]
            })),
            explicit_approval: true,
            ..ApplyRequest::default()
        })
        .expect("insert operations should apply");

        assert_eq!(
            fs::read_to_string(root.join("README.md")).expect("file should exist"),
            "A\nbefore\nMARK\nafter\nZ\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    fn temp_root(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("genify-{name}-{suffix}"));
        fs::create_dir_all(&path).expect("temp root should be created");
        path
    }
}
