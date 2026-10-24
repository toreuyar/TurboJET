use crate::adc::AgentDefinitionConfig;
use crate::catalog;
use crate::env::EnvCascade;
use crate::package::{
    LoadedWorkflowAsset, PackageResourceReader, StoredAssetResources, WorkflowAssets,
};
use crate::smg::StateMachineGraph;
use crate::system::{ProviderConfig, TurboJetConfig, WorkflowSourceKind};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct ConfigLoader {
    base_dir: PathBuf,
    config: TurboJetConfig,
}

/// A configured source after path resolution and id assignment.
#[derive(Debug, Clone)]
pub struct ConfiguredWorkflowSource {
    pub id: String,
    pub path: PathBuf,
    pub kind: WorkflowSourceKind,
}

/// One validated workflow graph read from an explicit source. Nothing is
/// registered until every loaded source has parsed and validated.
#[derive(Debug, Clone)]
pub struct LoadedWorkflow {
    pub source_id: String,
    pub source_path: PathBuf,
    /// Relative to the config base directory when possible; otherwise absolute.
    pub source_rel_path: String,
    pub graph: StateMachineGraph,
    /// Canonical JSON including catalog/capability metadata.
    pub content_json: String,
    pub content_hash: String,
    /// The kind of the loaded workflow itself. A package discovered through a
    /// `directory` source is still `package` so admission never reads its
    /// references from disk.
    pub source_kind: WorkflowSourceKind,
    /// Package assets in logical-path order. Legacy file/directory graphs have
    /// no assets and keep today's disk resolution.
    pub assets: Vec<LoadedWorkflowAsset>,
}

enum SourceItem {
    Graph(PathBuf),
    Package(PathBuf),
}

/// Find every `workflow.smg.json` below a package root. The caller requires
/// exactly one, at the root, so nested or duplicate manifests fail closed.
fn find_workflow_manifests(root: &Path) -> Result<Vec<PathBuf>> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("cannot read package directory {}", dir.display()))?
        {
            let entry = entry.with_context(|| {
                format!(
                    "cannot read an entry in package directory {}",
                    dir.display()
                )
            })?;
            let file_type = entry.file_type().with_context(|| {
                format!("cannot inspect package entry {}", entry.path().display())
            })?;
            let path = entry.path();
            if file_type.is_file()
                && path
                    .file_name()
                    .is_some_and(|name| name == "workflow.smg.json")
            {
                found.push(path);
            } else if file_type.is_dir() {
                walk(&path, found)?;
            }
        }
        Ok(())
    }

    let mut found = Vec::new();
    walk(root, &mut found)?;
    found.sort();
    Ok(found)
}

/// One ADC, prompt or schema read during workflow validation or bundle freeze.
#[derive(Debug, Clone)]
pub struct StageResource {
    /// Diagnostic and frozen reference: an absolute/relative filesystem path
    /// for legacy graphs, or a package-root-relative logical path.
    pub path: String,
    pub bytes: Vec<u8>,
}

impl StageResource {
    pub fn text(&self) -> Result<&str> {
        std::str::from_utf8(&self.bytes)
            .with_context(|| format!("{} must contain UTF-8 text", self.path))
    }
}

/// Resource lookup shared by legacy file graphs and package asset graphs. The
/// same validation and freezing code runs for both, so package workflows cannot
/// silently get weaker checks.
pub trait StageResourceReader {
    fn read_adc(&mut self, stage_id: &str, agent_ref: &str) -> Result<StageResource>;
    fn read_prompt(&mut self, adc: &StageResource, prompt: &str) -> Result<StageResource>;
    fn read_schema(&mut self, adc: &StageResource, schema: &str) -> Result<StageResource>;
    fn read_status_schema(&mut self, adc: &StageResource, schema: &str) -> Result<StageResource>;
}

/// Legacy file resolution: exactly the existing ADC-directory candidates,
/// config-base-relative prompt/schema paths, and absolute paths.
pub struct FileResourceReader<'a> {
    loader: &'a ConfigLoader,
}

impl<'a> FileResourceReader<'a> {
    pub fn new(loader: &'a ConfigLoader) -> Self {
        Self { loader }
    }

    fn read_path(&self, path: &Path, what: &str) -> Result<StageResource> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("cannot read {what} {}", path.display()))?;
        Ok(StageResource {
            path: path.to_string_lossy().into_owned(),
            bytes,
        })
    }
}

impl StageResourceReader for FileResourceReader<'_> {
    fn read_adc(&mut self, stage_id: &str, agent_ref: &str) -> Result<StageResource> {
        let path = self.loader.resolve_adc_reference(agent_ref, stage_id)?;
        self.read_path(&path, "ADC")
    }

    fn read_prompt(&mut self, adc: &StageResource, prompt: &str) -> Result<StageResource> {
        let path = self
            .loader
            .resolve_prompt_reference(Path::new(&adc.path), prompt);
        self.read_path(&path, "prompt")
    }

    fn read_schema(&mut self, _adc: &StageResource, schema: &str) -> Result<StageResource> {
        let path = self.loader.resolve_path(schema);
        self.read_path(&path, "schema")
    }

    fn read_status_schema(&mut self, _adc: &StageResource, schema: &str) -> Result<StageResource> {
        let path = self.loader.resolve_path(schema);
        self.read_path(&path, "schema")
    }
}

impl ConfigLoader {
    pub fn from_file(path: &Path) -> Result<Self> {
        // Resolve to an absolute path so derived paths (agents_dir, mount sources)
        // are absolute too — Docker rejects relative bind-mount sources.
        let abs_path = std::fs::canonicalize(path)
            .with_context(|| format!("Failed to canonicalize config path: {}", path.display()))?;
        let base_dir = abs_path
            .parent()
            .and_then(|p| p.parent())
            .unwrap_or(Path::new("."))
            .to_path_buf();

        let mut cascade = EnvCascade::new();

        let env_paths = [
            base_dir.join(".env"),
            PathBuf::from(shellexpand::tilde("$TURBOJET_HOME/.env").to_string()),
        ];
        for env_path in &env_paths {
            if env_path.exists() {
                tracing::info!(path = %env_path.display(), "Loading .env file");
                cascade.load_from_file(env_path)?;
                break;
            }
        }

        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        let mut config: TurboJetConfig = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;

        for provider in &mut config.providers {
            Self::resolve_provider(&cascade, provider);
        }

        Self::canonicalize_agent_ids(&mut config.agents)?;

        for agent in &mut config.agents {
            if !agent.model.is_empty() {
                if agent.id == "chief" {
                    // Canonical key wins. The legacy key is read only during the
                    // bounded compatibility window and never logged with its value.
                    if let Some(model) = cascade.lookup("TURBOJET_AGENT_CHIEF_MODEL") {
                        agent.model = model;
                    } else if let Some(model) = cascade.lookup("TURBOJET_AGENT_CHEF_MODEL") {
                        tracing::warn!(
                            "TURBOJET_AGENT_CHEF_MODEL is deprecated; use TURBOJET_AGENT_CHIEF_MODEL"
                        );
                        agent.model = model;
                    }
                } else {
                    let env_key = format!("TURBOJET_AGENT_{}_MODEL", agent.id.to_uppercase());
                    agent.model = cascade.resolve(&env_key, &agent.model).to_string();
                }
            }
        }

        Ok(Self { base_dir, config })
    }

