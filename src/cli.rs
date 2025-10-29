use std::borrow::Cow;

pub use clap::Parser;

#[derive(Parser)]
pub struct Cli {
    #[arg(long, default_value_t = Cow::Borrowed("http://localhost"))]
    pub host_url: Cow<'static, str>,

    #[arg(long, default_value_t = 5001)]
    pub ledger_api_port: u16,

    #[arg(long, default_value_t = 5002)]
    pub admin_api_port: u16,
}
