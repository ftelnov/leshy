use crate::config::Config;
use crate::routing::RouteManager;
use crate::zones::ZoneMatcher;
use hickory_proto::op::{Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::RecordType;
use hickory_server::authority::MessageResponseBuilder;
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct DnsHandler {
    config: Arc<Config>,
    matcher: Arc<ZoneMatcher>,
    route_manager: Arc<RwLock<RouteManager>>,
}

impl DnsHandler {
    pub fn new(config: Config, matcher: ZoneMatcher) -> anyhow::Result<Self> {
        let route_manager = RouteManager::new()?;

        Ok(Self {
            config: Arc::new(config),
            matcher: Arc::new(matcher),
            route_manager: Arc::new(RwLock::new(route_manager)),
        })
    }

    async fn forward_query(
        &self,
        request: &Request,
        upstream: SocketAddr,
    ) -> Result<Message, ResponseCode> {
        // Create UDP socket
        let socket = tokio::net::UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "Failed to bind UDP socket");
                ResponseCode::ServFail
            })?;

        // Connect to upstream
        socket.connect(upstream).await.map_err(|e| {
            tracing::error!(upstream = %upstream, error = %e, "Failed to connect to upstream");
            ResponseCode::ServFail
        })?;

        // Serialize the DNS query message
        let query_msg = Message::new();
        let mut query_msg = query_msg.clone();
        query_msg.add_query(hickory_proto::op::Query::query(
            request.query().name().clone().into(),
            request.query().query_type(),
        ));
        query_msg.set_id(request.id());
        query_msg.set_message_type(MessageType::Query);
        query_msg.set_op_code(request.op_code());
        query_msg.set_recursion_desired(request.recursion_desired());

        let request_bytes = query_msg.to_vec().map_err(|e| {
            tracing::error!(error = %e, "Failed to serialize query");
            ResponseCode::ServFail
        })?;

        // Send request
        socket.send(&request_bytes).await.map_err(|e| {
            tracing::error!(upstream = %upstream, error = %e, "Failed to send request");
            ResponseCode::ServFail
        })?;

        // Receive response with timeout
        let mut buf = vec![0u8; 4096];
        let len = tokio::time::timeout(std::time::Duration::from_secs(5), socket.recv(&mut buf))
            .await
            .map_err(|_| {
                tracing::warn!(upstream = %upstream, "Query timeout");
                ResponseCode::ServFail
            })?
            .map_err(|e| {
                tracing::error!(upstream = %upstream, error = %e, "Failed to receive response");
                ResponseCode::ServFail
            })?;

        // Parse response
        Message::from_vec(&buf[..len]).map_err(|e| {
            tracing::error!(error = %e, "Failed to parse response");
            ResponseCode::ServFail
        })
    }

    async fn add_routes_from_response(&self, message: &Message, qname: &str) {
        let zone = match self.matcher.find_zone(qname) {
            Some(z) => z,
            None => return, // No zone match, no routing needed
        };

        // Extract A and AAAA records from answers
        let ips: Vec<IpAddr> = message
            .answers()
            .iter()
            .filter_map(|record| match record.record_type() {
                RecordType::A => record
                    .data()
                    .and_then(|d| d.as_a())
                    .map(|a| IpAddr::V4(a.0)),
                RecordType::AAAA => record
                    .data()
                    .and_then(|d| d.as_aaaa())
                    .map(|aaaa| IpAddr::V6(aaaa.0)),
                _ => None,
            })
            .collect();

        if ips.is_empty() {
            tracing::debug!(qname = qname, "No A/AAAA records in response");
            return;
        }

        // Add routes in background (don't block DNS response)
        let route_manager = Arc::clone(&self.route_manager);
        let zone_clone = zone.clone();
        let qname = qname.to_string();

        tokio::spawn(async move {
            let manager = route_manager.read().await;
            for ip in ips {
                if let Err(e) = manager.add_route(ip, &zone_clone).await {
                    tracing::warn!(
                        ip = %ip,
                        zone = zone_clone.name,
                        qname = qname,
                        error = %e,
                        "Failed to add route"
                    );
                }
            }
        });
    }

    /// Get current config
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Cleanup routes for a specific zone
    pub async fn cleanup_zone(&self, zone_name: &str) -> anyhow::Result<()> {
        let manager = self.route_manager.read().await;
        manager.cleanup_zone(zone_name).await
    }

    /// Apply static routes for all zones that have them.
    /// Returns the number of failed routes (0 = all applied successfully).
    pub async fn apply_static_routes(&self) -> usize {
        let route_manager = self.route_manager.read().await;
        let mut failures = 0;
        for zone in &self.config.zones {
            for cidr in &zone.static_routes {
                if let Err(e) = route_manager.add_static_route(cidr, zone).await {
                    tracing::warn!(
                        cidr = cidr,
                        zone = zone.name,
                        error = %e,
                        "Failed to add static route"
                    );
                    failures += 1;
                }
            }
        }
        failures
    }

    /// Returns true if any zone has static routes configured
    pub fn has_static_routes(&self) -> bool {
        self.config.zones.iter().any(|z| !z.static_routes.is_empty())
    }

    /// Update config and matcher (for hot reload)
    pub async fn update_config(
        &mut self,
        new_config: Config,
        new_matcher: ZoneMatcher,
    ) -> anyhow::Result<()> {
        self.config = Arc::new(new_config);
        self.matcher = Arc::new(new_matcher);
        tracing::debug!("Handler config updated");
        Ok(())
    }
}

