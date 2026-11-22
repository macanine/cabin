use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;

use crate::app_error::AppError;
use crate::host::Host;

pub(crate) type Shared = Arc<Host>;

/// Result of a long-running command, delivered on its channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum TaskEvent<T> {
    Completed { result: T },
    Failed { error: AppError },
}

/// Run `work` on the blocking pool and report the outcome on `channel`.
pub(crate) fn spawn_task<T, F>(host: &Shared, channel: Channel<TaskEvent<T>>, work: F)
where
    T: Serialize + Clone + Send + 'static,
    F: FnOnce(&Host) -> Result<T, AppError> + Send + 'static,
{
    let host = host.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let event = match work(&host) {
            Ok(result) => TaskEvent::Completed { result },
            Err(error) => TaskEvent::Failed { error },
        };
        let _ = channel.send(event);
    });
}

/// Run a blocking call off the main thread.
pub(crate) async fn blocking<T, F>(host: &Shared, work: F) -> Result<T, AppError>
where
    T: Send + 'static,
    F: FnOnce(&Host) -> Result<T, AppError> + Send + 'static,
{
    let host = host.clone();
    tauri::async_runtime::spawn_blocking(move || work(&host))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_events_use_the_ipc_wire_contract() -> Result<(), serde_json::Error> {
        let failed = TaskEvent::<i32>::Failed {
            error: AppError {
                code: "invalid".into(),
                message: "bad".into(),
                details: None,
            },
        };
        assert_eq!(
            serde_json::to_value(TaskEvent::Completed { result: 42 })?,
            serde_json::json!({"type": "completed", "result": 42})
        );
        assert_eq!(
            serde_json::to_value(failed)?,
            serde_json::json!({"type": "failed", "error": {"code": "invalid", "message": "bad"}})
        );
        Ok(())
    }
}
