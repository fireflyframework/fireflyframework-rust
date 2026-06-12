// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Git-backed [`ConfigBackend`] — wraps a local or remote Git repository.
//!
//! The Rust port of pyfly's `pyfly.config_server.adapters.git`. pyfly
//! drives GitPython; this port shells out to the system `git` binary
//! (no extra crate, no libgit2). Config files are read from the working
//! tree of a cloned (or locally reused) repository; writes are committed
//! locally. Pushing to a remote is **out of scope** — call
//! [`GitStore::refresh`] after a remote push to pull the latest commits.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use async_trait::async_trait;

use super::{BackendError, ConfigBackend, ConfigSource, FsStore};

/// A [`ConfigBackend`] backed by a Git repository.
///
/// On first use the repository is cloned (or, for a local path / `file://`
/// URI, the on-disk repo is reused) into `clone_dir` (or an OS temp
/// directory when `clone_dir` is `None`). The working tree is then
/// delegated to an [`FsStore`] so all file-search and merge logic is
/// shared.
///
/// Construction is cheap and never touches the network; the clone happens
/// lazily on the first [`fetch`](GitStore::fetch) / [`save`](GitStore::save)
/// / [`list`](GitStore::list) / [`refresh`](GitStore::refresh) call. A
/// `Mutex` guards the lazy-init path so two concurrent first-callers
/// cannot both attempt to clone into the same directory.
///
/// # Example
///
/// ```no_run
/// # async fn run() -> Result<(), firefly_config_server::BackendError> {
/// use firefly_config_server::{ConfigBackend, GitStore};
///
/// let backend = GitStore::new("/path/to/repo")
///     .label("main")
///     .clone_dir("/var/lib/firefly/config-clone");
/// let source = backend.fetch("orders", "prod", "main").await?;
/// # let _ = source;
/// # Ok(())
/// # }
/// ```
pub struct GitStore {
    uri: String,
    label: String,
    clone_dir: Option<PathBuf>,
    state: Mutex<Option<GitState>>,
}

/// The lazily-initialised state: the resolved working tree and its
/// (optionally owned) temp directory guard.
struct GitState {
    fs: FsStore,
    work_dir: PathBuf,
    /// Kept alive so an auto-created temp dir is removed on drop; `None`
    /// when the caller supplied an explicit `clone_dir`.
    _temp: Option<tempfile::TempDir>,
}

impl std::fmt::Debug for GitStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitStore")
            .field("uri", &self.uri)
            .field("label", &self.label)
            .field("clone_dir", &self.clone_dir)
            .field("initialised", &self.state.lock().unwrap().is_some())
            .finish()
    }
}

