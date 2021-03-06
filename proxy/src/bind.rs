use std::error::Error;
use std::fmt;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use http;
use tokio_core::reactor::Handle;
use tower;
use tower_h2;
use tower_reconnect::Reconnect;

use conduit_proxy_controller_grpc;
use control;
use ctx;
use telemetry::{self, sensor};
use transparency::{self, HttpBody};
use transport;

/// Binds a `Service` from a `SocketAddr`.
///
/// The returned `Service` buffers request until a connection is established.
///
/// # TODO
///
/// Buffering is not bounded and no timeouts are applied.
pub struct Bind<C, B> {
    ctx: C,
    sensors: telemetry::Sensors,
    executor: Handle,
    req_ids: Arc<AtomicUsize>,
    _p: PhantomData<B>,
}

/// Binds a `Service` from a `SocketAddr` for a pre-determined protocol.
pub struct BindProtocol<C, B> {
    bind: Bind<C, B>,
    protocol: Protocol,
}

/// Mark whether to use HTTP/1 or HTTP/2
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Protocol {
    Http1,
    Http2
}

pub type Service<B> = Reconnect<NewHttp<B>>;

pub type NewHttp<B> = sensor::NewHttp<Client<B>, B, HttpBody>;

pub type HttpResponse = http::Response<sensor::http::ResponseBody<HttpBody>>;

pub type Client<B> = transparency::Client<
    sensor::Connect<transport::Connect>,
    B,
>;

#[derive(Copy, Clone, Debug)]
pub enum BufferSpawnError {
    Inbound,
    Outbound,
}

impl fmt::Display for BufferSpawnError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad(self.description())
    }
}

impl Error for BufferSpawnError {

    fn description(&self) -> &str {
        match *self {
            BufferSpawnError::Inbound =>
                "error spawning inbound buffer task",
            BufferSpawnError::Outbound =>
                "error spawning outbound buffer task",
        }
    }

    fn cause(&self) -> Option<&Error> { None }
}


impl<B> Bind<(), B> {
    pub fn new(executor: Handle) -> Self {
        Self {
            executor,
            ctx: (),
            sensors: telemetry::Sensors::null(),
            req_ids: Default::default(),
            _p: PhantomData,
        }
    }

    pub fn with_sensors(self, sensors: telemetry::Sensors) -> Self {
        Self {
            sensors,
            ..self
        }
    }

    pub fn with_ctx<C>(self, ctx: C) -> Bind<C, B> {
        Bind {
            ctx,
            sensors: self.sensors,
            executor: self.executor,
            req_ids: self.req_ids,
            _p: PhantomData,
        }
    }
}

impl<C: Clone, B> Clone for Bind<C, B> {
    fn clone(&self) -> Self {
        Self {
            ctx: self.ctx.clone(),
            sensors: self.sensors.clone(),
            executor: self.executor.clone(),
            req_ids: self.req_ids.clone(),
            _p: PhantomData,
        }
    }
}


impl<C, B> Bind<C, B> {

    // pub fn ctx(&self) -> &C {
    //     &self.ctx
    // }

    pub fn executor(&self) -> &Handle {
        &self.executor
    }

    // pub fn req_ids(&self) -> &Arc<AtomicUsize> {
    //     &self.req_ids
    // }

    // pub fn sensors(&self) -> &telemetry::Sensors {
    //     &self.sensors
    // }

}

impl<B> Bind<Arc<ctx::Proxy>, B>
where
    B: tower_h2::Body + 'static,
{
    pub fn bind_service(&self, addr: &SocketAddr, protocol: Protocol) -> Service<B> {
        trace!("bind_service addr={}, protocol={:?}", addr, protocol);
        let client_ctx = ctx::transport::Client::new(
            &self.ctx,
            addr,
            conduit_proxy_controller_grpc::common::Protocol::Http,
        );

        // Map a socket address to a connection.
        let connect = self.sensors.connect(
            transport::Connect::new(*addr, &self.executor),
            &client_ctx
        );

        let client = transparency::Client::new(
            protocol,
            connect,
            self.executor.clone(),
        );

        let proxy = self.sensors.http(self.req_ids.clone(), client, &client_ctx);

        // Automatically perform reconnects if the connection fails.
        //
        // TODO: Add some sort of backoff logic.
        Reconnect::new(proxy)
    }
}

// ===== impl BindProtocol =====


impl<C, B> Bind<C, B> {
    pub fn with_protocol(self, protocol: Protocol) -> BindProtocol<C, B> {
        BindProtocol {
            bind: self,
            protocol,
        }
    }
}

impl<B> control::discovery::Bind for BindProtocol<Arc<ctx::Proxy>, B>
where
    B: tower_h2::Body + 'static,
{
    type Request = http::Request<B>;
    type Response = HttpResponse;
    type Error = <Service<B> as tower::Service>::Error;
    type Service = Service<B>;
    type BindError = ();

    fn bind(&self, addr: &SocketAddr) -> Result<Self::Service, Self::BindError> {
        Ok(self.bind.bind_service(addr, self.protocol))
    }
}

