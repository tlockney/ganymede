//! Connected machines: other Ganymede servers mounted under
//! `/machines/{id}/` (ganymede-ui#697, #894).
//!
//! The embedding app adds a machine ([`Machines::add`], or
//! `ganymede_server_add_machine`), and the server then forwards the routes
//! on the allowlist ([`ganymede_gateway::mount::route`]) to it, HTTP and
//! websocket, adding the machine's token and sending `Ganymede-Client`.
//! Nothing is mounted until then.
//!
//! Only this server's own page session may use the mount
//! ([`crate::auth::mount`]): its login cookie, or its token from loopback.
//! A tailnet identity and the MCP token get 403. The page's cookie, token,
//! `Origin` and the rest are stripped before forwarding, and a `files` or
//! `nbconvert` response gets this server's own sandbox policy, since the
//! machine's HTML lands on the app's origin.
//!
//! The forwarding itself is in `ganymede-gateway` (`mount`), beside the
//! kernel gateway; this module joins it to the router.

#[cfg(feature = "gateway")]
pub use ganymede_gateway::mount::Machines;

/// Without the `gateway` feature nothing can be mounted: [`Machines::add`]
/// says so.
#[cfg(not(feature = "gateway"))]
#[derive(Default)]
pub struct Machines;

#[cfg(not(feature = "gateway"))]
impl Machines {
    pub fn new() -> Machines {
        Machines
    }

    pub fn add(&self, _id: &str, _url: &str, _token: Option<&str>) -> Result<(), String> {
        Err("this build has no `gateway` feature, so it cannot mount a machine".into())
    }

    pub fn remove(&self, _id: &str) -> bool {
        false
    }

    pub fn ids(&self) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(feature = "gateway")]
mod proxy;
#[cfg(feature = "gateway")]
pub(crate) use proxy::router;
