// Copyright 2022, The Tremor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::time::Duration;

use async_std::channel::{bounded, Receiver, Sender};
use async_std::prelude::*;
use tremor_value::{literal, structurize};

use super::meta::*;
use crate::connectors::prelude::*;
use crate::connectors::sink::concurrency_cap::ConcurrencyCap;
use crate::connectors::utils::mime::*;
use crate::postprocessor::{self, Postprocessors};
use crate::preprocessor::{self, Preprocessors};

const CONNECTOR_TYPE: &str = "http_client";

/// The HTTP client connector - for HTTP-based API interactions
pub struct HttpClient {
    max_concurrency: usize,
    response_tx: Sender<SourceReply>,
    response_rx: Receiver<SourceReply>,
    clients_tx: Sender<Vec<SurfClient>>,
    clients_rx: Receiver<Vec<SurfClient>>,
    connector_config: ConnectorConfig,
    codec_name: String,
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HttpClient")
    }
}

#[derive(Debug, Default)]
pub(crate) struct Builder {}

#[async_trait::async_trait]
impl ConnectorBuilder for Builder {
    fn connector_type(&self) -> ConnectorType {
        CONNECTOR_TYPE.into()
    }

    async fn from_config(
        &self,
        _id: &str,
        connector_config: &ConnectorConfig,
    ) -> Result<Box<dyn Connector>> {
        let _preprocessor_configs = connector_config.preprocessors.clone().unwrap_or_default();
        let _postprocessor_configs = connector_config.postprocessors.clone().unwrap_or_default();
        let codec_name = if let Some(codec_name) = &connector_config.codec {
            codec_name.name.clone()
        } else {
            "json".to_string()
        };
        if let Some(config) = &connector_config.config {
            let config = Config::new(&config)?;
            let _codec_map: MimeCodecMap =
                if let Some(codec_map) = &connector_config.config.get("codec_map") {
                    let value: Value = (*codec_map).clone();
                    structurize::<MimeCodecMap>(value)?
                } else {
                    MimeCodecMap::with_builtin()
                };
            let (clients_tx, clients_rx) = bounded(128);
            let (response_tx, response_rx) = bounded(128);
            Ok(Box::new(HttpClient {
                max_concurrency: config.concurrency,
                response_tx,
                response_rx,
                clients_tx,
                clients_rx,
                connector_config: connector_config.clone(),
                codec_name,
            }))
        } else {
            Err(ErrorKind::MissingConfiguration(String::from("HttpClient")).into())
        }
    }
}

#[async_trait::async_trait]
impl Connector for HttpClient {
    fn codec_requirements(&self) -> CodecReq {
        // FIXME Optional should be Optional(String)
        let codec_name = self.codec_name.clone();
        let codec_name = Box::leak(codec_name.into_boxed_str());
        CodecReq::Optional(codec_name)
    }

    async fn create_source(
        &mut self,
        source_context: SourceContext,
        builder: SourceManagerBuilder,
    ) -> Result<Option<SourceAddr>> {
        let source = HttpRequestSource {
            rx: self.response_rx.clone(),
            http_meta: HttpRequestMeta::from_config(&self.connector_config, "json")?,
        };
        builder.spawn(source, source_context).map(Some)
    }

    async fn create_sink(
        &mut self,
        sink_context: SinkContext,
        builder: SinkManagerBuilder,
    ) -> Result<Option<SinkAddr>> {
        let sink = HttpRequestSink::new(
            self.clients_rx.clone(),
            self.response_tx.clone(),
            builder.reply_tx(),
            self.max_concurrency,
            HttpRequestMeta::from_config(&self.connector_config, "json")?,
        );
        builder.spawn(sink, sink_context).map(Some)
    }

    async fn connect(&mut self, _ctx: &ConnectorContext, _attempt: &Attempt) -> Result<bool> {
        let mut clients = Vec::with_capacity(self.max_concurrency);

        for _i in 1..self.max_concurrency {
            clients.push(SurfClient {
                client: surf::client(),
            });
        }

        self.clients_tx.send(clients).await?;
        Ok(true)
    }
}

