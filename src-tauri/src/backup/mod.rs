//! Arciin Computer Backup: this PC's protected folders, backed up to Arciin.
//!
//! One direction only. Windows is the source of truth; the server is the
//! destination. Nothing here ever deletes or rewrites a local file, and there
//! is deliberately no code path that accepts a destructive instruction from
//! the server.
//!
//! The canonical shape sent to Arciin preserves the source tree:
//!
//! ```txt
//! Computers -> {this PC} -> {protected root} -> {source hierarchy}
//! ```
//!
//! This client never routes a file by media type. `logo.png` is uploaded once,
//! inside the folder it lives in; the server is what surfaces it in Images.

pub mod client;
pub mod engine;
pub mod folder_picker;
pub mod known_folders;
pub mod manager;
pub mod ordering;
pub mod protocol;
pub mod scan;
pub mod store;
