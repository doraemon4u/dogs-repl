//! Session layer: global state, chat scope, and the event-dispatch actor.
//!
//! The actor in [`actor`] is the only writer of [`store::Store`]. The event bus
//! remains available for local subscriptions; cross-layer traffic goes through
//! the actor so that ordering and ownership stay in one place.

pub mod actor;
pub mod event_bus;
pub mod store;

pub use actor::{Command, SessionConfig, SessionHandle};
