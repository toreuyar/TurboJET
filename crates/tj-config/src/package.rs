//! Workflow packages: self-contained workflow directories and their immutable
//! registry assets.
//!
//! A package owns `workflow.smg.json`, any `agents/<id>/` definitions and any
//! `schemas/*.json` files. Agent/prompt/schema references resolve strictly
//! inside the package; there is no fallback to the shared `adc_dir`.
//!
//! Import validates every reference before persistence. Once registered, the
//! graph and its assets are the source of truth: run admission reads only the
//! registry rows, so deleting or editing the source directory cannot change a
//! later run.

use crate::bundle::hash_bytes;
use crate::loader::{StageResource, StageResourceReader};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Maximum number of stored assets in one package. The manifest counts.
pub const MAX_PACKAGE_ASSETS: usize = 200;
/// Maximum size of one text asset (manifest, ADC, prompt or schema).
pub const MAX_PACKAGE_ASSET_BYTES: usize = 1024 * 1024;

/// One package asset with its package-root-relative logical path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedWorkflowAsset {
    pub logical_path: String,
    pub content: Vec<u8>,
    pub content_hash: String,
}

/// Immutable package assets keyed by package-root-relative logical path.
#[derive(Debug, Clone, Default)]
pub struct WorkflowAssets {
    entries: BTreeMap<String, Vec<u8>>,
}

impl WorkflowAssets {
    pub fn from_entries<I>(entries: I) -> Result<Self>
    where
        I: IntoIterator<Item = (String, Vec<u8>)>,
    {
        let mut assets = Self::default();
        for (logical_path, content) in entries {
            assets.insert(logical_path, content)?;
        }
        Ok(assets)
    }

    pub fn insert(&mut self, logical_path: String, content: Vec<u8>) -> Result<()> {
        validate_logical_path(&logical_path)?;
        ensure!(
            !self.entries.contains_key(&logical_path),
            "duplicate package asset logical path '{}'",
            logical_path
        );
        ensure!(
            self.entries.len() < MAX_PACKAGE_ASSETS,
            "package has more than {} assets; split it or remove unused files",
            MAX_PACKAGE_ASSETS
        );
        ensure!(
            content.len() <= MAX_PACKAGE_ASSET_BYTES,
            "package asset '{}' is {} bytes; the maximum text-asset size is 1 MiB ({} bytes)",
            logical_path,
            content.len(),
            MAX_PACKAGE_ASSET_BYTES
        );
        ensure!(
            std::str::from_utf8(&content).is_ok(),
            "package asset '{}' must be UTF-8 text",
            logical_path
        );
        self.entries.insert(logical_path, content);
        Ok(())
    }

    pub fn get(&self, logical_path: &str) -> Option<&[u8]> {
        self.entries.get(logical_path).map(Vec::as_slice)
    }

    pub fn contains(&self, logical_path: &str) -> bool {
        self.entries.contains_key(logical_path)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.entries
            .iter()
            .map(|(path, content)| (path.as_str(), content.as_slice()))
    }
}

/// Builder used while importing a package from disk. It records which requested
/// file produced each logical asset so two different files cannot silently
/// collapse onto one logical path.
struct PackageAssets {
    assets: WorkflowAssets,
    requested: BTreeMap<String, PathBuf>,
}

impl PackageAssets {
    fn new() -> Self {
        Self {
            assets: WorkflowAssets::default(),
            requested: BTreeMap::new(),
        }
    }

    fn insert_from(
        &mut self,
        logical_path: String,
        requested: &Path,
        content: Vec<u8>,
    ) -> Result<()> {
        if let Some(previous) = self.requested.get(&logical_path) {
            ensure!(
                previous == requested,
                "duplicate package asset logical path '{}' is provided by both {} and {}",
                logical_path,
                previous.display(),
                requested.display()
            );
            ensure!(
                self.assets.get(&logical_path) == Some(content.as_slice()),
                "package asset '{}' changed while it was being read",
                logical_path
            );
            return Ok(());
        }
        self.assets.insert(logical_path.clone(), content)?;
        self.requested.insert(logical_path, requested.to_path_buf());
        Ok(())
    }

    fn into_loaded(self) -> Vec<LoadedWorkflowAsset> {
        self.assets
            .entries
            .into_iter()
            .map(|(logical_path, content)| LoadedWorkflowAsset {
                logical_path,
                content_hash: hash_bytes(&content),
                content,
            })
            .collect()
    }
}

/// Reads a package from disk while recording every referenced asset. This is
/// import-time only; admission uses [`StoredAssetResources`].
pub(crate) struct PackageResourceReader<'a> {
    root: &'a Path,
    assets: PackageAssets,
}

