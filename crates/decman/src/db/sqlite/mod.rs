//! The SQLite store: the pool, and the schema traits implemented on it.

mod connect;
mod read;
#[cfg(test)]
mod tests;
mod tx;
mod write;

pub use connect::{MIGRATOR, connect, repair_migration_checksums};
