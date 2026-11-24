use reqwest::{header, Method, RequestBuilder, Response};

use super::{events::event_options, optional_header, Client, Error, EventsQuery};
use crate::{OutputListQuery, RunEventsQuery, RunListQuery, ThumbnailQuery};

/// HTTP range and cache controls for an original output.
#[derive(Debug, Default)]
pub struct OutputRequest<'a> {
    pub if_none_match: Option<&'a str>,
    pub range: Option<&'a str>,
    pub if_range: Option<&'a str>,
}

/// The GET routes also accept HEAD. Use this to inspect their response headers
/// without downloading a body.
#[non_exhaustive]
pub enum HeadEndpoint<'a> {
    Health,
    OpenApi,
    Workflows,
    Workflow(&'a str),
    WorkflowThumbnail(&'a str),
    Runs(&'a RunListQuery),
    Run(&'a str),
    RunEvents(&'a str, &'a RunEventsQuery),
    RunManifest(&'a str, Option<&'a str>),
    Outputs(&'a OutputListQuery),
    Output(&'a str, Option<&'a str>),
    OutputThumbnail(&'a str, &'a ThumbnailQuery, Option<&'a str>),
    Upload(&'a str),
    Settings,
    Events(&'a EventsQuery<'a>),
}

impl Client {
    /// Fetches the workflow thumbnail as a streaming HTTP response.
    pub async fn workflow_thumbnail(&self, id: &str) -> Result<Response, Error> {
        self.send(self.get(&["workflows", id, "thumbnail"])?, false)
            .await
    }

    /// Fetches the original output without buffering it. The response exposes
    /// media headers and the full, partial (206) or unmodified (304) status.
    pub async fn output(&self, id: &str, options: &OutputRequest<'_>) -> Result<Response, Error> {
        let request = self.get(&["outputs", id])?;
        let request = cache(request, options.if_none_match);
        let request = optional_header(request, header::RANGE, options.range);
        let request = optional_header(request, header::IF_RANGE, options.if_range);
        self.send(request, true).await
    }

    /// Fetches the generated thumbnail as a streaming response, or 304 if unchanged.
    pub async fn output_thumbnail(
        &self,
        id: &str,
        query: &ThumbnailQuery,
        if_none_match: Option<&str>,
    ) -> Result<Response, Error> {
        let request = self.get(&["outputs", id, "thumbnail"])?.query(query);
        self.send(cache(request, if_none_match), true).await
    }

    /// Fetches the archived manifest JSON as stored, preserving headers and 304 responses.
    pub async fn run_manifest(
        &self,
        id: &str,
        if_none_match: Option<&str>,
    ) -> Result<Response, Error> {
        let request = self.get(&["runs", id, "manifest"])?;
        self.send(cache(request, if_none_match), true).await
    }

    /// Sends HEAD. An error status has no problem body, so it is returned as
    /// [`Error::HttpStatus`].
    pub async fn head(&self, endpoint: HeadEndpoint<'_>) -> Result<Response, Error> {
        let head = |path: &[&str]| self.request(Method::HEAD, path);
        let request = match endpoint {
            HeadEndpoint::Health => head(&["health"])?,
            HeadEndpoint::OpenApi => head(&["openapi.json"])?,
            HeadEndpoint::Workflows => head(&["workflows"])?,
            HeadEndpoint::Workflow(id) => head(&["workflows", id])?,
            HeadEndpoint::WorkflowThumbnail(id) => head(&["workflows", id, "thumbnail"])?,
            HeadEndpoint::Runs(query) => head(&["runs"])?.query(query),
            HeadEndpoint::Run(id) => head(&["runs", id])?,
            HeadEndpoint::RunEvents(id, query) => head(&["runs", id, "events"])?.query(query),
            HeadEndpoint::RunManifest(id, etag) => cache(head(&["runs", id, "manifest"])?, etag),
            HeadEndpoint::Outputs(query) => head(&["outputs"])?.query(query),
            HeadEndpoint::Output(id, etag) => cache(head(&["outputs", id])?, etag),
            HeadEndpoint::OutputThumbnail(id, query, etag) => {
                cache(head(&["outputs", id, "thumbnail"])?.query(query), etag)
            }
            HeadEndpoint::Upload(id) => head(&["uploads", id])?,
            HeadEndpoint::Settings => head(&["settings"])?,
            HeadEndpoint::Events(query) => event_options(head(&["events"])?, query),
        };
        self.send(request, true).await
    }
}

fn cache(request: RequestBuilder, if_none_match: Option<&str>) -> RequestBuilder {
    optional_header(request, header::IF_NONE_MATCH, if_none_match)
}
