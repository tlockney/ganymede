//! The Ganymede engine: one library for everything a host does with
//! kernels and notebooks, with no HTTP and no language knowledge.
//!
//! ```no_run
//! # async fn demo() -> Result<(), ganymede_engine::Error> {
//! use ganymede_engine::Engine;
//!
//! let engine = Engine::new();
//! let kernel = engine.start("cdp-node").await?;
//! let result = kernel.execute("1 + 1").await?;
//! println!("{}", result.text());
//! kernel.shutdown().await?;
//! # Ok(()) }
//! ```
//!
//! - [`Engine`] finds kernelspecs and starts, attaches to, or adopts
//!   kernels.
//! - [`Kernel`] is one running kernel: execute with streaming output and
//!   stdin, complete, inspect, interrupt, restart, shut down, and run
//!   notebooks with `nbclient`'s semantics.
//!
//! The crates underneath are re-exported for anything the facade does not
//! cover: [`protocol`], [`nbformat`], [`host`], [`exec`].

mod engine;
mod kernel;

pub use engine::{notebook_kernel_name, Engine, EngineBuilder, StartOptions};
pub use kernel::{ExecuteOptions, ExecuteResult, Handler, IsComplete, Kernel, NoHandler};

pub use ganymede_exec as exec;
pub use ganymede_kernel_host as host;
pub use ganymede_nbformat as nbformat;
pub use ganymede_protocol as protocol;

pub use ganymede_exec::{Options as NotebookOptions, Report as NotebookReport};
pub use ganymede_kernel_host::Kernelspec;
pub use ganymede_nbformat::{Notebook, Output};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Host(#[from] ganymede_kernel_host::Error),
    #[error(transparent)]
    Notebook(#[from] ganymede_nbformat::Error),
    #[error(transparent)]
    Exec(#[from] ganymede_exec::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Usage(String),
}

impl From<ganymede_protocol::Error> for Error {
    fn from(e: ganymede_protocol::Error) -> Self {
        Error::Host(e.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
