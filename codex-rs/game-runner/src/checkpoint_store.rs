use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use crate::CampaignCheckpoint;
use crate::CheckpointValidationError;
use crate::DurableCampaignState;
use crate::DurableMutationResult;
use crate::MAX_CHECKPOINT_BYTES;
use crate::PauseReason;
use crate::RunnerDeployment;

const CHECKPOINT_FILE: &str = "campaign.json";
const LOCK_FILE: &str = "campaign.lock";
const STORE_DIRECTORY: &str = "game-runner";

#[derive(Debug, thiserror::Error)]
pub enum CheckpointStoreError {
    #[error(transparent)]
    Validation(#[from] CheckpointValidationError),
    #[error("checkpoint {operation} failed at {path}", path = path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("checkpoint durability is uncertain after {operation} at {path}", path = path.display())]
    DurabilityUncertain {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("campaign checkpoint deployment does not match this runner")]
    DeploymentMismatch,
    #[error("another game runner holds the campaign lock at {path}", path = path.display())]
    AlreadyLocked { path: PathBuf },
}

/// A newly created checkpoint file that can cross its file-level durability barrier.
trait DurableCheckpointTemp: Write + Send {
    fn sync_all(&self) -> io::Result<()>;
}

/// Filesystem primitives whose ordering defines checkpoint durability.
trait DurableCheckpointFs: Send + Sync {
    fn acquire_lock(&self, path: &Path) -> io::Result<Box<dyn Send>>;
    fn read_limited(&self, path: &Path, max_bytes: usize) -> io::Result<Option<Vec<u8>>>;
    fn reject_symlink(&self, path: &Path) -> io::Result<()>;
    fn create_temp(&self, path: &Path) -> io::Result<Box<dyn DurableCheckpointTemp>>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn sync_directory(&self, path: &Path) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<bool>;
}

pub struct CampaignCheckpointStore {
    root: PathBuf,
    path: PathBuf,
    filesystem: Arc<dyn DurableCheckpointFs>,
}

pub struct CampaignStoreGuard {
    _lock: Box<dyn Send>,
}

struct LocalCheckpointFs;

struct LocalCheckpointTemp(File);

impl Write for LocalCheckpointTemp {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl DurableCheckpointTemp for LocalCheckpointTemp {
    fn sync_all(&self) -> io::Result<()> {
        self.0.sync_all()
    }
}

impl DurableCheckpointFs for LocalCheckpointFs {
    fn acquire_lock(&self, path: &Path) -> io::Result<Box<dyn Send>> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Box::new(file)),
            Err(std::fs::TryLockError::WouldBlock) => {
                Err(io::Error::from(io::ErrorKind::WouldBlock))
            }
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }

