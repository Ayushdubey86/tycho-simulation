use reqwest::{
    blocking::{Client, ClientBuilder},
    header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE},
    Url,
};
use std::{collections::HashMap, string::ToString};
use thiserror::Error;
use tracing::{debug, error, info, instrument, trace, warn};
use tungstenite::{connect, Message};
use uuid::Uuid;

use super::tycho_models::{
    BlockAccountChanges, Chain, Command, ExtractorIdentity, Response, WebSocketMessage,
};
use crate::evm_simulation::tycho_models::{
    StateRequestBody, StateRequestParameters, StateRequestResponse,
};
use tokio::sync::mpsc::{self, Receiver};

/// TODO read consts from config
pub const TYCHO_SERVER_VERSION: &str = "v1";
pub const AMBIENT_EXTRACTOR_HANDLE: &str = "vm:ambient";
pub const AMBIENT_ACCOUNT_ADDRESS: &str = "0xaaaaaaaaa24eeeb8d57d431224f73832bc34f688";

#[derive(Error, Debug)]
pub enum TychoClientError {
    #[error("Failed to parse URI: {0}. Error: {1}")]
    UrlParsing(String, String),
    #[error("Failed to format request: {0}")]
    FormatRequest(String),
    #[error("Unexpected HTTP client error: {0}")]
    HttpClient(String),
    #[error("Failed to parse response: {0}")]
    ParseResponse(String),
}

#[derive(Debug, Clone)]
pub struct TychoHttpClientImpl {
    http_client: Client,
    url: Url,
}
impl TychoHttpClientImpl {
    pub fn new(http_url: &str) -> Result<Self, TychoClientError> {
        // Add a default header to accept JSON
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let client = ClientBuilder::new()
            .default_headers(headers)
            .build()
            .map_err(|e| TychoClientError::HttpClient(e.to_string()))?;
        let url = Url::parse(http_url)
            .map_err(|e| TychoClientError::UrlParsing(http_url.to_owned(), e.to_string()))?;

        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(TychoClientError::UrlParsing(
                http_url.to_owned(),
                "URL scheme must be http or https".to_owned(),
            ));
        }

        Ok(Self { http_client: client, url })
    }
}

pub trait TychoHttpClient {
    fn get_state(
        &self,
        filters: &StateRequestParameters,
        request: &StateRequestBody,
    ) -> Result<StateRequestResponse, TychoClientError>;
}

impl TychoHttpClient for TychoHttpClientImpl {
    #[instrument(skip(self, filters, request))]
    fn get_state(
        &self,
        filters: &StateRequestParameters,
        request: &StateRequestBody,
    ) -> Result<StateRequestResponse, TychoClientError> {
        // Check if contract ids are specified
        if request.contract_ids.is_none() ||
            request
                .contract_ids
                .as_ref()
                .unwrap()
                .is_empty()
        {
            warn!("No contract ids specified in request.");
        }

        // Build the URL
        let mut url = self
            .url
            .join(format!("{}/contract_state", TYCHO_SERVER_VERSION).as_str())
            .map_err(|e| TychoClientError::UrlParsing(self.url.to_string(), e.to_string()))?;

        // Add query params
        url.set_query(Some(&filters.to_query_string()));

        debug!(%url, "Sending contract_state request to Tycho server");
        let body = serde_json::to_string(&request)
            .map_err(|e| TychoClientError::FormatRequest(e.to_string()))?;

        // let header = hyper::header::HeaderValue::from_str("application/json")
        //     .map_err(|e| TychoClientError::FormatRequest(e.to_string()))?;

        let response = self
            .http_client
            .post(url)
            .body(body)
            .send()
            .map_err(|e| TychoClientError::HttpClient(e.to_string()))?;
        debug!(?response, "Received response from Tycho server");

        // Check the response status and read the body
        let response_body = response
            .text()
            .map_err(|e| TychoClientError::ParseResponse(e.to_string()))?;
        let accounts: StateRequestResponse = serde_json::from_str(&response_body)
            .map_err(|e| TychoClientError::ParseResponse(e.to_string()))?;
        info!(?accounts, "Received contract_state response from Tycho server");

        Ok(accounts)
    }
}

pub struct TychoWsClientImpl {
    url: Url,
}