/// Time to await an answer before handing control back to the source manager
const SOURCE_RECV_TIMEOUT: Duration = Duration::from_millis(100);

struct HttpRequestSource {
    #[allow(dead_code)]
    http_meta: HttpRequestMeta,
    rx: Receiver<SourceReply>,
}

#[async_trait::async_trait()]
impl Source for HttpRequestSource {
    async fn pull_data(&mut self, _pull_id: &mut u64, _ctx: &SourceContext) -> Result<SourceReply> {
        match self.rx.recv().timeout(SOURCE_RECV_TIMEOUT).await? {
            Ok(receiver) => Ok(receiver),
            Err(e) => Err(e.into()),
        }
    }

    fn is_transactional(&self) -> bool {
        true
    }

    fn asynchronous(&self) -> bool {
        true
    }
}

#[derive(Clone)]
struct SurfClient {
    client: surf::Client,
}

struct SurfClients {
    clients: Vec<SurfClient>,
    idx: usize,
}

impl SurfClients {
    fn new(clients: Vec<SurfClient>) -> Self {
        Self { clients, idx: 0 }
    }

    /// Return a freshly cloned client and its address.
    ///
    /// Sending a request tracks the transport-lifetime
    /// so we need to clone it, so we can handle it in a separate task.
    /// Cloning the client should be cheap
    fn next(&mut self) -> Option<SurfClient> {
        let len = self.clients.len();
        if len > 0 {
            let idx = self.idx % len;
            self.idx += 1;
            if let Some(client) = self.clients.get(idx) {
                return Some(client.clone());
            }
        }
        None
    }
}

struct HttpRequestSink {
    clients: SurfClients,
    clients_rx: Receiver<Vec<SurfClient>>,
    response_tx: Sender<SourceReply>,
    // reply_tx: Sender<AsyncSinkReply>,
    concurrency_cap: ConcurrencyCap,
    origin_uri: EventOriginUri,
    http_meta: HttpRequestMeta,
}

impl HttpRequestSink {
    fn new(
        clients_rx: Receiver<Vec<SurfClient>>,
        response_tx: Sender<SourceReply>,
        reply_tx: Sender<AsyncSinkReply>,
        max_in_flight_requests: usize,
        http_meta: HttpRequestMeta,
    ) -> Self {
        Self {
            clients: SurfClients::new(vec![]),
            clients_rx,
            response_tx,
            // reply_tx: reply_tx.clone(),
            concurrency_cap: ConcurrencyCap::new(max_in_flight_requests, reply_tx),
            origin_uri: EventOriginUri {
                scheme: String::from("rest_client"),
                host: String::from("dummy"), // will be replaced in `on_event`
                port: None,
                path: vec![],
            },
            http_meta,
        }
    }

    async fn refresh_clients(&mut self, ctx: &SinkContext) -> Result<()> {
        // check for a client refresh
        if let Ok(new_clients) = self.clients_rx.try_recv() {
            info!("{} Received new clients", ctx);
            self.clients = SurfClients::new(new_clients);
        }
        Ok(())
    }
}

