//! Downloading: the queue, the state machine, and the transfer engine.
//!
//! Three pieces, in increasing order of invariant risk:
//!
//! * [`task`] — [`task::Task`] and [`task::TaskState`]. Plain data plus a
//!   checked transition graph. No I/O.
//! * [`store`] — [`store::Store`]. Atomic writes, corrupt-tolerant reads. A
//!   malformed queue degrades to "some tasks lost", never to "will not start".
//! * [`engine`] — [`engine::run`]. One task, start to finish, resumably. This
//!   is where a file gets silently corrupted if the resume logic is wrong, so
//!   the decisions are extracted into pure functions in [`crate::net::range`]
//!   and exhaustively unit-tested there.
//!
//! [`manager`] sits above all three and owns scheduling, persistence and the
//! event stream a UI renders from.

pub mod engine;
pub mod manager;
pub mod store;
pub mod task;

pub use engine::{run as run_task, Cancel, Progress};
pub use manager::{Manager, ManagerOptions, QueueEvent, DEFAULT_CONCURRENCY};
pub use store::{QueueFile, Store, STORE_VERSION};
pub use task::{
    EventKind, Mark, Task, TaskEvent, TaskState, MARK_INTERVAL_MS, MAX_EVENTS, MAX_MARKS,
};