    fn read_limited(&self, path: &Path, max_bytes: usize) -> io::Result<Option<Vec<u8>>> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() > max_bytes as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "campaign checkpoint exceeds its byte limit",
            ));
        }
        let mut bytes = Vec::new();
        file.take(max_bytes as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "campaign checkpoint grew beyond its byte limit",
            ));
        }
        Ok(Some(bytes))
    }

    fn reject_symlink(&self, path: &Path) -> io::Result<()> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "symbolic links are forbidden for campaign state",
            )),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn create_temp(&self, path: &Path) -> io::Result<Box<dyn DurableCheckpointTemp>> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(path)
            .map(LocalCheckpointTemp)
            .map(|temporary| Box::new(temporary) as Box<dyn DurableCheckpointTemp>)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn sync_directory(&self, path: &Path) -> io::Result<()> {
        File::open(path)?.sync_all()
    }

    fn remove_file(&self, path: &Path) -> io::Result<bool> {
        match fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

impl CampaignCheckpointStore {
    pub fn open(codex_home: &Path) -> Result<(Self, CampaignStoreGuard), CheckpointStoreError> {
        let filesystem: Arc<dyn DurableCheckpointFs> = Arc::new(LocalCheckpointFs);
        let root = codex_home.join(STORE_DIRECTORY);
        filesystem
            .reject_symlink(&root)
            .map_err(|source| io_error("initialize", &root, source))?;
        create_store_directory(&root)?;
        let lock_path = root.join(LOCK_FILE);
        filesystem
            .reject_symlink(&lock_path)
            .map_err(|source| io_error("inspect lock", &lock_path, source))?;
        let lock = filesystem.acquire_lock(&lock_path).map_err(|source| {
            if source.kind() == io::ErrorKind::WouldBlock {
                CheckpointStoreError::AlreadyLocked {
                    path: lock_path.clone(),
                }
            } else {
                io_error("acquire lock", &lock_path, source)
            }
        })?;
        Ok((
            Self::from_parts(root, filesystem),
            CampaignStoreGuard { _lock: lock },
        ))
    }

    fn from_parts(root: PathBuf, filesystem: Arc<dyn DurableCheckpointFs>) -> Self {
        let path = root.join(CHECKPOINT_FILE);
        Self {
            root,
            path,
            filesystem,
        }
    }

    pub fn replace(&self, checkpoint: &CampaignCheckpoint) -> Result<(), CheckpointStoreError> {
        let bytes = checkpoint.encode()?;
        let path = self.path();
        self.filesystem
            .reject_symlink(path)
            .map_err(|source| io_error("inspect", path, source))?;
        let temporary_path = self
            .root
            .join(format!(".{CHECKPOINT_FILE}.{}.tmp", uuid::Uuid::new_v4()));
        self.filesystem
            .reject_symlink(&temporary_path)
            .map_err(|source| io_error("inspect temporary file", &temporary_path, source))?;
        let mut temporary = self
            .filesystem
            .create_temp(&temporary_path)
            .map_err(|source| io_error("create temporary file", &temporary_path, source))?;
        if let Err(source) = temporary.write_all(&bytes) {
            drop(temporary);
            let _ = self.filesystem.remove_file(&temporary_path);
            return Err(io_error("write temporary file", &temporary_path, source));
        }
        if let Err(source) = temporary.sync_all() {
            drop(temporary);
            let _ = self.filesystem.remove_file(&temporary_path);
            return Err(io_error("sync temporary file", &temporary_path, source));
        }
        drop(temporary);
        if let Err(source) = self.filesystem.rename(&temporary_path, path) {
            let _ = self.filesystem.remove_file(&temporary_path);
            return Err(io_error("replace", path, source));
        }
        self.filesystem
            .sync_directory(&self.root)
            .map_err(|source| CheckpointStoreError::DurabilityUncertain {
                operation: "directory sync",
                path: self.root.clone(),
                source,
            })?;
        Ok(())
    }

    pub fn load_and_normalize(
        &self,
        deployment: &RunnerDeployment,
    ) -> Result<Option<CampaignCheckpoint>, CheckpointStoreError> {
        self.filesystem
            .reject_symlink(self.path())
            .map_err(|source| io_error("inspect", self.path(), source))?;
        let Some(encoded) = self
            .filesystem
            .read_limited(self.path(), MAX_CHECKPOINT_BYTES)
            .map_err(|source| io_error("read", self.path(), source))?
        else {
            return Ok(None);
        };
        let mut checkpoint = CampaignCheckpoint::decode(&encoded)?;
        if checkpoint.deployment.helper_app != deployment.helper_app
            || checkpoint.deployment.socket_path != deployment.socket_path
            || checkpoint.deployment.target_app != deployment.target_app
        {
            return Err(CheckpointStoreError::DeploymentMismatch);
        }
        if checkpoint.state == DurableCampaignState::Running {
            checkpoint.state = DurableCampaignState::Paused {
                reason: PauseReason::UnexpectedExit,
            };
            if let Some(mutation) = &mut checkpoint.unresolved_mutation
                && mutation.result == DurableMutationResult::Pending
            {
                mutation.result = DurableMutationResult::Indeterminate;
            }
            self.replace(&checkpoint)?;
        }
        Ok(Some(checkpoint))
    }

    pub fn remove(&self) -> Result<(), CheckpointStoreError> {
        self.filesystem
            .reject_symlink(self.path())
            .map_err(|source| io_error("inspect", self.path(), source))?;
        let removed = self
            .filesystem
            .remove_file(self.path())
            .map_err(|source| io_error("remove", self.path(), source))?;
        if removed {
            self.filesystem
                .sync_directory(&self.root)
                .map_err(|source| CheckpointStoreError::DurabilityUncertain {
                    operation: "removal directory sync",
                    path: self.root.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn create_store_directory(root: &Path) -> Result<(), CheckpointStoreError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && root.is_dir() => Ok(()),
        Err(source) => Err(io_error("create store directory", root, source)),
    }
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> CheckpointStoreError {
    CheckpointStoreError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
#[path = "checkpoint_store_tests.rs"]
mod tests;