#[async_trait::async_trait()]
impl Sink for HttpRequestSink {
    async fn on_event(
        &mut self,
        _input: &str,
        event: Event,
        ctx: &SinkContext,
        _serializer: &mut EventSerializer,
        _start: u64,
    ) -> Result<SinkReply> {
        self.refresh_clients(ctx).await?;
        // constrain to max concurrency - propagate CB close on hitting limit
        let guard = self.concurrency_cap.inc_for(&event).await?;

        if let Some(client) = self.clients.next() {
            let response_tx = self.response_tx.clone();
            //            let _reply_tx = self.reply_tx.clone();
            let origin_uri = self.origin_uri.clone();

            let (request, request_meta) = self.http_meta.process(&event)?;

            let mut codec = self.http_meta.codec.boxed_clone();
            let mut preprocessors: Preprocessors =
                Vec::with_capacity(self.http_meta.preprocessors.len());
            for p in &self.http_meta.preprocessors {
                preprocessors.push(preprocessor::lookup(p.name())?);
            }
            let mut postprocessors: Postprocessors =
                Vec::with_capacity(self.http_meta.postprocessors.len());
            for p in &self.http_meta.postprocessors {
                postprocessors.push(postprocessor::lookup(p.name())?);
            }

            let client = client.client;

            async_std::task::Builder::new()
                .name(format!("Rest Connector #{}", guard.num()))
                .spawn::<_, Result<()>>(async move {
                    match HttpResponseMeta::invoke(
                        &mut codec,
                        &mut preprocessors,
                        &mut postprocessors,
                        request_meta.clone(),
                        &origin_uri,
                        client,
                        request,
                    )
                    .await
                    {
                        Ok(ResponseEventCont::Valid(source_replies)) => {
                            for sr in source_replies {
                                response_tx.send(sr).await?;
                            }
                        }
                        Ok(ResponseEventCont::CodecError) => {
                            let meta = request_meta;
                            response_tx
                                .send(SourceReply::Structured {
                                    origin_uri,
                                    payload: EventPayload::try_new::<crate::Error, _>(
                                        vec![],
                                        |_mut_data| {
                                            let value = literal!({ "status": 415}).clone_static();
                                            Ok(ValueAndMeta::from_parts(
                                                value,
                                                literal!({
                                                    "request": meta,
                                                }),
                                            ))
                                        },
                                    )?,
                                    stream: DEFAULT_STREAM_ID,
                                    port: None,
                                })
                                .await?;
                        }
                        Err(_e) => {
                            error!(
                                "Unhandled / unexpected condition responding to http_server event"
                            )
                        }
                    };
                    Ok(())
                })?;
        } else {
            error!("{} No rest client available.", &ctx);
            return Ok(SinkReply::FAIL);
        }

        Ok(SinkReply::NONE)
    }

    async fn on_signal(
        &mut self,
        _signal: Event,
        ctx: &SinkContext,
        _serializer: &mut EventSerializer,
    ) -> Result<SinkReply> {
        self.refresh_clients(ctx).await?;
        Ok(SinkReply::default())
    }

