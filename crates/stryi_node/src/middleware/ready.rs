use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use http::Request as HttpRequest;
use tokio::sync::RwLock;
use tonic::server::NamedService;
use tower::{Layer, Service};

/// Shared flag for controlling readiness of the API.
/// Wrapped services will reject requests until this flag is `true`.
pub type ReadyFlag = Arc<RwLock<bool>>;

/// Implemented by a service to emit a protocol-correct "not ready" reply.
pub trait NotReadyResponder: Send + Sync + 'static {
    type NotReadyResponse;

    fn not_ready(&self) -> Self::NotReadyResponse;
}

/// Adds [`ReadyGate`] around any Tower service.
#[derive(Clone)]
pub struct ReadyGateLayer {
    flag: ReadyFlag,
}

impl ReadyGateLayer {
    pub fn new(flag: ReadyFlag) -> Self {
        Self { flag }
    }
}

impl<S> Layer<S> for ReadyGateLayer {
    type Service = ReadyGate<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ReadyGate {
            inner,
            flag: self.flag.clone(),
        }
    }
}

/// Middleware that blocks requests until the node is ready.
#[derive(Clone)]
pub struct ReadyGate<S> {
    inner: S,
    flag: ReadyFlag,
}

/// Preserve the gRPC service name when wrapping a Tonic server.
impl<S> NamedService for ReadyGate<S>
where
    S: NamedService,
{
    const NAME: &'static str = S::NAME;
}

impl<S, B> Service<HttpRequest<B>> for ReadyGate<S>
where
    S: Service<HttpRequest<B>> + Clone + Send + Sync + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
    B: http_body::Body + Send + 'static,
    S: NotReadyResponder<NotReadyResponse = S::Response>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: HttpRequest<B>) -> Self::Future {
        let ready_flag = self.flag.clone();
        let mut ready_call = self.inner.clone(); // normal request
        let reject_call = self.inner.clone(); // not-ready response

        Box::pin(async move {
            if !*ready_flag.read().await {
                return Ok(reject_call.not_ready());
            }
            ready_call.call(req).await
        })
    }
}