    /// Canonicalize the single legacy persona id (`chef` -> `chief`). Both ids
    /// in one config are ambiguous and rejected instead of silently picking one.
    fn canonicalize_agent_ids(agents: &mut [crate::system::AgentConfig]) -> Result<()> {
        for agent in agents.iter() {
            if agent.id == "chef" && agents.iter().any(|other| other.id == "chief") {
                anyhow::bail!(
                    "agent ids 'chef' and 'chief' are both configured; keep only the canonical 'chief' id"
                );
            }
        }
        for agent in agents.iter_mut() {
            if agent.id == "chef" {
                tracing::warn!(
                    "agent id 'chef' is deprecated; use the canonical id 'chief' (sessions are not keyed by this display id)"
                );
                agent.id = "chief".into();
            }
        }
        let mut seen = HashSet::new();
        for agent in agents.iter() {
            anyhow::ensure!(
                seen.insert(agent.id.clone()),
                "duplicate agent id '{}'; agent ids must be unique after compatibility normalization",
                agent.id
            );
        }
        Ok(())
    }

    fn resolve_provider(cascade: &EnvCascade, provider: &mut ProviderConfig) {
        let prefix = format!("TURBOJET_PROVIDER_{}", provider.id.to_uppercase());

        provider.base_url = cascade
            .resolve(&format!("{}_BASE_URL", prefix), &provider.base_url)
            .to_string();

        provider.default_model = cascade
            .resolve(&format!("{}_MODEL", prefix), &provider.default_model)
            .to_string();

        if std::env::var(format!("{}_API_KEY", prefix)).is_ok() {
            provider.api_key_env = format!("{}_API_KEY", prefix);
        }
    }