    fn auto_ack(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use env_logger;
    use http_types::Method;

    #[async_std::test]
    async fn http_client_builder() -> Result<()> {
        let with_processors = literal!({
            "id": "my_rest_client",
            "type": "rest_client",
            "config": {
                "url": "https://www.google.com"
            },
            "preprocessors": [ "lines" ],
            "postprocessors": [ "lines" ],
        });
        let config: ConnectorConfig = crate::config::Connector::from_defn(
            "snot".into(),
            ConnectorType("rest_client".into()),
            with_processors,
        )?;

        let builder = super::Builder::default();
        let _connector = builder.from_config("foo", &config).await?;

        Ok(())
    }

    #[test]
    fn default_http_meta_codec_handling() -> Result<()> {
        let connector_config = literal!({
            "id": "my_rest_client",
            "type": "rest_client",
            "config": {
                "url": "https://www.google.com"
            },
            "preprocessors": [ "lines" ]
        });
        let config: ConnectorConfig = crate::config::Connector::from_defn(
            "snot".into(),
            ConnectorType("rest_client".into()),
            connector_config,
        )?;
        let http_meta = HttpRequestMeta::from_config(&config, "json")?;
        assert_eq!("json", http_meta.codec.name());
        let http_meta = HttpRequestMeta::from_config(&config, "string")?;
        assert_eq!("string", http_meta.codec.name());
        let http_meta = HttpRequestMeta::from_config(&config, "snot");
        assert!(http_meta.is_err());

        let connector_config = literal!({
            "id": "my_rest_client",
            "type": "rest_client",
            "config": {
                "url": "https://www.google.com"
            },
            "codec": "msgpack",
            "preprocessors": [ "lines" ]
        });
        let config: ConnectorConfig = crate::config::Connector::from_defn(
            "snot".into(),
            ConnectorType("rest_client".into()),
            connector_config,
        )?;
        let http_meta = HttpRequestMeta::from_config(&config, "snot")?;
        assert_eq!("msgpack", http_meta.codec.name());

        Ok(())
    }

    #[test]
    fn default_http_meta_endpoint_handling() -> Result<()> {
        env_logger::init();
        let connector_config = literal!({
            "id": "my_rest_client",
            "type": "rest_client",
            "config": {
            },
            "preprocessors": [ "lines" ]
        });
        let config: ConnectorConfig = crate::config::Connector::from_defn(
            "snot".into(),
            ConnectorType("rest_client".into()),
            connector_config,
        )?;
        let http_meta = HttpRequestMeta::from_config(&config, "json")?;
        assert_eq!("https://localhost:443/", http_meta.endpoint);

        let connector_config = literal!({
            "id": "my_rest_client",
            "type": "rest_client",
            "config": {
                "url": "https://tremor.rs/"
            },
            "preprocessors": [ "lines" ]
        });
        let config: ConnectorConfig = crate::config::Connector::from_defn(
            "snot".into(),
            ConnectorType("rest_client".into()),
            connector_config,
        )?;
        let http_meta = HttpRequestMeta::from_config(&config, "json")?;
        assert_eq!("https://tremor.rs/", http_meta.endpoint);
        Ok(())
    }

    #[test]
    fn default_http_meta_method_handling() -> Result<()> {
        let connector_config = literal!({
            "id": "my_rest_client",
            "type": "rest_client",
            "config": {
                "url": "https://tremor.rs/benchmarks/"
            },
            "preprocessors": [ "lines" ]
        });
        let config: ConnectorConfig = crate::config::Connector::from_defn(
            "snot".into(),
            ConnectorType("rest_client".into()),
            connector_config,
        )?;
        let http_meta = HttpRequestMeta::from_config(&config, "json")?;
        assert_eq!("json", http_meta.codec.name());
        assert_eq!("https://tremor.rs/benchmarks/", http_meta.endpoint);
        assert_eq!(Method::Post, http_meta.method);

        let connector_config = literal!({
            "id": "my_rest_client",
            "type": "rest_client",
            "config": {
                "url": "https://tremor.rs/",
                "method": "get"
            },
            "preprocessors": [ "lines" ]
        });
        let config: ConnectorConfig = crate::config::Connector::from_defn(
            "snot".into(),
            ConnectorType("rest_client".into()),
            connector_config,
        )?;
        let http_meta = HttpRequestMeta::from_config(&config, "json")?;
        assert_eq!("json", http_meta.codec.name());
        assert_eq!("https://tremor.rs/", http_meta.endpoint);
        assert_eq!(Method::Get, http_meta.method);
        Ok(())
    }

    #[test]
    fn default_http_meta_headers_handling() -> Result<()> {
        let connector_config = literal!({
                    "id": "my_rest_client",
                    "type": "rest_client",
                    "config": {
        //                "url": "https://tremor.rs/",
                        "method": "pUt",
                    },
                    "preprocessors": [ "lines" ]
                });
        let config: ConnectorConfig = crate::config::Connector::from_defn(
            "snot".into(),
            ConnectorType("rest_client".into()),
            connector_config,
        )?;
        let http_meta = HttpRequestMeta::from_config(&config, "json")?;
        assert_eq!("json", http_meta.codec.name());
        assert_eq!("https://localhost:443/", http_meta.endpoint);
        assert_eq!(Method::Put, http_meta.method);
        assert_eq!(0, http_meta.headers.len());

        let connector_config = literal!({
            "id": "my_rest_client",
            "type": "rest_client",
            "config": {
                "url": "https://tremor.rs/",
                "method": "pAtCH",
                "headers": {
                    "snot": [ "Badger" ],
                }
            },
            "preprocessors": [ "lines" ]
        });
        let config: ConnectorConfig = crate::config::Connector::from_defn(
            "snot".into(),
            ConnectorType("rest_client".into()),
            connector_config,
        )?;
        let http_meta = HttpRequestMeta::from_config(&config, "json")?;
        assert_eq!("json", http_meta.codec.name());
        assert_eq!("https://tremor.rs/", http_meta.endpoint);
        assert_eq!(Method::Patch, http_meta.method);
        assert_eq!(1, http_meta.headers.len());
        assert_eq!(
            "Badger",
            http_meta.headers.get("snot").unwrap()[0].to_string()
        );
        Ok(())
    }
}