//! StreamService — event subscriptions.

use std::pin::Pin;

use futures::Stream;
use tonic::{Request, Response, Status};

use themis_proto::{
    stream_service_server::StreamService,
    Event, SubscribeRequest,
};

use crate::daemon_deps::DaemonDeps;

pub struct StreamServiceImpl {
    deps: DaemonDeps,
}

impl StreamServiceImpl {
    pub fn new(deps: DaemonDeps) -> Self {
        Self { deps }
    }
}

#[tonic::async_trait]
impl StreamService for StreamServiceImpl {
    type SubscribeEventsStream =
        Pin<Box<dyn Stream<Item = Result<Event, Status>> + Send + 'static>>;

    async fn subscribe_events(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeEventsStream>, Status> {
        let req = request.into_inner();

        let lab_filter = req.lab;
        let kind_filter = req.kinds;

        // Create a bounded mpsc channel for this subscriber.
        // The spawned task feeds the channel; we return a stream backed by the receiver.
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Status>>(64);

        let hub = self.deps.hub.clone();
        tokio::spawn(async move {
            hub.stream_events(lab_filter, kind_filter, tx).await;
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(stream)))
    }
}