impl<'a> PackageResourceReader<'a> {
    pub(crate) fn new(root: &'a Path) -> Self {
        Self {
            root,
            assets: PackageAssets::new(),
        }
    }

    pub(crate) fn insert_manifest(&mut self, content: Vec<u8>) -> Result<()> {
        let requested = self.root.join("workflow.smg.json");
        self.assets
            .insert_from("workflow.smg.json".into(), &requested, content)
    }

    pub(crate) fn into_loaded_assets(self) -> Vec<LoadedWorkflowAsset> {
        self.assets.into_loaded()
    }

    fn checked_file(&self, logical_path: &str) -> Result<PathBuf> {
        validate_logical_path(logical_path)?;
        let requested = self.root.join(logical_path);
        let canonical = std::fs::canonicalize(&requested).with_context(|| {
            format!(
                "package asset '{}' is missing at {}",
                logical_path,
                requested.display()
            )
        })?;
        ensure!(
            canonical.starts_with(self.root),
            "package asset '{}' escapes the package directory {}",
            logical_path,
            self.root.display()
        );
        ensure!(
            canonical == requested,
            "package asset '{}' must be named directly; it resolves through a symlink or non-canonical path",
            logical_path
        );
        ensure!(
            canonical.is_file(),
            "package asset '{}' is not a regular file",
            logical_path
        );
        Ok(canonical)
    }

    fn optional_file(&self, logical_path: &str) -> Result<Option<PathBuf>> {
        validate_logical_path(logical_path)?;
        let requested = self.root.join(logical_path);
        match std::fs::canonicalize(&requested) {
            Ok(canonical) => {
                ensure!(
                    canonical.starts_with(self.root),
                    "package asset '{}' escapes the package directory {}",
                    logical_path,
                    self.root.display()
                );
                ensure!(
                    canonical == requested,
                    "package asset '{}' must be named directly; it resolves through a symlink or non-canonical path",
                    logical_path
                );
                ensure!(
                    canonical.is_file(),
                    "package asset '{}' is not a regular file",
                    logical_path
                );
                Ok(Some(canonical))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "cannot inspect package asset '{}' at {}",
                    logical_path,
                    requested.display()
                )
            }),
        }
    }

    fn read_file(&mut self, logical_path: String) -> Result<StageResource> {
        let path = self.checked_file(&logical_path)?;
        let content = std::fs::read(&path)
            .with_context(|| format!("cannot read package asset '{}'", path.display()))?;
        self.assets
            .insert_from(logical_path.clone(), &path, content.clone())?;
        Ok(StageResource {
            path: logical_path,
            bytes: content,
        })
    }

    fn read_schema_candidate(&mut self, adc_path: &str, schema: &str) -> Result<StageResource> {
        let candidates = schema_logical_candidates(adc_path, schema)?;
        for candidate in &candidates {
            if self.optional_file(candidate)?.is_some() {
                return self.read_file(candidate.clone());
            }
        }
        anyhow::bail!(
            "schema '{}' is missing from package; searched {}",
            schema,
            candidates.join(", ")
        )
    }
}

impl StageResourceReader for PackageResourceReader<'_> {
    fn read_adc(&mut self, _stage_id: &str, agent_ref: &str) -> Result<StageResource> {
        let logical_path = agent_logical_path(agent_ref)?;
        self.read_file(logical_path)
    }

    fn read_prompt(&mut self, adc: &StageResource, prompt: &str) -> Result<StageResource> {
        let logical_path = prompt_logical_path(&adc.path, prompt)?;
        self.read_file(logical_path)
    }

    fn read_schema(&mut self, adc: &StageResource, schema: &str) -> Result<StageResource> {
        self.read_schema_candidate(&adc.path, schema)
    }

    fn read_status_schema(&mut self, adc: &StageResource, schema: &str) -> Result<StageResource> {
        self.read_schema_candidate(&adc.path, schema)
    }
}

/// Reads package resources from already-imported registry assets. No filesystem
/// path is consulted. A missing row fails closed instead of falling back to the
/// source directory or the shared ADC directory.
pub struct StoredAssetResources<'a> {
    assets: &'a WorkflowAssets,
}

impl<'a> StoredAssetResources<'a> {
    pub fn new(assets: &'a WorkflowAssets) -> Self {
        Self { assets }
    }

    fn get(&self, logical_path: &str) -> Result<StageResource> {
        let content = self.assets.get(logical_path).ok_or_else(|| {
            anyhow::anyhow!(
                "package asset '{}' is missing from the workflow registry; re-import the package source",
                logical_path
            )
        })?;
        Ok(StageResource {
            path: logical_path.to_string(),
            bytes: content.to_vec(),
        })
    }

