use std::sync::Arc;

use crate::paths::AppPaths;
use crate::store::Store;
use crate::system::System;

/// Shared handles that operation steps clone and own.
#[derive(Clone)]
pub struct Context {
    pub paths: AppPaths,
    pub store: Arc<Store>,
    pub system: Arc<dyn System>,
}

impl Context {
    pub fn settings(&self) -> crate::model::AppSettings {
        self.store.setting("app").ok().flatten().unwrap_or_default()
    }
}
