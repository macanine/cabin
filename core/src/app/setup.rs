use crate::error::Result;
use crate::model::{OperationRecord, SetupAction, SetupCheck};
use crate::setup;

use super::App;

impl App {
    // ----------------------------------------------------------------- setup

    pub fn setup_checks(&self) -> Vec<SetupCheck> {
        setup::checks(&self.ctx)
    }

    pub fn run_setup_action(&self, action: &SetupAction) -> Result<OperationRecord> {
        self.run_and_refresh_agents(setup::action_operation(&self.ctx, action)?)
    }
}
