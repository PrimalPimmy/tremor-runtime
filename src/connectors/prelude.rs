// Copyright 2021, The Tremor Team
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

pub use crate::connectors::quiescence::QuiescenceBeacon;
pub use crate::connectors::reconnect::{Attempt, ConnectionLostNotifier};
pub use crate::connectors::sink::{
    AsyncSinkReply, ChannelSink, ChannelSinkRuntime, ContraflowData, EventSerializer,
    SingleStreamSink, SingleStreamSinkRuntime, Sink, SinkAck, SinkAddr, SinkContext,
    SinkManagerBuilder, SinkMeta, SinkReply, StreamWriter,
};
pub use crate::connectors::source::{
    ChannelSource, ChannelSourceRuntime, Source, SourceAddr, SourceContext, SourceManagerBuilder,
    SourceReply, SourceReplySender, StreamReader, DEFAULT_POLL_INTERVAL,
};
pub use crate::connectors::{
    Connector, ConnectorBuilder, ConnectorContext, ConnectorState, StreamDone, StreamIdGen,
};
pub use crate::errors::{Error, ErrorKind, Result};
pub use crate::url::TremorUrl;
pub use crate::utils::hostname;
pub use crate::{Event, OpConfig, QSIZE};
pub use std::sync::atomic::Ordering;
pub use tremor_pipeline::{ConfigImpl, EventOriginUri, DEFAULT_STREAM_ID};
pub(crate) fn default_buf_size() -> usize {
    8192
}