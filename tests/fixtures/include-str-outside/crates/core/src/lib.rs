/// Reads a file outside this crate's directory. Nothing in the manifest says so.
pub const SCHEMA: &str = include_str!("../../../docs/schema.sql");

pub fn schema_len() -> usize {
    SCHEMA.len()
}
