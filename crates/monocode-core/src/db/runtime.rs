//! One small tokio runtime for every database call. The UI thread never
//! touches a socket: it spawns here and awaits the `JoinHandle`, which is
//! executor-agnostic.

use std::sync::OnceLock;
use tokio::runtime::{Builder, Runtime};

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("monocode-db")
            .enable_all()
            .build()
            .expect("database runtime")
    })
}
