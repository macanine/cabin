use crate::error::{Error, Result};
use crate::model::Workspace;
use crate::util::{clean_name, new_id, now_millis};

use super::{App, WorkspacePatch, expand_home};

impl App {
    fn resolve_folder(&self, raw: &str) -> Result<String> {
        if raw.trim().is_empty() {
            return Err(Error::invalid("Choose a folder for the workspace"));
        }
        let path = expand_home(raw, self.ctx.paths.home());
        if !path.is_absolute() {
            return Err(Error::invalid("Use an absolute folder path"));
        }
        if path.exists() && !path.is_dir() {
            return Err(Error::invalid(format!(
                "{} is a file, not a folder",
                path.display()
            )));
        }
        crate::fsx::ensure_dir(&path)?;
        Ok(path.display().to_string())
    }

    pub fn create_workspace(&self, name: &str, path: &str) -> Result<Workspace> {
        let now = now_millis();
        let workspace = Workspace {
            id: new_id("ws"),
            name: clean_name(name, "workspace")?,
            path: self.resolve_folder(path)?,
            model_overrides: Vec::new(),
            archived: false,
            created_at: now,
            updated_at: now,
        };
        self.ctx.store.insert_workspace(&workspace)?;
        self.emit_library()?;
        Ok(workspace)
    }

    pub fn update_workspace(&self, id: &str, patch: WorkspacePatch) -> Result<Workspace> {
        let mut workspace = self.ctx.store.workspace(id)?;
        if let Some(name) = patch.name {
            workspace.name = clean_name(&name, "workspace")?;
        }
        if let Some(path) = patch.path {
            workspace.path = self.resolve_folder(&path)?;
        }
        if let Some(models) = patch.model_overrides {
            workspace.model_overrides = models
                .into_iter()
                .filter(|entry| !entry.model.trim().is_empty())
                .collect();
        }
        if let Some(archived) = patch.archived {
            workspace.archived = archived;
        }
        self.ctx.store.update_workspace(&workspace)?;
        self.emit_library()?;
        self.ctx.store.workspace(id)
    }

    pub fn duplicate_workspace(&self, id: &str) -> Result<Workspace> {
        let source = self.ctx.store.workspace(id)?;
        let now = now_millis();
        let copy = Workspace {
            id: new_id("ws"),
            name: format!("{} copy", source.name),
            created_at: now,
            updated_at: now,
            archived: false,
            ..source
        };
        self.ctx.store.insert_workspace(&copy)?;
        self.emit_library()?;
        Ok(copy)
    }

    pub fn delete_workspace(&self, id: &str) -> Result<()> {
        let _guard = self.lock_terminal_lifecycle();
        let session_ids = self
            .ctx
            .store
            .sessions()?
            .into_iter()
            .filter(|session| session.workspace_id == id)
            .map(|session| session.id)
            .collect::<Vec<_>>();
        for id in &session_ids {
            self.sessions.forget(id)?;
            self.chats.forget(id);
        }
        self.ctx.store.delete_workspace(id)?;
        for id in session_ids {
            self.remove_chat_attachments(&id);
        }
        self.emit_library()
    }
}
