use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub claude_root: PathBuf,
    pub codex_root: PathBuf,
    pub data_root: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let home = env::var_os("HOME").map(PathBuf::from);
        let claude_root = env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|path| path.join(".claude")))
            .context("HOME or CLAUDE_CONFIG_DIR must be set")?;
        let codex_root = env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|path| path.join(".codex")))
            .context("HOME or CODEX_HOME must be set")?;
        let data_root = env::var_os("DREDGE_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .map(|path| path.join("dredge"))
            })
            .or_else(|| {
                home.as_ref()
                    .map(|path| path.join(".local").join("share").join("dredge"))
            })
            .context("HOME, XDG_DATA_HOME, or DREDGE_DATA_DIR must be set")?;
        Ok(Self {
            claude_root,
            codex_root,
            data_root,
        })
    }

    pub fn index_dir(&self) -> PathBuf {
        self.data_root.join("index")
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.data_root.join("manifest.json")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.data_root.join("index.lock")
    }

    pub fn create_private_dir(path: &Path) -> Result<()> {
        std::fs::create_dir_all(path)
            .with_context(|| format!("create data directory {}", path.display()))?;
        set_mode(path, 0o700)?;
        Ok(())
    }

    pub fn ensure_safe_data_root(&self) -> Result<()> {
        if self.data_root.as_os_str().is_empty() || self.data_root == Path::new("/") {
            bail!("refusing unsafe Dredge data path")
        }
        Self::create_private_dir(&self.data_root)
    }
}

#[cfg(unix)]
pub fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("set private permissions on {}", path.display()))
}

#[cfg(not(unix))]
pub fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}