impl GitStore {
    /// Creates a [`GitStore`] for `uri` (any URI `git clone` accepts: an
    /// `https://`/`git@` remote, or a local path / `file://` URI),
    /// checking out the `"main"` label and cloning into an OS temp
    /// directory. Use [`label`](GitStore::label) /
    /// [`clone_dir`](GitStore::clone_dir) to customise.
    ///
    /// No network access happens until the first backend operation.
    pub fn new(uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            label: "main".to_string(),
            clone_dir: None,
            state: Mutex::new(None),
        }
    }

    /// Sets the branch / tag / SHA to check out (default `"main"`).
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Sets an explicit clone directory.
    ///
    /// **In production it is strongly recommended to pass an explicit
    /// `clone_dir`** so the clone persists across restarts and the
    /// location can be managed by the operator. When unset, a temporary
    /// directory is created and removed when this [`GitStore`] is dropped.
    #[must_use]
    pub fn clone_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.clone_dir = Some(dir.into());
        self
    }

    /// Returns the cached [`FsStore`], cloning on the first call.
    fn ensure_repo(&self) -> Result<(), BackendError> {
        let mut guard = self.state.lock().expect("GitStore lock poisoned");
        if guard.is_some() {
            return Ok(());
        }

        let (work_dir, temp): (PathBuf, Option<tempfile::TempDir>) = match &self.clone_dir {
            Some(dir) => (dir.clone(), None),
            None => {
                let td = tempfile::Builder::new()
                    .prefix("firefly-git-config-")
                    .tempdir()
                    .map_err(|e| BackendError::Io(e.to_string()))?;
                (td.path().to_path_buf(), Some(td))
            }
        };

        clone_into(&self.uri, &work_dir)?;
        // Best-effort checkout: a missing branch is a warning in pyfly,
        // not an error, so we ignore checkout failures.
        let _ = run_git(&work_dir, &["checkout", &self.label]);

        let fs = FsStore::new(work_dir.clone())?;
        *guard = Some(GitState {
            fs,
            work_dir,
            _temp: temp,
        });
        Ok(())
    }

    /// Pulls the latest commits from `origin` (a no-op when the clone has
    /// no remote, e.g. a clone of a local path that was later detached).
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::Git`] if the `git pull` subprocess fails.
    pub async fn refresh(&self) -> Result<(), BackendError> {
        self.ensure_repo()?;
        let work_dir = {
            let guard = self.state.lock().expect("GitStore lock poisoned");
            guard
                .as_ref()
                .expect("ensure_repo set state")
                .work_dir
                .clone()
        };
        // No remote → nothing to pull, mirroring pyfly's graceful skip.
        let remotes = run_git(&work_dir, &["remote"])?;
        if remotes.trim().is_empty() {
            return Ok(());
        }
        run_git(&work_dir, &["pull", "origin", &self.label])?;
        Ok(())
    }

    /// Stages all changes and creates a local commit, returning quietly
    /// when there is nothing to commit (matching pyfly).
    fn commit(work_dir: &Path, message: &str) -> Result<(), BackendError> {
        run_git(work_dir, &["add", "-A"])?;
        // `git diff --cached --quiet` exits 1 when there are staged
        // changes; 0 means the tree is clean → nothing to commit.
        if git_status_clean(work_dir)? {
            return Ok(());
        }
        run_git(work_dir, &["commit", "-m", message])?;
        Ok(())
    }
}

#[async_trait]
impl ConfigBackend for GitStore {
    async fn fetch(
        &self,
        application: &str,
        profile: &str,
        label: &str,
    ) -> Result<Option<ConfigSource>, BackendError> {
        self.ensure_repo()?;
        let fs = {
            let guard = self.state.lock().expect("GitStore lock poisoned");
            guard.as_ref().expect("ensure_repo set state").fs.clone()
        };
        fs.fetch(application, profile, label).await
    }

    async fn save(&self, source: ConfigSource) -> Result<(), BackendError> {
        self.ensure_repo()?;
        let (fs, work_dir) = {
            let guard = self.state.lock().expect("GitStore lock poisoned");
            let state = guard.as_ref().expect("ensure_repo set state");
            (state.fs.clone(), state.work_dir.clone())
        };
        let message = format!(
            "firefly: update {}/{}@{}",
            source.application, source.profile, source.label
        );
        fs.save(source).await?;
        Self::commit(&work_dir, &message)?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ConfigSource>, BackendError> {
        self.ensure_repo()?;
        let fs = {
            let guard = self.state.lock().expect("GitStore lock poisoned");
            guard.as_ref().expect("ensure_repo set state").fs.clone()
        };
        fs.list().await
    }
}

/// Clones `uri` into `work_dir`. When `work_dir` already contains a Git
/// repository (e.g. a persistent `clone_dir` reused across restarts), the
/// clone is skipped so existing local commits survive.
fn clone_into(uri: &str, work_dir: &Path) -> Result<(), BackendError> {
    if work_dir.join(".git").is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(work_dir).map_err(|e| BackendError::Io(e.to_string()))?;
    // `git clone <uri> <dir>` — `<dir>` may already exist as long as it
    // is empty, which create_dir_all guarantees for a fresh path.
    let work = work_dir.to_string_lossy();
    let output = Command::new("git")
        .args(["clone", uri, work.as_ref()])
        .output()
        .map_err(|e| BackendError::Git(format!("failed to spawn git: {e}")))?;
    if !output.status.success() {
        return Err(BackendError::Git(format!(
            "git clone failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// Runs `git <args>` inside `work_dir`, returning captured stdout.
///
/// Identity is forced via `-c user.name`/`-c user.email` so commits
/// succeed in CI environments without a configured global identity —
/// the Rust analogue of pyfly's per-repo `config_writer()` setup.
fn run_git(work_dir: &Path, args: &[&str]) -> Result<String, BackendError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(work_dir)
        .args([
            "-c",
            "user.name=Firefly Config Server",
            "-c",
            "user.email=config-server@firefly.dev",
        ])
        .args(args)
        .output()
        .map_err(|e| BackendError::Git(format!("failed to spawn git: {e}")))?;
    if !output.status.success() {
        return Err(BackendError::Git(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Returns `true` when the working tree has no staged or untracked
/// changes — i.e. there is nothing to commit.
fn git_status_clean(work_dir: &Path) -> Result<bool, BackendError> {
    let status = run_git(work_dir, &["status", "--porcelain"])?;
    Ok(status.trim().is_empty())
}
