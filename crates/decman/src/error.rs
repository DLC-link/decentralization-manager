use std::result;

type Error = anyhow::Error;
pub type Result<T = (), E = Error> = result::Result<T, E>;
