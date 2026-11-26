use crate::error::Result;
use crate::model::{NodeRelease, NodeState, OperationRecord};
use crate::node;

use super::App;

impl App {
    // ------------------------------------------------------------------ node

    pub fn node_state(&self) -> NodeState {
        node::state(&self.ctx)
    }

    pub fn node_releases(&self, mirror: Option<&str>) -> Result<Vec<NodeRelease>> {
        node::releases(&self.ctx, mirror)
    }

    pub fn node_install(&self, request: node::InstallRequest) -> Result<OperationRecord> {
        self.run_and_refresh_agents(node::install_operation(&self.ctx, request)?)
    }

    pub fn node_use(&self, version: &str) -> Result<OperationRecord> {
        self.run_and_refresh_agents(node::use_operation(&self.ctx, version)?)
    }

    pub fn node_remove(&self, version: &str) -> Result<OperationRecord> {
        self.ops.run(node::remove_operation(&self.ctx, version)?)
    }
}