impl TychoWsClientImpl {
    pub fn new(ws_url: &str) -> Result<Self, TychoClientError> {
        let url = Url::parse(ws_url)
            .map_err(|e| TychoClientError::UrlParsing(ws_url.to_owned(), e.to_string()))?;

        if url.scheme() != "ws" && url.scheme() != "wss" {
            return Err(TychoClientError::UrlParsing(
                ws_url.to_owned(),
                "URL scheme must be ws or wss".to_owned(),
            ));
        }

        Ok(Self { url })
    }
}

pub trait TychoWsClient {
    /// Subscribe to an extractor and receive realtime messages
    fn subscribe(&self, extractor_id: ExtractorIdentity) -> Result<(), TychoClientError>;

    /// Unsubscribe from an extractor
    fn unsubscribe(&self, subscription_id: Uuid) -> Result<(), TychoClientError>;

    /// Consumes realtime messages from the WebSocket server
    fn realtime_messages(&self) -> Receiver<BlockAccountChanges>;
}

impl TychoWsClient for TychoWsClientImpl {
    #[allow(unused_variables)]
    fn subscribe(&self, extractor_id: ExtractorIdentity) -> Result<(), TychoClientError> {
        panic!("Not implemented");
    }

    #[allow(unused_variables)]
    fn unsubscribe(&self, subscription_id: Uuid) -> Result<(), TychoClientError> {
        panic!("Not implemented");
    }

    fn realtime_messages(&self) -> Receiver<BlockAccountChanges> {
        // Create a channel to send and receive messages.
        let (tx, rx) = mpsc::channel(30); //TODO: Set this properly.

        // Spawn a task to connect to the WebSocket server and listen for realtime messages.
        // let ws_url = format!("ws://{}/{}/ws", self.url, TYCHO_SERVER_VERSION); // TODO: Set path
        // properly
        let ws_url = self
            .url
            .join(format!("{}/ws", TYCHO_SERVER_VERSION).as_str())
            .unwrap();
        info!(?ws_url, "Spawning task to connect to WebSocket server");
        let mut active_extractors: HashMap<Uuid, ExtractorIdentity> = HashMap::new();

        // Connect to Tycho server
        info!(?ws_url, "Connecting to WebSocket server");
        let (mut ws, _) = connect(&ws_url)
            .map_err(|e| error!(error = %e, "Failed to connect to WebSocket server"))
            .expect("connect to websocket");

        // Send a subscribe request to ambient extractor
        // TODO: Read from config
        let command = Command::Subscribe {
            extractor_id: ExtractorIdentity::new(Chain::Ethereum, AMBIENT_EXTRACTOR_HANDLE),
        };
        let _ = ws
            .send(Message::Text(serde_json::to_string(&command).unwrap()))
            .map_err(|e| error!(error = %e, "Failed to send subscribe request"));

        // Use the stream directly to listen for messages.
        while let Ok(msg) = ws.read() {
            match msg {
                Message::Text(text) => match serde_json::from_str::<WebSocketMessage>(&text) {
                    Ok(WebSocketMessage::BlockAccountChanges(block_state_changes)) => {
                        info!(
                            ?block_state_changes,
                            "Received a block state change, sending to channel"
                        );
                        tx.blocking_send(block_state_changes)
                            .map_err(|e| error!(error = %e, "Failed to send message"))
                            .expect("send message");
                    }
                    Ok(WebSocketMessage::Response(Response::NewSubscription {
                        extractor_id,
                        subscription_id,
                    })) => {
                        info!(?extractor_id, ?subscription_id, "Received a new subscription");
                        active_extractors.insert(subscription_id, extractor_id);
                        trace!(?active_extractors, "Active extractors");
                    }
                    Ok(WebSocketMessage::Response(Response::SubscriptionEnded {
                        subscription_id,
                    })) => {
                        info!(?subscription_id, "Received a subscription ended");
                        active_extractors
                            .remove(&subscription_id)
                            .expect("subscription id in active extractors");
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to deserialize message");
                    }
                },
                Message::Ping(_) => {
                    // Respond to pings with pongs.
                    ws.send(Message::Pong(Vec::new()))
                        .unwrap();
                }
                Message::Pong(_) => {
                    // Do nothing.
                }
                Message::Close(_) => {
                    // Close the connection.
                    drop(tx);
                    break;
                }
                unknown_msg => {
                    info!("Received an unknown message type: {:?}", unknown_msg);
                }
            }
        }

        info!("Returning receiver");
        rx
    }
}

#[cfg(test)]
mod tests {
    use crate::evm_simulation::tycho_models::{AccountUpdate, Block, ChangeType};
    use chrono::NaiveDateTime;
    use std::{net::TcpListener, str::FromStr};