#[async_trait::async_trait]
impl RequestHandler for DnsHandler {
    async fn handle_request<R: ResponseHandler>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        // Only handle queries
        if request.op_code() != OpCode::Query {
            let builder = MessageResponseBuilder::from_message_request(request);
            let response = builder.error_msg(request.header(), ResponseCode::NotImp);
            return response_handle.send_response(response).await.unwrap();
        }

        // Get query name - convert to string
        let qname = request.query().name().to_string();
        let qtype = request.query().query_type();

        tracing::info!(qname = qname, qtype = ?qtype, "Received query");

        // Find matching zone
        let zone = self.matcher.find_zone(&qname);
        let upstream = match &zone {
            Some(z) if !z.dns_servers.is_empty() => {
                // Use zone's DNS servers (pick first for now, TODO: load balance)
                tracing::debug!(
                    qname = qname,
                    zone = z.name,
                    upstream = ?z.dns_servers[0],
                    "Routing to zone DNS"
                );
                z.dns_servers[0]
            }
            _ => {
                // Use default upstream (pick first for now, TODO: load balance)
                tracing::debug!(
                    qname = qname,
                    upstream = ?self.config.server.default_upstream[0],
                    "Routing to default DNS"
                );
                self.config.server.default_upstream[0]
            }
        };

        // Forward query
        match self.forward_query(request, upstream).await {
            Ok(response) => {
                tracing::debug!(
                    qname = qname,
                    answers = response.answers().len(),
                    "Got response"
                );

                // Add routes for resolved IPs (async, don't wait)
                self.add_routes_from_response(&response, &qname).await;

                // Convert Message to MessageResponse
                let builder = MessageResponseBuilder::from_message_request(request);
                let response_msg = builder.build(
                    *response.header(),
                    response.answers().iter(),
                    response.name_servers().iter(),
                    std::iter::empty(),
                    response.additionals().iter(),
                );

                response_handle.send_response(response_msg).await.unwrap()
            }
            Err(rcode) => {
                tracing::error!(qname = qname, rcode = ?rcode, "Query failed");
                let builder = MessageResponseBuilder::from_message_request(request);
                let response = builder.error_msg(request.header(), rcode);
                response_handle.send_response(response).await.unwrap()
            }
        }
    }
}
