//! Twitch protocol logic with no network and no clock of its own: every function takes text or numbers in and
//! gives values out, so the whole of it is tested without a Twitch account. The binary does the I/O.

pub mod eventsub;
pub mod helix;
pub mod irc;
pub mod oauth;
mod secret;

pub use secret::Secret;