    use super::*;

    use mockito::Server;

    use revm::primitives::{B160, B256, U256 as rU256};

    #[test]
    fn test_realtime_messages() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();

        let server_thread = std::thread::spawn(move || {
            // Accept only the first connection
            if let Ok((stream, _)) = server.accept() {
                let mut websocket = tungstenite::accept(stream).unwrap();

                let test_msg_content = r#"
                {
                    "extractor": "vm:ambient",
                    "chain": "ethereum",
                    "block": {
                        "number": 123,
                        "hash": "0x0000000000000000000000000000000000000000000000000000000000000000",
                        "parent_hash":
                            "0x0000000000000000000000000000000000000000000000000000000000000000",            
                        "chain": "ethereum",             "ts": "2023-09-14T00:00:00"
                                },
                                "account_updates": {
                                    "0x7a250d5630b4cf539739df2c5dacb4c659f2488d": {
                                        "address": "0x7a250d5630b4cf539739df2c5dacb4c659f2488d",
                                        "chain": "ethereum",
                                        "slots": {},
                                        "balance":
                        "0x00000000000000000000000000000000000000000000000000000000000001f4",            
                        "code": "",                 "change": "Update"
                                    }
                                },
                                "new_pools": {}
                }
                "#;

                websocket
                    .send(Message::Text(test_msg_content.to_string()))
                    .expect("Failed to send message");

                // Close the WebSocket connection
                let _ = websocket.close(None);
            }
        });

        // Now, you can create a client and connect to the mocked WebSocket server
        let client = TychoWsClientImpl::new(&format!("ws://{}", addr)).unwrap();

        // You can listen to the realtime_messages and expect the messages that you send from
        // handle_connection
        let mut rx = client.realtime_messages();
        let received_msg = rx
            .blocking_recv()
            .expect("receive message");

        let expected_blk = Block {
            number: 123,
            hash: B256::from_str(
                "0x0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
            parent_hash: B256::from_str(
                "0x0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
            chain: Chain::Ethereum,
            ts: NaiveDateTime::from_str("2023-09-14T00:00:00").unwrap(),
        };
        let account_update = AccountUpdate::new(
            B160::from_str("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D").unwrap(),
            Chain::Ethereum,
            HashMap::new(),
            Some(rU256::from(500)),
            Some(Vec::<u8>::new()),
            ChangeType::Update,
        );
        let account_updates: HashMap<B160, AccountUpdate> = vec![(
            B160::from_str("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D").unwrap(),
            account_update,
        )]
        .into_iter()
        .collect();
        let expected = BlockAccountChanges::new(
            "vm:ambient".to_string(),
            Chain::Ethereum,
            expected_blk,
            account_updates,
            HashMap::new(),
        );

        assert_eq!(received_msg, expected);

        server_thread.join().unwrap();
    }

    #[test]
    fn test_simple_route_mock() {
        let mut server = Server::new();
        let server_resp = r#"
        {
            "accounts": [
                {
                    "chain": "ethereum",
                    "address": "0x0000000000000000000000000000000000000000",
                    "title": "",
                    "slots": {},
                    "balance": "0x1f4",
                    "code": "",
                    "code_hash": "0x5c06b7c5b3d910fd33bc2229846f9ddaf91d584d9b196e16636901ac3a77077e",
                    "balance_modify_tx": "0x0000000000000000000000000000000000000000000000000000000000000000",
                    "code_modify_tx": "0x0000000000000000000000000000000000000000000000000000000000000000",
                    "creation_tx": null
                }
            ]
        }
        "#;
        // test that the response is deserialized correctly
        serde_json::from_str::<StateRequestResponse>(server_resp).expect("deserialize");

        let mocked_server = server
            .mock("POST", "/v1/contract_state?chain=ethereum")
            .expect(1)
            .with_body(server_resp)
            .create();

        let client = TychoHttpClientImpl::new(&server.url()).expect("create client");

        let response = client
            .get_state(&Default::default(), &Default::default())
            .expect("get state");
        let accounts = response.accounts;

        mocked_server.assert();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].slots, HashMap::new());
        assert_eq!(accounts[0].balance, rU256::from(500));
        assert_eq!(accounts[0].code, Vec::<u8>::new());
        assert_eq!(
            accounts[0].code_hash,
            B256::from_str("0x5c06b7c5b3d910fd33bc2229846f9ddaf91d584d9b196e16636901ac3a77077e")
                .unwrap()
        );
    }
}
