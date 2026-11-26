//! Wire types shared with the TypeScript UI (camelCase on the boundary).
//!
//! Every timestamp on the wire is Unix milliseconds. Guest timestamps carry the
//! guest's clock; server epochs (for example Codex quota) are normalized to
//! milliseconds at the agent boundary. TypeScript declarations are generated
//! from these types by the `ipc_bindings_are_current` test in `lib.rs`.

mod agent;
mod browser;
mod chat;
mod environment;
mod events;
mod machine;
mod misc;
mod network;
mod operations;
mod session;
mod setup;
mod sync_types;
mod workbench;
mod workspace;

pub use agent::*;
pub use browser::*;
pub use chat::*;
pub use environment::*;
pub use events::*;
pub use machine::*;
pub use misc::*;
pub use network::*;
pub use operations::*;
pub use session::*;
pub use setup::*;
pub use sync_types::*;
pub use workbench::*;
pub use workspace::*;