    pub fn config(&self) -> &TurboJetConfig {
        &self.config
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn resolve_path(&self, relative: &str) -> PathBuf {
        let expanded = shellexpand::tilde(relative).to_string();
        let expanded = if expanded.contains("$TURBOJET_HOME") {
            let home = std::env::var("TURBOJET_HOME").unwrap_or_else(|_| {
                format!("{}/.turbojet", std::env::var("HOME").unwrap_or_default())
            });
            expanded.replace("$TURBOJET_HOME", &home)
        } else {
            expanded
        };
        let p = PathBuf::from(&expanded);
        if p.is_absolute() {
            p
        } else {
            self.base_dir.join(&p)
        }
    }

    /// Legacy/test-only single-graph loader. Production startup uses
    /// `load_workflow_sources`; this method never supplies a built-in default.
    pub fn load_smg(&self) -> Result<StateMachineGraph> {
        let smg_path = self.config.smg_path.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "no smg_path configured; use workflow_sources (load_smg is legacy/test-only)"
            )
        })?;
        let path = self.resolve_path(smg_path);
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read SMG file: {}", path.display()))?;
        let smg: StateMachineGraph = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse SMG file: {}", path.display()))?;
        self.validate_smg(&smg)
            .with_context(|| format!("Invalid workflow {}", path.display()))?;
        Ok(smg)
    }

    /// Resolve configured sources, including the deprecated `smg_path` alias.
    /// Duplicate operator ids fail before any file is read.
    pub fn configured_workflow_sources(&self) -> Result<Vec<ConfiguredWorkflowSource>> {
        let mut sources = Vec::new();
        for source in &self.config.workflow_sources {
            anyhow::ensure!(
                !source.path.trim().is_empty(),
                "workflow source path must not be empty"
            );
            let id = match source.id.as_deref() {
                Some(id) if !id.trim().is_empty() => id.trim().to_string(),
                Some(_) => anyhow::bail!("workflow source '{}' has an empty id", source.path),
                None => workflow_source_slug(&source.path),
            };
            sources.push(ConfiguredWorkflowSource {
                id,
                path: self.resolve_path(&source.path),
                kind: source.kind,
            });
        }
        if let Some(legacy) = self.config.smg_path.as_deref() {
            anyhow::ensure!(
                !legacy.trim().is_empty(),
                "smg_path is configured but empty; remove it or name a workflow file"
            );
            sources.push(ConfiguredWorkflowSource {
                id: "legacy-smg-path".into(),
                path: self.resolve_path(legacy),
                kind: WorkflowSourceKind::File,
            });
        }
        let mut seen = HashSet::new();
        for source in &sources {
            anyhow::ensure!(
                seen.insert(source.id.clone()),
                "duplicate workflow source id '{}'; source ids must be unique",
                source.id
            );
        }
        Ok(sources)
    }

    /// Parse and validate every explicitly configured workflow source and every
    /// referenced ADC/prompt/schema. The caller registers only after this
    /// returns successfully, so a bad source cannot leave partial registry rows.
    pub fn load_workflow_sources(&self) -> Result<Vec<LoadedWorkflow>> {
        if self.config.smg_path.is_some() {
            tracing::warn!(
                "smg_path is deprecated; use workflow_sources with an explicit file, directory or package source"
            );
        }
        let sources = self.configured_workflow_sources()?;
        let mut loaded = Vec::new();
        let mut default_catalog: Option<String> = None;
        for source in sources {
            for item in self.expand_source(&source)? {
                let workflow = match item {
                    SourceItem::Graph(path) => self.load_graph_source(&source, &path)?,
                    SourceItem::Package(path) => self.load_package_source(&source, &path)?,
                };
                self.check_catalog_default(&workflow, &mut default_catalog)?;
                loaded.push(workflow);
            }
        }
        Ok(loaded)
    }

    fn load_graph_source(
        &self,
        source: &ConfiguredWorkflowSource,
        path: &Path,
    ) -> Result<LoadedWorkflow> {
        let raw = std::fs::read_to_string(path).with_context(|| {
            format!(
                "cannot read workflow source '{}' file {}",
                source.id,
                path.display()
            )
        })?;
        let graph: StateMachineGraph = serde_json::from_str(&raw).with_context(|| {
            format!(
                "workflow source '{}' file {} is not valid SMG JSON",
                source.id,
                path.display()
            )
        })?;
        self.validate_smg(&graph).with_context(|| {
            format!(
                "workflow source '{}' file {} failed validation",
                source.id,
                path.display()
            )
        })?;
        let content_json = serde_json::to_string(&graph)
            .context("serialize validated workflow for content addressing")?;
        let content_hash = format!("{:x}", Sha256::digest(content_json.as_bytes()));
        Ok(LoadedWorkflow {
            source_id: source.id.clone(),
            source_path: path.to_path_buf(),
            source_rel_path: self.relative_source_path(path),
            graph,
            content_json,
            content_hash,
            source_kind: source.kind,
            assets: Vec::new(),
        })
    }

    fn check_catalog_default(
        &self,
        workflow: &LoadedWorkflow,
        default_catalog: &mut Option<String>,
    ) -> Result<()> {
        let catalog = catalog::validate_for_source_import(&workflow.graph).with_context(|| {
            format!(
                "workflow source '{}' file {} cannot be imported",
                workflow.source_id,
                workflow.source_path.display()
            )
        })?;
        if catalog.default {
            if let Some(existing) = default_catalog.as_deref() {
                anyhow::ensure!(
                    existing == catalog.id,
                    "workflow source '{}' file {} declares catalog default '{}' but catalog '{}' is already the default; at most one catalog default is allowed",
                    workflow.source_id,
                    workflow.source_path.display(),
                    catalog.id,
                    existing
                );
            } else {
                *default_catalog = Some(catalog.id.clone());
            }
        }
        Ok(())
    }

    fn relative_source_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.base_dir)
            .map(|rel| rel.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned())
    }

    /// Load one package directory. Validation uses package-confined resolution
    /// and records every referenced asset before the caller persists anything.
    fn load_package_source(
        &self,
        source: &ConfiguredWorkflowSource,
        package_dir: &Path,
    ) -> Result<LoadedWorkflow> {
        let metadata = std::fs::metadata(package_dir).with_context(|| {
            format!(
                "explicitly configured workflow source '{}' is missing: {}",
                source.id,
                package_dir.display()
            )
        })?;
        anyhow::ensure!(
            metadata.is_dir(),
            "workflow source '{}' declares kind={} but {} is not a directory",
            source.id,
            source.kind.as_str(),
            package_dir.display()
        );
        let root = std::fs::canonicalize(package_dir).with_context(|| {
            format!("package '{}' could not be resolved", package_dir.display())
        })?;
        let manifests = find_workflow_manifests(&root)?;
        anyhow::ensure!(
            manifests.len() == 1,
            "package '{}' must contain exactly one workflow.smg.json; found {}: {}",
            root.display(),
            manifests.len(),
            manifests
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let manifest = &manifests[0];
        anyhow::ensure!(
            manifest == &root.join("workflow.smg.json"),
            "package '{}' must place workflow.smg.json at its root; found {}",
            root.display(),
            manifest.display()
        );
        let raw = std::fs::read(manifest)
            .with_context(|| format!("cannot read package manifest {}", manifest.display()))?;
        let manifest_text = std::str::from_utf8(&raw).with_context(|| {
            format!("package manifest {} must be UTF-8 text", manifest.display())
        })?;
        let graph: StateMachineGraph = serde_json::from_str(manifest_text).with_context(|| {
            format!(
                "package '{}' workflow.smg.json is not valid SMG JSON",
                root.display()
            )
        })?;

        let mut reader = PackageResourceReader::new(&root);
        reader.insert_manifest(raw.clone())?;
        self.validate_smg_with_reader(&graph, &mut reader)
            .with_context(|| {
                format!(
                    "package '{}' workflow.smg.json failed validation",
                    root.display()
                )
            })?;
        catalog::validate_for_source_import(&graph).with_context(|| {
            format!(
                "package '{}' workflow.smg.json cannot be imported",
                root.display()
            )
        })?;
        let content_json = serde_json::to_string(&graph)
            .context("serialize validated workflow for content addressing")?;
        let content_hash = format!("{:x}", Sha256::digest(content_json.as_bytes()));
        let assets = reader.into_loaded_assets();
        Ok(LoadedWorkflow {
            source_id: source.id.clone(),
            source_path: manifest.clone(),
            source_rel_path: self.relative_source_path(manifest),
            graph,
            content_json,
            content_hash,
            source_kind: WorkflowSourceKind::Package,
            assets,
        })
    }

    fn expand_source(&self, source: &ConfiguredWorkflowSource) -> Result<Vec<SourceItem>> {
        match source.kind {
            WorkflowSourceKind::File => {
                let metadata = std::fs::metadata(&source.path).with_context(|| {
                    format!(
                        "explicitly configured workflow source '{}' is missing: {}",
                        source.id,
                        source.path.display()
                    )
                })?;
                anyhow::ensure!(
                    metadata.is_file(),
                    "workflow source '{}' declares kind=file but {} is not a file",
                    source.id,
                    source.path.display()
                );
                Ok(vec![SourceItem::Graph(source.path.clone())])
            }
            WorkflowSourceKind::Directory => {
                let metadata = std::fs::metadata(&source.path).with_context(|| {
                    format!(
                        "explicitly configured workflow source '{}' is missing: {}",
                        source.id,
                        source.path.display()
                    )
                })?;
                anyhow::ensure!(
                    metadata.is_dir(),
                    "workflow source '{}' declares kind=directory but {} is not a directory",
                    source.id,
                    source.path.display()
                );
                let mut paths = Vec::new();
                let mut package_dirs = Vec::new();
                for entry in std::fs::read_dir(&source.path).with_context(|| {
                    format!(
                        "cannot read workflow source '{}' directory {}",
                        source.id,
                        source.path.display()
                    )
                })? {
                    let entry = entry.with_context(|| {
                        format!(
                            "cannot read an entry in workflow source '{}' directory {}",
                            source.id,
                            source.path.display()
                        )
                    })?;
                    let path = entry.path();
                    let file_type = entry.file_type().ok();
                    if file_type.map(|kind| kind.is_file()).unwrap_or(false)
                        && path
                            .file_name()
                            .map(|name| name.to_string_lossy().ends_with(".smg.json"))
                            .unwrap_or(false)
                    {
                        paths.push(path);
                    } else if file_type.map(|kind| kind.is_dir()).unwrap_or(false)
                        && path.join("workflow.smg.json").is_file()
                    {
                        package_dirs.push(path);
                    }
                }
                paths.sort();
                package_dirs.sort();
                let mut items = paths.into_iter().map(SourceItem::Graph).collect::<Vec<_>>();
                items.extend(package_dirs.into_iter().map(SourceItem::Package));
                Ok(items)
            }
            WorkflowSourceKind::Package => {
                let metadata = std::fs::metadata(&source.path).with_context(|| {
                    format!(
                        "explicitly configured workflow source '{}' is missing: {}",
                        source.id,
                        source.path.display()
                    )
                })?;
                anyhow::ensure!(
                    metadata.is_dir(),
                    "workflow source '{}' declares kind=package but {} is not a directory",
                    source.id,
                    source.path.display()
                );
                Ok(vec![SourceItem::Package(source.path.clone())])
            }
        }
    }

    /// Validate both graph topology and every executable resource reference.
    /// This same contract is used by startup, imports, run admission and the
    /// executor so a late file/config error cannot become a successful route.
    pub fn validate_smg(&self, smg: &StateMachineGraph) -> Result<()> {
        let mut reader = FileResourceReader::new(self);
        self.validate_smg_with_reader(smg, &mut reader)
    }

    /// Validate a graph whose ADC/prompt/schema content comes from already
    /// imported package assets. No filesystem reference is read.
    pub fn validate_smg_with_assets(
        &self,
        smg: &StateMachineGraph,
        assets: &WorkflowAssets,
    ) -> Result<()> {
        let mut reader = StoredAssetResources::new(assets);
        self.validate_smg_with_reader(smg, &mut reader)
    }

    /// Shared validation for legacy files and package assets.
    pub fn validate_smg_with_reader(
        &self,
        smg: &StateMachineGraph,
        reader: &mut dyn StageResourceReader,
    ) -> Result<()> {
        crate::validation::validate_graph(smg)?;
        crate::capability::validate_for_graph(smg)?;
        let stage_names: std::collections::HashSet<&str> = smg
            .flows
            .values()
            .flat_map(|flow| flow.stages.keys().map(String::as_str))
            .collect();

        for (flow_name, flow) in &smg.flows {
            for (stage_name, stage) in &flow.stages {
                let contract = format!(
                    "workflow '{}' flow '{}' stage '{}'",
                    smg.name, flow_name, stage_name
                );
                let adc_resource =
                    reader.read_adc(stage_name, &stage.agent).with_context(|| {
                        format!("{contract}: invalid agent/ADC reference '{}'", stage.agent)
                    })?;
                let adc_raw = adc_resource.text().with_context(|| {
                    format!("{contract}: cannot read ADC {}", adc_resource.path)
                })?;
                let adc: AgentDefinitionConfig = serde_json::from_str(adc_raw)
                    .with_context(|| format!("{contract}: invalid ADC {}", adc_resource.path))?;
                anyhow::ensure!(
                    !adc.id.trim().is_empty(),
                    "{contract}: ADC {} has empty id",
                    adc_resource.path
                );
                anyhow::ensure!(
                    !adc.name.trim().is_empty(),
                    "{contract}: ADC {} has empty name",
                    adc_resource.path
                );
                anyhow::ensure!(
                    !adc.prompt.trim().is_empty(),
                    "{contract}: ADC {} must reference a prompt",
                    adc_resource.path
                );
                anyhow::ensure!(
                    adc.timeout > 0,
                    "{contract}: ADC {} timeout must be positive",
                    adc_resource.path
                );

                let prompt_resource = reader
                    .read_prompt(&adc_resource, &adc.prompt)
                    .with_context(|| format!("{contract}: cannot read prompt"))?;
                let prompt = prompt_resource.text().with_context(|| {
                    format!("{contract}: cannot read prompt {}", prompt_resource.path)
                })?;
                anyhow::ensure!(
                    !prompt.trim().is_empty(),
                    "{contract}: prompt {} is empty",
                    prompt_resource.path
                );

                anyhow::ensure!(
                    !adc.schema.trim().is_empty(),
                    "{contract}: ADC {} must reference an output schema",
                    adc_resource.path
                );
                let schema_resource = reader
                    .read_schema(&adc_resource, &adc.schema)
                    .with_context(|| format!("{contract}: cannot read schema"))?;
                let schema_raw = schema_resource.text().with_context(|| {
                    format!("{contract}: cannot read schema {}", schema_resource.path)
                })?;
                let schema: serde_json::Value =
                    serde_json::from_str(schema_raw).with_context(|| {
                        format!("{contract}: invalid schema JSON {}", schema_resource.path)
                    })?;
                validate_schema_document(&schema).with_context(|| {
                    format!(
                        "{contract}: unsupported/invalid schema {}",
                        schema_resource.path
                    )
                })?;

                let verdict = schema
                    .get("properties")
                    .and_then(|v| v.get("verdict"))
                    .with_context(|| {
                        format!(
                            "{contract}: schema {} must constrain properties.verdict",
                            schema_resource.path
                        )
                    })?;
                anyhow::ensure!(
                    verdict.get("type").and_then(|t| t.as_str()) == Some("string"),
                    "{contract}: schema {} properties.verdict must have type string",
                    schema_resource.path
                );
                let mut schema_routes: Vec<&str> = verdict
                    .get("enum")
                    .and_then(|v| v.as_array())
                    .with_context(|| {
                        format!(
                            "{contract}: schema {} properties.verdict must declare an enum",
                            schema_resource.path
                        )
                    })?
                    .iter()
                    .map(|v| {
                        v.as_str().with_context(|| {
                            format!(
                                "{contract}: schema {} verdict enum values must be strings",
                                schema_resource.path
                            )
                        })
                    })
                    .collect::<Result<_>>()?;
                let mut route_keys: Vec<&str> = stage.routes.keys().map(String::as_str).collect();
                schema_routes.sort_unstable();
                route_keys.sort_unstable();
                anyhow::ensure!(
                    schema_routes == route_keys,
                    "{contract}: schema {} verdict enum {:?} does not exactly match route keys {:?}",
                    schema_resource.path,
                    schema_routes,
                    route_keys
                );

                if smg.maturity == crate::smg::WorkflowMaturity::Supported {
                    anyhow::ensure!(
                        !adc.tools.bash,
                        "{contract}: supported workflow profile must not grant shell/bash tools"
                    );
                    anyhow::ensure!(
                        !adc.tools.edit,
                        "{contract}: supported workflow profile must not grant repository edit tools"
                    );
                }
                validate_artifact_contracts(&contract, &adc, &stage_names)?;
                if let Some(status) = &adc.output.status
                    && !status.schema.trim().is_empty()
                {
                    let status_resource = reader
                        .read_status_schema(&adc_resource, &status.schema)
                        .with_context(|| {
                            format!(
                                "{contract}: invalid output status schema '{}'",
                                status.schema
                            )
                        })?;
                    let status_raw = status_resource.text().with_context(|| {
                        format!(
                            "{contract}: invalid output status schema '{}'",
                            status.schema
                        )
                    })?;
                    let status_schema: serde_json::Value = serde_json::from_str(status_raw)
                        .with_context(|| {
                            format!(
                                "{contract}: invalid output status schema '{}'",
                                status.schema
                            )
                        })?;
                    validate_schema_document(&status_schema).with_context(|| {
                        format!(
                            "{contract}: invalid output status schema '{}'",
                            status.schema
                        )
                    })?;
                }
            }
        }
        Ok(())
    }

    /// Resolve the legacy shared ADC directory. There is no built-in default:
    /// package workflows carry their own agents and never read this path.
    fn legacy_adc_dir(&self) -> Result<PathBuf> {
        let configured = self.config.adc_dir.trim();
        anyhow::ensure!(
            !configured.is_empty(),
            "legacy bare agent references require an explicit adc_dir; configure a package workflow or set adc_dir to the directory holding the referenced agents"
        );
        Ok(self.resolve_path(configured))
    }

    pub fn load_adc(&self, stage_id: &str) -> Result<AgentDefinitionConfig> {
        let dir = self.legacy_adc_dir()?;
        let candidates = [
            dir.join(stage_id).join("agent.json"),
            dir.join(format!("{}.json", stage_id)),
            dir.join(format!("{}.adc.json", stage_id)),
            dir.join(stage_id).join("adc.json"),
        ];
        for candidate in &candidates {
            if candidate.exists() {
                let raw = std::fs::read_to_string(candidate)
                    .with_context(|| format!("Failed to read ADC file: {}", candidate.display()))?;
                let adc: AgentDefinitionConfig = serde_json::from_str(&raw).with_context(|| {
                    format!("Failed to parse ADC file: {}", candidate.display())
                })?;
                return Ok(adc);
            }
        }
        anyhow::bail!(
            "ADC file not found for stage '{}'. Searched: {}",
            stage_id,
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    /// Resolve an SMG's explicit agent reference. Path references are loaded
    /// exactly; legacy bare identifiers use the same ADC-directory candidates
    /// as `load_adc` but never substitute defaults on failure.
    pub fn load_adc_reference(
        &self,
        stage_id: &str,
        agent_ref: &str,
    ) -> Result<AgentDefinitionConfig> {
        let path = self.resolve_adc_reference(agent_ref, stage_id)?;
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read ADC file: {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse ADC file: {}", path.display()))
    }

    pub fn load_prompt_for_adc(
        &self,
        stage_id: &str,
        agent_ref: &str,
        prompt: &str,
    ) -> Result<String> {
        anyhow::ensure!(
            !prompt.trim().is_empty(),
            "ADC prompt path is required for stage '{}'",
            stage_id
        );
        let adc_path = self.resolve_adc_reference(agent_ref, stage_id)?;
        let path = self.resolve_prompt_reference(&adc_path, prompt);
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read prompt file: {}", path.display()))?;
        anyhow::ensure!(
            !content.trim().is_empty(),
            "Prompt file is empty: {}",
            path.display()
        );
        Ok(content)
    }

    pub(crate) fn resolve_adc_reference(&self, agent_ref: &str, stage_id: &str) -> Result<PathBuf> {
        let reference = Path::new(agent_ref);
        if reference.is_absolute() || agent_ref.contains('/') || agent_ref.ends_with(".json") {
            let resolved = self.resolve_path(agent_ref);
            if resolved.is_dir() {
                let candidate = resolved.join("agent.json");
                anyhow::ensure!(
                    candidate.is_file(),
                    "ADC file not found: {}",
                    candidate.display()
                );
                return Ok(candidate);
            }
            anyhow::ensure!(
                resolved.is_file(),
                "ADC file not found: {}",
                resolved.display()
            );
            return Ok(resolved);
        }
        let dir = self.legacy_adc_dir()?;
        for id in [agent_ref, stage_id] {
            for candidate in [
                dir.join(id).join("agent.json"),
                dir.join(format!("{id}.json")),
                dir.join(format!("{id}.adc.json")),
                dir.join(id).join("adc.json"),
            ] {
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
        anyhow::bail!(
            "ADC reference '{}' for stage '{}' did not resolve under {}",
            agent_ref,
            stage_id,
            dir.display()
        )
    }

    pub(crate) fn resolve_prompt_reference(&self, adc_path: &Path, prompt: &str) -> PathBuf {
        let prompt_path = Path::new(prompt);
        if prompt_path.is_absolute() {
            return prompt_path.to_path_buf();
        }
        if prompt.contains('/') {
            return self.resolve_path(prompt);
        }
        adc_path
            .parent()
            .unwrap_or(self.base_dir())
            .join(prompt_path)
    }

    pub fn load_prompt(&self, prompt_path: &str) -> Result<String> {
        let path = self.resolve_path(prompt_path);
        std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read prompt file: {}", path.display()))
    }

    pub fn load_prompt_file(&self, relative_path: &str) -> Result<String> {
        let path = self.resolve_path(relative_path);
        std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read prompt file: {}", path.display()))
    }

    /// Load a JSON schema file (path relative to the config root) for use as a
    /// structured-output contract. Empty path or unreadable file yields None
    /// (with a warning for non-empty paths), so stages without schemas are unaffected.
    pub fn load_schema(&self, schema_path: &str) -> Option<serde_json::Value> {
        if schema_path.trim().is_empty() {
            return None;
        }
        let path = self.resolve_path(schema_path);
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str(&raw) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(schema = %path.display(), error = %e, "Invalid schema JSON, ignoring");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(schema = %path.display(), error = %e, "Failed to read schema file, ignoring");
                None
            }
        }
    }

    pub fn load_schema_required(&self, schema_path: &str) -> Result<serde_json::Value> {
        anyhow::ensure!(!schema_path.trim().is_empty(), "schema path is required");
        let path = self.resolve_path(schema_path);
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read schema file: {}", path.display()))?;
        let schema: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse schema file: {}", path.display()))?;
        validate_schema_document(&schema)
            .with_context(|| format!("Invalid schema file: {}", path.display()))?;
        Ok(schema)
    }

    pub fn validate_artifact(&self, artifact: &serde_json::Value, schema_path: &str) -> Result<()> {
        let path = self.resolve_path(schema_path);
        let schema_raw = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read schema file: {}", path.display()))?;
        let schema: serde_json::Value = serde_json::from_str(&schema_raw)?;
        validate_json_value(artifact, &schema, "")?;
        Ok(())
    }
}

/// Validate a JSON value against a subset-of-JSON-Schema document. Supported
/// constraints: `type` (string/integer/number/boolean/object/array/null),
/// `enum` (JSON-value membership), `required`, and recursive `properties`.
/// Error messages name the JSON path of the violated constraint so they can be
/// fed back verbatim as corrective output-contract violations.
/// Derive a readable source id from a path. The id is operator metadata only;
/// it never selects catalog or workflow behavior.
fn workflow_source_slug(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let file = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    let stem = file.strip_suffix(".smg.json").unwrap_or(file);
    let mut out = String::new();
    let mut last_dash = false;
    for ch in stem.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            last_dash = false;
        } else if !out.is_empty() && !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "workflow-source".into()
    } else {
        out.to_string()
    }
}

pub fn validate_json_value(
    value: &serde_json::Value,
    schema: &serde_json::Value,
    path: &str,
) -> Result<()> {
    if let Some(expected) = schema.get("type").and_then(|t| t.as_str())
        && !type_matches(value, expected)
    {
        anyhow::bail!(
            "Validation error at {}: expected type '{}', found '{}'",
            path_display(path),
            expected,
            json_type_name(value)
        );
    }
    if let Some(allowed) = schema.get("enum").and_then(|e| e.as_array())
        && !allowed.iter().any(|candidate| candidate == value)
    {
        let allowed_str =
            serde_json::to_string(allowed).unwrap_or_else(|_| "<unrepresentable enum>".to_string());
        anyhow::bail!(
            "Validation error at {}: value {} is not one of {}",
            path_display(path),
            value,
            allowed_str
        );
    }
    if let Some(required) = schema.get("required").and_then(|r| r.as_array())
        && let Some(obj) = value.as_object()
    {
        for field in required {
            if let Some(field_name) = field.as_str()
                && !obj.contains_key(field_name)
            {
                anyhow::bail!(
                    "Validation error at {}: required field '{}' missing",
                    join_path(path, field_name),
                    field_name
                );
            }
        }
    }
    if let Some(properties) = schema.get("properties").and_then(|p| p.as_object())
        && let Some(obj) = value.as_object()
    {
        for (key, prop_schema) in properties {
            if let Some(val) = obj.get(key) {
                validate_json_value(val, prop_schema, &join_path(path, key))?;
            }
        }
    }
    if let (Some(item_schema), Some(items)) = (schema.get("items"), value.as_array()) {
        for (index, item) in items.iter().enumerate() {
            validate_json_value(item, item_schema, &join_path(path, &index.to_string()))?;
        }
    }
    Ok(())
}

/// Validate the deliberately supported JSON-Schema subset before use. Unknown
/// keywords are errors rather than silently pretending to enforce them.
pub fn validate_schema_document(schema: &serde_json::Value) -> Result<()> {
    let obj = schema.as_object().context("schema must be an object")?;
    for key in obj.keys() {
        anyhow::ensure!(
            matches!(
                key.as_str(),
                "type" | "enum" | "required" | "properties" | "items" | "description"
            ),
            "unsupported schema keyword '{}'",
            key
        );
    }
    if let Some(typ) = obj.get("type") {
        let typ = typ.as_str().context("schema type must be a string")?;
        anyhow::ensure!(
            matches!(
                typ,
                "string" | "integer" | "number" | "boolean" | "object" | "array" | "null"
            ),
            "unsupported schema type '{}'",
            typ
        );
    }
    if let Some(values) = obj.get("enum") {
        anyhow::ensure!(
            values.as_array().is_some_and(|a| !a.is_empty()),
            "schema enum must be a non-empty array"
        );
    }
    if let Some(required) = obj.get("required") {
        let fields = required
            .as_array()
            .context("schema required must be an array")?;
        anyhow::ensure!(
            fields.iter().all(serde_json::Value::is_string),
            "schema required entries must be strings"
        );
    }
    if let Some(properties) = obj.get("properties") {
        let properties = properties
            .as_object()
            .context("schema properties must be an object")?;
        for (name, property) in properties {
            validate_schema_document(property)
                .with_context(|| format!("schema property '{}'", name))?;
        }
    }
    if let Some(items) = obj.get("items") {
        anyhow::ensure!(
            obj.get("type").and_then(serde_json::Value::as_str) == Some("array"),
            "schema items is only supported when type is array"
        );
        validate_schema_document(items).context("schema items")?;
    }
    if let (Some(required), Some(properties)) = (obj.get("required"), obj.get("properties")) {
        let names: std::collections::HashSet<&str> = properties
            .as_object()
            .expect("validated")
            .keys()
            .map(String::as_str)
            .collect();
        for field in required.as_array().expect("validated") {
            let field = field.as_str().expect("validated");
            anyhow::ensure!(
                names.contains(field),
                "schema required field '{}' has no matching property",
                field
            );
        }
    }
    Ok(())
}

fn validate_artifact_contracts(
    context: &str,
    adc: &AgentDefinitionConfig,
    stage_names: &std::collections::HashSet<&str>,
) -> Result<()> {
    for (kind, artifacts) in [
        ("input", &adc.input.artifacts),
        ("output", &adc.output.artifacts),
    ] {
        let mut names = std::collections::HashSet::new();
        for artifact in artifacts {
            anyhow::ensure!(
                !artifact.name.trim().is_empty(),
                "{context}: ADC {kind} artifact name must not be empty"
            );
            anyhow::ensure!(
                names.insert(artifact.name.as_str()),
                "{context}: ADC {kind} declares duplicate artifact '{}'",
                artifact.name
            );
            if let Some(from) = artifact.from.as_deref() {
                anyhow::ensure!(
                    stage_names.contains(from),
                    "{context}: ADC input artifact '{}' references unknown source stage '{}'",
                    artifact.name,
                    from
                );
            }
            if let Some(verify) = artifact.verify.as_deref() {
                anyhow::ensure!(
                    verify == "non-empty",
                    "{context}: ADC artifact '{}' uses unsupported verify rule '{}'",
                    artifact.name,
                    verify
                );
            }
        }
    }
    Ok(())
}

fn type_matches(value: &serde_json::Value, expected: &str) -> bool {
    match expected {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn path_display(path: &str) -> &str {
    if path.is_empty() { "(root)" } else { path }
}

fn join_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{}.{}", path, key)
    }
}

#[cfg(test)]
mod tests {
    use super::ConfigLoader;
    use super::validate_json_value;
    use serde_json::json;

    fn stage_schema() -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["verdict", "content"],
            "properties": {
                "verdict": { "type": "string", "enum": ["proceed", "reject"] },
                "content": { "type": "string" },
                "summary": { "type": "string" },
                "meta": {
                    "type": "object",
                    "required": ["author"],
                    "properties": {
                        "author": { "type": "string" }
                    }
                }
            }
        })
    }

    #[test]
    fn accepts_valid_artifact() {
        let artifact = json!({
            "verdict": "proceed",
            "content": "report body",
            "meta": { "author": "agent" }
        });
        assert!(validate_json_value(&artifact, &stage_schema(), "").is_ok());
    }

    #[test]
    fn rejects_missing_required_field_with_path_in_error() {
        let artifact = json!({ "content": "report body" });
        let err = validate_json_value(&artifact, &stage_schema(), "").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("required field 'verdict' missing"), "{}", msg);
        assert!(msg.contains("Validation error at verdict"), "{}", msg);
    }

    #[test]
    fn rejects_wrong_type_with_path_in_error() {
        let artifact = json!({ "verdict": "proceed", "content": 42 });
        let err = validate_json_value(&artifact, &stage_schema(), "").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Validation error at content"), "{}", msg);
        assert!(msg.contains("expected type 'string'"), "{}", msg);
        assert!(msg.contains("found 'integer'"), "{}", msg);
    }

    #[test]
    fn rejects_value_outside_enum() {
        let artifact = json!({ "verdict": "maybe", "content": "x" });
        let err = validate_json_value(&artifact, &stage_schema(), "").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Validation error at verdict"), "{}", msg);
        assert!(msg.contains("\"maybe\""), "{}", msg);
        assert!(msg.contains("not one of"), "{}", msg);
    }

    #[test]
    fn rejects_non_object_against_object_schema() {
        let schema = json!({ "type": "object" });
        let err = validate_json_value(&json!([1, 2]), &schema, "").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Validation error at (root)"), "{}", msg);
        assert!(msg.contains("expected type 'object'"), "{}", msg);
        assert!(msg.contains("found 'array'"), "{}", msg);
    }

    #[test]
    fn validates_nested_properties_with_dotted_path() {
        let artifact = json!({
            "verdict": "reject",
            "content": "x",
            "meta": { "author": 7 }
        });
        let err = validate_json_value(&artifact, &stage_schema(), "").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Validation error at meta.author"), "{}", msg);
        assert!(msg.contains("expected type 'string'"), "{}", msg);
    }

    #[test]
    fn integer_type_accepts_whole_numbers_only() {
        let schema = json!({ "type": "integer" });
        assert!(validate_json_value(&json!(3), &schema, "n").is_ok());
        let err = validate_json_value(&json!(3.5), &schema, "n").unwrap_err();
        assert!(
            err.to_string().contains("expected type 'integer'"),
            "{}",
            err
        );
    }

    #[test]
    fn synthetic_workflow_and_all_references_validate() {
        use tj_test_support::{
            DEFAULT_CHECK, DEFAULT_FINISH, DEFAULT_FLOW, DEFAULT_GATE, SyntheticTree,
            SyntheticWorkflow,
        };

        let tree = SyntheticTree::with_workflow(SyntheticWorkflow::new().with_gate(DEFAULT_GATE));
        let loader = ConfigLoader::from_file(tree.config_path()).unwrap();
        let smg = loader.load_workflow_sources().unwrap().remove(0).graph;
        assert_eq!(smg.maturity, crate::smg::WorkflowMaturity::Experimental);
        assert!(smg.flows[DEFAULT_FLOW].stages.contains_key(DEFAULT_CHECK));
        assert!(smg.flows[DEFAULT_FLOW].stages.contains_key(DEFAULT_FINISH));
        assert!(!smg.flows[DEFAULT_GATE].stages.contains_key(DEFAULT_CHECK));
    }

    #[test]
    fn bare_agent_reference_without_adc_dir_fails_actionably() {
        let (guard, _loader) = temp_workflow();
        let config_path = guard.0.join("config/fixture.json");
        write(
            &config_path,
            &json!({"smg_path": "config/graph.json"}).to_string(),
        );
        write(
            &guard.0.join("config/graph.json"),
            &json!({
                "version": 1, "name": "No shared tree", "entry": "work", "exit": "work",
                "limits": {"maxIterations": 2, "maxSteps": 5, "maxDepth": 2},
                "flow-order": ["work"],
                "flows": {"work": {"type": "flow", "entry": "start", "exit": "start",
                    "stages": {"start": {"agent": "one", "routes": {"success": "_exit"}}}}}
            })
            .to_string(),
        );
        let loader = ConfigLoader::from_file(&config_path).unwrap();
        let error = format!("{:#}", loader.load_workflow_sources().unwrap_err());
        assert!(
            error.contains("require an explicit adc_dir"),
            "unexpected error: {error}"
        );
        assert!(error.contains("stage 'start'"), "unexpected error: {error}");
    }

    struct TempConfig(std::path::PathBuf);
    impl Drop for TempConfig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn stage_agent(schema: &str) -> serde_json::Value {
        json!({
            "id": "one",
            "name": "One",
            "prompt": "prompt.md",
            "schema": schema,
            "timeout": 60
        })
    }

    fn write(path: &std::path::Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Minimal on-disk workflow with one stage and all referenced resources.
    fn temp_workflow() -> (TempConfig, ConfigLoader) {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "tj-config-validate-{}-{unique}",
            std::process::id()
        ));
        let root_guard = TempConfig(root.clone());
        write(
            &root.join("config/fixture.json"),
            &json!({
                "smg_path": "config/graph.json",
                "adc_dir": "config/stages"
            })
            .to_string(),
        );
        write(&root.join("config/graph.json"), &json!({
            "version": 1, "name": "Temp", "entry": "work", "exit": "work",
            "limits": {"maxIterations": 2, "maxSteps": 5, "maxDepth": 2},
            "flow-order": ["work"],
            "flows": {"work": {"type": "flow", "entry": "start", "exit": "start",
                "stages": {"start": {"agent": "config/stages/one/agent.json", "routes": {"success": "_exit"}}}}}
        }).to_string());
        write(
            &root.join("config/stages/one/agent.json"),
            &stage_agent("config/stages/one/schema.json").to_string(),
        );
        write(&root.join("config/stages/one/prompt.md"), "Do the work.");
        write(
            &root.join("config/stages/one/schema.json"),
            &json!({
                "type": "object", "required": ["verdict"],
                "properties": {"verdict": {"type": "string", "enum": ["success"]}}
            })
            .to_string(),
        );
        let loader = ConfigLoader::from_file(&root.join("config/fixture.json")).unwrap();
        (root_guard, loader)
    }

    #[test]
    fn missing_agent_prompt_or_schema_fails_before_execution() {
        let (root, loader) = temp_workflow();
        let smg = loader.load_smg().unwrap();
        loader.validate_smg(&smg).unwrap();

        std::fs::remove_file(root.0.join("config/stages/one/prompt.md")).unwrap();
        let error = format!("{:#}", loader.validate_smg(&smg).unwrap_err());
        assert!(
            error.contains("cannot read prompt") && error.contains("stage 'start'"),
            "{error}"
        );

        write(&root.0.join("config/stages/one/prompt.md"), "Do the work.");
        std::fs::remove_file(root.0.join("config/stages/one/schema.json")).unwrap();
        let error = format!("{:#}", loader.validate_smg(&smg).unwrap_err());
        assert!(
            error.contains("cannot read schema") && error.contains("stage 'start'"),
            "{error}"
        );

        std::fs::remove_file(root.0.join("config/stages/one/agent.json")).unwrap();
        let error = format!("{:#}", loader.validate_smg(&smg).unwrap_err());
        assert!(
            error.contains("ADC file not found") && error.contains("stage 'start'"),
            "{error}"
        );
    }

    #[test]
    fn schema_vocabulary_and_route_enum_mismatches_are_rejected() {
        let (root, loader) = temp_workflow();
        let smg = loader.load_smg().unwrap();
        write(
            &root.0.join("config/stages/one/schema.json"),
            &json!({
                "type": "object", "properties": {
                    "verdict": {"type": "string", "enum": ["success"]},
                    "secret": {"type": "string", "pattern": "^x$"}
                }
            })
            .to_string(),
        );
        let error = format!("{:#}", loader.validate_smg(&smg).unwrap_err());
        assert!(
            error.contains("unsupported schema keyword 'pattern'")
                && error.contains("stage 'start'"),
            "{error}"
        );

        write(
            &root.0.join("config/stages/one/schema.json"),
            &json!({
                "type": "object", "properties": {
                    "verdict": {"type": "string", "enum": ["success", "extra"]}
                }
            })
            .to_string(),
        );
        let error = format!("{:#}", loader.validate_smg(&smg).unwrap_err());
        assert!(
            error.contains("does not exactly match route keys") && error.contains("stage 'start'"),
            "{error}"
        );
    }

    #[test]
    fn artifact_contracts_reject_unknown_sources_and_verification_rules() {
        use crate::adc::{AgentDefinitionConfig, ArtifactDef};
        let names: std::collections::HashSet<&str> = ["start"].into_iter().collect();
        let mut adc: AgentDefinitionConfig =
            serde_json::from_value(json!({"id":"one","name":"One"})).unwrap();
        adc.input.artifacts.push(ArtifactDef {
            name: "packet".into(),
            from: Some("missing-stage".into()),
            required: true,
            verify: None,
        });
        let error =
            super::validate_artifact_contracts("workflow 'x' flow 'y' stage 'start'", &adc, &names)
                .unwrap_err()
                .to_string();
        assert!(
            error.contains("unknown source stage 'missing-stage'"),
            "{error}"
        );

        let mut adc: AgentDefinitionConfig =
            serde_json::from_value(json!({"id":"one","name":"One"})).unwrap();
        adc.output.artifacts.push(ArtifactDef {
            name: "packet".into(),
            from: None,
            required: true,
            verify: Some("checksum".into()),
        });
        let error =
            super::validate_artifact_contracts("workflow 'x' flow 'y' stage 'start'", &adc, &names)
                .unwrap_err()
                .to_string();
        assert!(
            error.contains("unsupported verify rule 'checksum'"),
            "{error}"
        );
    }

    #[test]
    fn schema_items_are_validated_and_unknown_vocabulary_is_rejected() {
        let schema = json!({"type":"array","items":{"type":"string"}});
        validate_json_value(&json!(["valid"]), &schema, "items").unwrap();
        let error = validate_json_value(&json!(["valid", 2]), &schema, "items").unwrap_err();
        assert!(error.to_string().contains("items.1"));

        let unsupported = json!({"type":"object","patternProperties":{}});
        assert!(
            super::validate_schema_document(&unsupported)
                .unwrap_err()
                .to_string()
                .contains("unsupported schema keyword 'patternProperties'")
        );
    }

    static AGENT_ENV_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

    fn agent_config(root: &std::path::Path, agents: serde_json::Value) -> std::path::PathBuf {
        let path = root.join("config/fixture.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, json!({"agents": agents}).to_string()).unwrap();
        path
    }

    #[test]
    fn legacy_chef_id_and_env_are_accepted_with_canonical_chief_identity() {
        let _guard = AGENT_ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = agent_config(
            temp.path(),
            json!([{"id":"chef","provider_id":"p","model":"config-model","default":true}]),
        );
        // Safety: serialized by AGENT_ENV_LOCK; values are test-only.
        unsafe {
            std::env::remove_var("TURBOJET_AGENT_CHIEF_MODEL");
            std::env::set_var("TURBOJET_AGENT_CHEF_MODEL", "legacy-env-model");
            std::env::set_var("TURBOJET_HOME", temp.path());
        }
        let loader = super::ConfigLoader::from_file(&path).unwrap();
        assert_eq!(loader.config().agents[0].id, "chief");
        assert_eq!(loader.config().agents[0].model, "legacy-env-model");
        unsafe {
            std::env::remove_var("TURBOJET_AGENT_CHEF_MODEL");
            std::env::remove_var("TURBOJET_HOME");
        }
    }

    #[test]
    fn canonical_chief_env_wins_and_duplicate_persona_ids_are_rejected() {
        let _guard = AGENT_ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = agent_config(
            temp.path(),
            json!([{"id":"chef","provider_id":"p","model":"config-model","default":true}]),
        );
        // Safety: serialized by AGENT_ENV_LOCK; values are test-only.
        unsafe {
            std::env::set_var("TURBOJET_AGENT_CHIEF_MODEL", "canonical-model");
            std::env::set_var("TURBOJET_AGENT_CHEF_MODEL", "legacy-model");
            std::env::set_var("TURBOJET_HOME", temp.path());
        }
        let loader = super::ConfigLoader::from_file(&path).unwrap();
        assert_eq!(loader.config().agents[0].id, "chief");
        assert_eq!(loader.config().agents[0].model, "canonical-model");

        let duplicate = agent_config(
            temp.path(),
            json!([
                {"id":"chef","model":"a"},
                {"id":"chief","model":"b"}
            ]),
        );
        let error = super::ConfigLoader::from_file(&duplicate)
            .err()
            .expect("duplicate persona ids must be rejected")
            .to_string();
        assert!(error.contains("both configured"), "{error}");
        unsafe {
            std::env::remove_var("TURBOJET_AGENT_CHIEF_MODEL");
            std::env::remove_var("TURBOJET_AGENT_CHEF_MODEL");
            std::env::remove_var("TURBOJET_HOME");
        }
    }
}
