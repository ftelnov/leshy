use crate::dns::handler::DnsHandler;
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use hickory_server::ServerFuture;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

/// Wrapper around DnsHandler that allows Arc<RwLock<>> access
pub struct ReloadableHandler {
    handler: Arc<RwLock<DnsHandler>>,
}

impl ReloadableHandler {
    pub fn new(handler: Arc<RwLock<DnsHandler>>) -> Self {
        Self { handler }
    }
}

#[async_trait::async_trait]
impl RequestHandler for ReloadableHandler {
    async fn handle_request<R: ResponseHandler>(
        &self,
        request: &Request,
        response_handle: R,
    ) -> ResponseInfo {
        let handler = self.handler.read().await;
        handler.handle_request(request, response_handle).await
    }
}

pub struct DnsServer {
    server: ServerFuture<ReloadableHandler>,
}

impl DnsServer {
    pub async fn new(
        listen_addr: SocketAddr,
        handler: Arc<RwLock<DnsHandler>>,
    ) -> anyhow::Result<Self> {
        let reloadable_handler = ReloadableHandler::new(handler);
        let mut server = ServerFuture::new(reloadable_handler);

        // Bind UDP socket
        let socket = UdpSocket::bind(listen_addr).await?;
        tracing::info!(addr = %listen_addr, "DNS server listening on UDP");
        server.register_socket(socket);

        Ok(Self { server })
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        self.server.block_until_done().await?;
        Ok(())
    }
}
