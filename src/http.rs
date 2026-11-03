use crate::error::Failure;
use reqwest::Client;
use std::time::Duration;

pub const USER_AGENT: &str = concat!("toknix/", env!("CARGO_PKG_VERSION"));

#[derive(Debug)]
pub struct Fetched {
    pub status: reqwest::StatusCode,
    pub body: String,
}

#[derive(Debug)]
pub enum TransportError {
    Timeout,
    Network(String),
}

pub fn client(timeout: Duration) -> Client {
    Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(USER_AGENT)
        .build()
        .expect("reqwest client")
}

pub async fn execute(builder: reqwest::RequestBuilder) -> Result<Fetched, TransportError> {
    let response = builder.send().await.map_err(transport)?;
    let status = response.status();
    let bytes = response.bytes().await.map_err(transport)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(TransportError::Network(
            "response exceeded 8 MiB".to_string(),
        ));
    }
    Ok(Fetched {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

pub fn transport_failure(error: TransportError, timed_out: &str, network: &str) -> Failure {
    match error {
        TransportError::Timeout => Failure::new(timed_out),
        TransportError::Network(reason) => Failure::new(format!("{network}: {reason}")),
    }
}

fn transport(error: reqwest::Error) -> TransportError {
    if error.is_timeout() {
        TransportError::Timeout
    } else {
        TransportError::Network(error.to_string())
    }
}
