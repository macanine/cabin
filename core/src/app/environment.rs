use crate::environment;
use crate::error::Result;
use crate::model::{
    AppEvent, EnvironmentReview, EnvironmentSpec, EnvironmentState, OperationRecord,
};

use super::App;

impl App {
    // ----------------------------------------------------------- environment

    pub fn environment(&self) -> EnvironmentState {
        environment::state(&self.ctx)
    }

    pub fn review_environment(&self, draft: &EnvironmentSpec) -> EnvironmentReview {
        environment::review(&self.ctx, draft)
    }

    pub fn apply_environment(&self, spec: EnvironmentSpec) -> Result<OperationRecord> {
        let record = self
            .ops
            .run(environment::apply_operation(&self.ctx, spec)?)?;
        self.emit(AppEvent::Environment {
            state: self.environment(),
        });
        Ok(record)
    }
}
