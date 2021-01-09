#[tokio::main]
async fn main() {
    let service = hyper::service::make_service_fn(|_addr| async {
        Result::<_, std::convert::Infallible>::Ok(hyper::service::service_fn(on_request))
    });

    let server = hyper::server::Server::bind(&"0.0.0.0:8000".parse().unwrap());
    server.serve(service);
}

async fn on_request(
    _req: hyper::Request<hyper::Body>,
) -> anyhow::Result<hyper::Response<hyper::Body>> {
    anyhow::bail!("TODO")
}
