//! Network layer: the transport seam, HTTP range arithmetic, and measurement.
//!
//! [`client`] defines the injectable [`HttpClient`] trait. [`range`] holds pure
//! arithmetic. [`capability`] probes whether a source can be resumed.
//! [`speedtest`] measures how fast bytes actually arrive — the one place in
//! this crate that reads a clock, deliberately. [`reqwest`] is the default
//! implementation, behind the `network` feature.

pub mod capability;
pub mod client;
pub mod range;
pub mod speedtest;

#[cfg(feature = "network")]
pub mod reqwest;

pub use capability::{probe as probe_capabilities, Capabilities};
pub use client::{
    collect_limited, CollectError, Headers, HttpClient, HttpRequest, Method, RawResponse,
    DEFAULT_TIMEOUT, MAX_REDIRECTS,
};
pub use range::{
    decide_resume, parse_content_range, parse_inclusive_span, parse_media_range,
    parse_range_header, ByteRange, ContentRange, RangeParseError, ResumeDecision,
};
pub use speedtest::{Cancel, Latency, Sample, SpeedResult, SpeedTestConfig};

#[cfg(feature = "network")]
pub use reqwest::{ReqwestClient, ReqwestClientBuilder};
