use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use similar::{ChangeTag, TextDiff};
use thiserror::Error;

use crate::{
    Config, Error as GenifyError, Rule, Value, parse_toml, render_config_props, render_config_rules,
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
        let changed_files = simulation.changed_relative_paths();

        for change in simulation.changed_files() {
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
            let path = self
                .sandbox
                .resolve_generated_path(effective_root, rule_path(rule))?;
            let relative_path = self.sandbox.display_path(&path);
            let operation = match rule {
                Rule::File { content, .. } => PreparedOperation {
                    kind: FileOperationKind::Create,
                    path,
                    relative_path,
                    content: content.clone(),
                    replace: None,
                },
                Rule::Append { content, .. } => PreparedOperation {
                    kind: FileOperationKind::Append,
                    path,
                    relative_path,
                    content: content.clone(),
                    replace: None,
                },
                Rule::Prepend { content, .. } => PreparedOperation {
                    kind: FileOperationKind::Prepend,
                    path,
                    relative_path,
                    content: content.clone(),
                    replace: None,
                },
                Rule::Replace {
                    replace, content, ..
                } => PreparedOperation {
                    kind: FileOperationKind::Replace,
                    path,
                    relative_path,
                    content: content.clone(),
                    replace: Some(replace.clone()),
                },
            };
            operations.push(operation);
        }
        Ok(operations)
    }

    fn simulate(&self, prepared: &PreparedGeneration) -> Result<Simulation, CoreError> {
        let mut files: BTreeMap<PathBuf, SimulatedFile> = BTreeMap::new();
        let mut warnings = Vec::new();
        let mut errors = Vec::new();

        for operation in &prepared.operations {
            let file = match files.get_mut(&operation.path) {
                Some(file) => file,
                None => {
                    let original = read_optional_string(&operation.path)?;
                    let current = original.clone().unwrap_or_default();
                    files.insert(
                        operation.path.clone(),
                        SimulatedFile {
                            path: operation.path.clone(),
                            relative_path: operation.relative_path.clone(),
                            original: current.clone(),
                            current,
                            existed: original.is_some(),
                        },
                    );
                    files
                        .get_mut(&operation.path)
                        .expect("simulated file was just inserted")
                }
            };

            operation.simulate(file, &mut warnings, &mut errors);
        }

        Ok(Simulation {
            files,
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
    pub exists: bool,
    pub will_create: bool,
    pub will_modify: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOperationKind {
    Create,
    Append,
    Prepend,
    Replace,
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
            if operation.kind == FileOperationKind::Create && exists {
                errors.push(Diagnostic::error(
                    "file_exists",
                    "file rule cannot create a file that already exists",
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
            operations.push(PlannedFileOperation {
                operation: operation.kind,
                path: operation.relative_path.clone(),
                exists,
                will_create: !exists,
                will_modify: exists || operation.kind != FileOperationKind::Create,
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
    content: String,
    replace: Option<regex::Regex>,
}

impl PreparedOperation {
    fn simulate(
        &self,
        file: &mut SimulatedFile,
        warnings: &mut Vec<Diagnostic>,
        errors: &mut Vec<Diagnostic>,
    ) {
        match self.kind {
            FileOperationKind::Create => {
                if file.existed {
                    errors.push(Diagnostic::error(
                        "file_exists",
                        "file rule cannot create a file that already exists",
                        Some(file.relative_path.clone()),
                    ));
                    return;
                }
                file.current = format!("{}\n", self.content.trim_end());
                file.existed = true;
            }
            FileOperationKind::Append => {
                file.current
                    .push_str(&format!("{}\n", self.content.trim_end()));
                file.existed = true;
            }
            FileOperationKind::Prepend => {
                file.current = format!("{}\n{}\n", self.content, file.current.trim_end());
                file.existed = true;
            }
            FileOperationKind::Replace => {
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
                    return;
                };
                let replaced = replace.replacen(&file.current, 1, self.content.as_str());
                file.current = format!("{}\n", replaced.trim_end());
                file.existed = true;
            }
        }
    }
}

#[derive(Debug, Clone)]
struct SimulatedFile {
    path: PathBuf,
    relative_path: String,
    original: String,
    current: String,
    existed: bool,
}

#[derive(Debug, Clone)]
struct Simulation {
    files: BTreeMap<PathBuf, SimulatedFile>,
    warnings: Vec<Diagnostic>,
    errors: Vec<Diagnostic>,
}

impl Simulation {
    fn changed_files(&self) -> Vec<&SimulatedFile> {
        self.files
            .values()
            .filter(|file| file.original != file.current)
            .collect()
    }

    fn changed_relative_paths(&self) -> Vec<String> {
        self.changed_files()
            .into_iter()
            .map(|file| file.relative_path.clone())
            .collect()
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
        Rule::File { path, .. }
        | Rule::Append { path, .. }
        | Rule::Prepend { path, .. }
        | Rule::Replace { path, .. } => path,
    }
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
                operation: FileOperationKind::Create,
                config: json!({
                    "rules": [
                        {
                            "type": "file",
                            "path": "new/file.rs",
                            "content": "..."
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
                            "type": "file",
                            "path": "../outside.txt",
                            "content": "nope"
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
                            "type": "file",
                            "path": "out.txt",
                            "content": "Hello {{ name }}"
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
                            "type": "file",
                            "path": "out.txt",
                            "content": "Hello"
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