    fn schema_candidate(&self, adc_path: &str, schema: &str) -> Result<StageResource> {
        let candidates = schema_logical_candidates(adc_path, schema)?;
        for candidate in &candidates {
            if self.assets.contains(candidate) {
                return self.get(candidate);
            }
        }
        anyhow::bail!(
            "schema '{}' is missing from the workflow registry; searched stored assets {}",
            schema,
            candidates.join(", ")
        )
    }
}

impl StageResourceReader for StoredAssetResources<'_> {
    fn read_adc(&mut self, _stage_id: &str, agent_ref: &str) -> Result<StageResource> {
        self.get(&agent_logical_path(agent_ref)?)
    }

    fn read_prompt(&mut self, adc: &StageResource, prompt: &str) -> Result<StageResource> {
        self.get(&prompt_logical_path(&adc.path, prompt)?)
    }

    fn read_schema(&mut self, adc: &StageResource, schema: &str) -> Result<StageResource> {
        self.schema_candidate(&adc.path, schema)
    }

    fn read_status_schema(&mut self, adc: &StageResource, schema: &str) -> Result<StageResource> {
        self.schema_candidate(&adc.path, schema)
    }
}

/// Reject a logical path that is absolute, contains `..`, or is not already
/// normalized. Package assets always use `/` separators and never escape.
pub fn validate_logical_path(path: &str) -> Result<()> {
    ensure!(
        !path.trim().is_empty(),
        "package asset path must not be empty"
    );
    ensure!(
        !path.contains('\\'),
        "package asset '{}' must use '/' separators",
        path
    );
    ensure!(
        !Path::new(path).is_absolute(),
        "package asset '{}' must be relative, not absolute",
        path
    );
    for component in Path::new(path).components() {
        ensure!(
            matches!(component, Component::Normal(_)),
            "package asset '{}' must not contain '.', '..' or a root component",
            path
        );
    }
    Ok(())
}

fn normalize_relative(path: &str) -> Result<String> {
    validate_logical_path(path)?;
    Ok(path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/"))
}

/// Resolve a package graph's `agent` reference to a logical path. A bare id
/// means `agents/<id>/agent.json`; a value containing `/` is the exact
/// package-relative file.
pub fn agent_logical_path(agent_ref: &str) -> Result<String> {
    ensure!(
        !agent_ref.trim().is_empty(),
        "agent reference must not be empty"
    );
    ensure!(
        !Path::new(agent_ref).is_absolute(),
        "agent reference '{}' must be package-relative, not absolute",
        agent_ref
    );
    if agent_ref.contains('/') {
        normalize_relative(agent_ref).with_context(|| format!("agent reference '{}'", agent_ref))
    } else {
        ensure!(
            agent_ref != "." && agent_ref != "..",
            "agent reference '{}' must name an agent id or a package-relative file",
            agent_ref
        );
        normalize_relative(&format!("agents/{agent_ref}/agent.json"))
            .with_context(|| format!("agent reference '{}'", agent_ref))
    }
}

/// Resolve an ADC `prompt`. A bare filename is relative to the ADC's directory;
/// a value containing `/` is package-root-relative.
pub fn prompt_logical_path(adc_path: &str, prompt: &str) -> Result<String> {
    ensure!(!prompt.trim().is_empty(), "ADC prompt path is required");
    ensure!(
        !Path::new(prompt).is_absolute(),
        "prompt '{}' must be package-relative, not absolute",
        prompt
    );
    if prompt.contains('/') {
        normalize_relative(prompt).with_context(|| format!("prompt '{}'", prompt))
    } else {
        let parent = Path::new(adc_path)
            .parent()
            .with_context(|| format!("ADC path '{}' has no directory", adc_path))?;
        normalize_relative(&format!("{}/{}", parent.to_string_lossy(), prompt))
            .with_context(|| format!("prompt '{}'", prompt))
    }
}

/// Resolve an ADC `schema`: package-root-relative first, then ADC-dir-relative.
/// Both candidates are confined to the package.
pub fn schema_logical_candidates(adc_path: &str, schema: &str) -> Result<Vec<String>> {
    ensure!(
        !schema.trim().is_empty(),
        "ADC output schema path is required"
    );
    ensure!(
        !Path::new(schema).is_absolute(),
        "schema '{}' must be package-relative, not absolute",
        schema
    );
    let root = normalize_relative(schema).with_context(|| format!("schema '{}'", schema))?;
    let parent = Path::new(adc_path)
        .parent()
        .with_context(|| format!("ADC path '{}' has no directory", adc_path))?;
    let adc_relative = normalize_relative(&format!("{}/{}", parent.to_string_lossy(), schema))
        .with_context(|| format!("schema '{}'", schema))?;
    if root == adc_relative {
        Ok(vec![root])
    } else {
        Ok(vec![root, adc_relative])
    }
}
