use std::path::{Path, PathBuf};

use anyhow::Result;

/// Validates that file paths stay within a workspace boundary.
/// Canonicalizes paths to defeat symlink-based escapes.
pub struct PathValidator {
    workspace_root: PathBuf,
}

#[derive(Debug)]
pub enum PathValidationError {
    EmptyPath,
    WorkspaceNotFound(PathBuf, std::io::Error),
    CanonicalizeFailed(PathBuf, std::io::Error),
    PathOutsideWorkspace {
        requested: PathBuf,
        workspace: PathBuf,
    },
    ParentNotFound(PathBuf),
}

impl std::fmt::Display for PathValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPath => write!(f, "Path cannot be empty"),
            Self::WorkspaceNotFound(p, e) => {
                write!(f, "Workspace root does not exist: {}: {}", p.display(), e)
            }
            Self::CanonicalizeFailed(p, e) => {
                write!(f, "Failed to canonicalize path: {}: {}", p.display(), e)
            }
            Self::PathOutsideWorkspace {
                requested,
                workspace,
            } => {
                write!(
                    f,
                    "Path '{}' is outside the workspace '{}'",
                    requested.display(),
                    workspace.display()
                )
            }
            Self::ParentNotFound(p) => {
                write!(f, "Parent directory does not exist: {}", p.display())
            }
        }
    }
}

impl std::error::Error for PathValidationError {}

impl PathValidator {
    /// Create a new validator rooted at the given workspace directory.
    /// Canonicalizes the workspace root eagerly, returning an error if it does not exist.
    pub fn new(workspace: PathBuf) -> Result<Self, PathValidationError> {
        let canonical = workspace
            .canonicalize()
            .map_err(|e| PathValidationError::WorkspaceNotFound(workspace, e))?;
        Ok(Self {
            workspace_root: canonical,
        })
    }

    /// Validate that `relative_path` resolves to a location inside the workspace.
    ///
    /// - If the target exists, it is canonicalized and checked.
    /// - If the target does not exist but its parent does, the parent is canonicalized
    ///   and the filename is appended, then checked.
    /// - Otherwise returns `ParentNotFound`.
    pub fn validate(&self, relative_path: &str) -> Result<PathBuf, PathValidationError> {
        tracing::debug!(
            path = %relative_path,
            workspace = %self.workspace_root.display(),
            "Validating path against workspace boundary"
        );

        if relative_path.is_empty() {
            tracing::debug!("Path validation failed: empty path");
            return Err(PathValidationError::EmptyPath);
        }

        let joined = self.workspace_root.join(relative_path);

        if joined.exists() {
            let canonical = joined
                .canonicalize()
                .map_err(|e| PathValidationError::CanonicalizeFailed(joined, e))?;
            tracing::debug!(
                canonical = %canonical.display(),
                "Path exists, canonicalized"
            );
            if canonical.starts_with(&self.workspace_root) {
                tracing::debug!("Path validation passed");
                Ok(canonical)
            } else {
                tracing::debug!(
                    canonical = %canonical.display(),
                    "Path validation failed: escapes workspace"
                );
                Err(PathValidationError::PathOutsideWorkspace {
                    requested: canonical,
                    workspace: self.workspace_root.clone(),
                })
            }
        } else {
            let parent = joined
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_path_buf());

            if let Some(parent) = parent {
                if parent.exists() {
                    let canonical_parent = parent
                        .canonicalize()
                        .map_err(|e| PathValidationError::CanonicalizeFailed(parent.clone(), e))?;
                    let file_name = joined.file_name().ok_or(PathValidationError::EmptyPath)?;
                    let full_path = canonical_parent.join(file_name);
                    tracing::debug!(
                        full_path = %full_path.display(),
                        "Path does not exist, parent canonicalized"
                    );
                    if full_path.starts_with(&self.workspace_root) {
                        tracing::debug!("Path validation passed");
                        Ok(full_path)
                    } else {
                        tracing::debug!(
                            full_path = %full_path.display(),
                            "Path validation failed: escapes workspace via parent"
                        );
                        Err(PathValidationError::PathOutsideWorkspace {
                            requested: full_path,
                            workspace: self.workspace_root.clone(),
                        })
                    }
                } else {
                    tracing::debug!(
                        parent = %parent.display(),
                        "Path validation failed: parent directory does not exist"
                    );
                    Err(PathValidationError::ParentNotFound(parent))
                }
            } else {
                tracing::debug!("Path validation failed: no parent directory component");
                Err(PathValidationError::ParentNotFound(joined))
            }
        }
    }

    /// Return the canonical workspace root.
    pub fn workspace(&self) -> &Path {
        &self.workspace_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_workspace() -> TempDir {
        let dir = TempDir::new().expect("failed to create temp dir");
        std::fs::create_dir_all(dir.path().join("src")).expect("failed to create src/");
        std::fs::write(dir.path().join("src/lib.rs"), "// lib").expect("failed to write lib.rs");
        dir
    }

    #[test]
    fn test_valid_relative_path() {
        let ws = setup_workspace();
        let validator = PathValidator::new(ws.path().to_path_buf()).expect("validator creation");
        let result = validator.validate("src/lib.rs").expect("validation");
        assert!(result.starts_with(ws.path()));
        assert!(result.ends_with("src/lib.rs"));
    }

    #[test]
    fn test_path_escape_via_dotdot() {
        let root = TempDir::new().expect("test root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(root.path().join("outside.txt"), "outside workspace").unwrap();
        let validator = PathValidator::new(workspace).expect("validator creation");
        let err = validator.validate("../outside.txt").unwrap_err();
        assert!(
            matches!(err, PathValidationError::PathOutsideWorkspace { .. }),
            "expected PathOutsideWorkspace, got: {err}"
        );
    }

    #[test]
    fn test_empty_path_rejected() {
        let ws = setup_workspace();
        let validator = PathValidator::new(ws.path().to_path_buf()).expect("validator creation");
        let err = validator.validate("").unwrap_err();
        assert!(
            matches!(err, PathValidationError::EmptyPath),
            "expected EmptyPath, got: {err}"
        );
    }

    #[test]
    fn test_nonexistent_file_in_valid_dir() {
        let ws = setup_workspace();
        let validator = PathValidator::new(ws.path().to_path_buf()).expect("validator creation");
        let result = validator.validate("src/newfile.rs").expect("validation");
        assert!(result.starts_with(ws.path()));
        assert!(result.ends_with("src/newfile.rs"));
    }

    #[test]
    fn test_nonexistent_dir_rejected() {
        let ws = setup_workspace();
        let validator = PathValidator::new(ws.path().to_path_buf()).expect("validator creation");
        let err = validator.validate("nonexistent_dir/file.rs").unwrap_err();
        assert!(
            matches!(err, PathValidationError::ParentNotFound(_)),
            "expected ParentNotFound, got: {err}"
        );
    }
}
