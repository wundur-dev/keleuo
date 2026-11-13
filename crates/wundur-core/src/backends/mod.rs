//! The agents a session can run. Each backend maps its own protocol onto
//! [`crate::agent::Event`]s inside its module; see the README for how to add one.

pub mod claude;
pub mod cursor;
pub mod scripted;
