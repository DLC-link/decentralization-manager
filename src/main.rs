mod cli;

use cli::{Cli, Parser};

use grpc_test::{
    error::Result,
    proto::com::digitalasset::canton::admin::health::v30::{
        GetLastErrorsRequest, status_service_client::StatusServiceClient,
    },
};

#[tokio::main]
async fn main() -> Result {
    let args = Cli::parse();

    let admin_api_url = format!("{}:{}", args.host_url, args.admin_api_port);

    let mut client = StatusServiceClient::connect(admin_api_url).await?;

    let request = tonic::Request::new(GetLastErrorsRequest {});
    let errors = client.get_last_errors(request).await?;
    println!("Errors collected from the chain: {errors:?}");

    Ok(())
}
