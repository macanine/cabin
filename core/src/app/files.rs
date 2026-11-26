use std::path::PathBuf;

use crate::error::{Error, Result};

use super::App;

impl App {
    // --- Workbench panel: files, git changes, dev-server ports -------------

    fn workbench_root(&self, workspace_id: &str, session_id: Option<&str>) -> Result<PathBuf> {
        let root = self.session_workspace_root(workspace_id, session_id)?;
        if !root.is_dir() {
            return Err(Error::invalid("The workspace folder does not exist"));
        }
        Ok(root)
    }

    pub fn fs_list(
        &self,
        workspace_id: &str,
        session_id: Option<&str>,
        path: &str,
    ) -> Result<crate::model::FsListing> {
        crate::workbench::files::list(&self.workbench_root(workspace_id, session_id)?, path)
    }

    pub fn fs_read(
        &self,
        workspace_id: &str,
        session_id: Option<&str>,
        path: &str,
    ) -> Result<crate::model::FsFile> {
        crate::workbench::files::read(&self.workbench_root(workspace_id, session_id)?, path)
    }

    pub fn git_status(
        &self,
        workspace_id: &str,
        session_id: Option<&str>,
    ) -> Result<crate::model::GitStatus> {
        crate::workbench::git::status(&self.workbench_root(workspace_id, session_id)?)
    }

    pub fn git_diff(
        &self,
        workspace_id: &str,
        session_id: Option<&str>,
        path: &str,
        staged: bool,
    ) -> Result<crate::model::GitFileDiff> {
        crate::workbench::git::diff(
            &self.workbench_root(workspace_id, session_id)?,
            path,
            staged,
        )
    }

    pub fn git_stage(
        &self,
        workspace_id: &str,
        session_id: Option<&str>,
        paths: &[String],
        stage: bool,
    ) -> Result<crate::model::GitStatus> {
        let root = self.workbench_root(workspace_id, session_id)?;
        if stage {
            crate::workbench::git::stage(&root, paths)?;
        } else {
            crate::workbench::git::unstage(&root, paths)?;
        }
        crate::workbench::git::status(&root)
    }

    pub fn git_discard(
        &self,
        workspace_id: &str,
        session_id: Option<&str>,
        paths: &[String],
    ) -> Result<crate::model::GitStatus> {
        let root = self.workbench_root(workspace_id, session_id)?;
        crate::workbench::git::discard(&root, paths)?;
        crate::workbench::git::status(&root)
    }

    pub fn git_commit(
        &self,
        workspace_id: &str,
        session_id: Option<&str>,
        message: &str,
        all: bool,
    ) -> Result<crate::model::GitCommitResult> {
        crate::workbench::git::commit(
            &self.workbench_root(workspace_id, session_id)?,
            message,
            all,
        )
    }

    pub fn listening_ports(&self) -> Vec<crate::model::ListeningPort> {
        crate::workbench::ports::listening()
    }
}
