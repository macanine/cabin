use crate::error::{Error, Result};
use crate::util::LockExt;

use super::{App, sync_model_prices};

impl App {
    pub fn usage_sync(&self) -> Result<cabin_usage::SyncResult> {
        static SYNC: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = SYNC.locked();
        let mut usage = cabin_usage::Usage::open(&self.ctx.paths.data().join("usage.sqlite3"))
            .map_err(|error| Error::Internal(format!("open usage database: {error}")))?;
        usage
            .sync(self.ctx.paths.home())
            .map_err(|error| Error::Internal(format!("sync usage: {error}")))
    }

    pub fn usage_dashboard(&self, filter: &cabin_usage::Filter) -> Result<cabin_usage::Dashboard> {
        let usage = cabin_usage::Usage::open(&self.ctx.paths.data().join("usage.sqlite3"))
            .map_err(|error| Error::Internal(format!("open usage database: {error}")))?;
        usage
            .dashboard(filter)
            .map_err(|error| Error::Internal(format!("read usage: {error}")))
    }

    pub fn usage_sync_prices(&self) -> Result<Vec<cabin_usage::pricing::SourceStatus>> {
        sync_model_prices(&self.ctx, true)
    }

    pub fn usage_save_price(&self, price: &cabin_usage::Price) -> Result<()> {
        let usage = cabin_usage::Usage::open(&self.ctx.paths.data().join("usage.sqlite3"))
            .map_err(|error| Error::Internal(format!("open usage database: {error}")))?;
        usage
            .save_price(price)
            .map_err(|error| Error::invalid(format!("save price: {error}")))
    }
}
