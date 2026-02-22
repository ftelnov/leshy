use crate::config::{Config, DnsProtocol, DnsServerConfig, ServerConfig, ZoneConfig};
use crate::dns::cache::DnsCache;
use crate::routing::RouteManager;
use crate::zones::ZoneMatcher;
use hickory_proto::op::{Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::RecordType;
use hickory_server::authority::MessageResponseBuilder;
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

pub struct DnsHandler {
    config: Arc<Config>,
    matcher: Arc<ZoneMatcher>,
    route_manager: Arc<RwLock<RouteManager>>,
    cache: Arc<DnsCache>,
}

impl DnsHandler {
    pub fn new(config: Config, matcher: ZoneMatcher) -> anyhow::Result<Self> {
        let route_manager = RouteManager::new(config.server.route_aggregation_prefix)?;
        let cache = Arc::new(DnsCache::new(config.server.cache_size));

        Ok(Self {
            config: Arc::new(config),
            matcher: Arc::new(matcher),
            route_manager: Arc::new(RwLock::new(route_manager)),
            cache,
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

    async fn forward_query_tcp(
        &self,
        request: &Request,
        upstream: SocketAddr,
    ) -> Result<Message, ResponseCode> {
        let mut stream = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::net::TcpStream::connect(upstream),
        )
        .await
        .map_err(|_| {
            tracing::warn!(upstream = %upstream, "TCP connect timeout");
            ResponseCode::ServFail
        })?
        .map_err(|e| {
            tracing::error!(upstream = %upstream, error = %e, "Failed to connect TCP to upstream");
            ResponseCode::ServFail
        })?;

        // Build query message
        let mut query_msg = Message::new();
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

        // DNS over TCP: 2-byte big-endian length prefix + message
        let len_prefix = (request_bytes.len() as u16).to_be_bytes();
        stream.write_all(&len_prefix).await.map_err(|e| {
            tracing::error!(upstream = %upstream, error = %e, "Failed to send TCP length prefix");
            ResponseCode::ServFail
        })?;
        stream.write_all(&request_bytes).await.map_err(|e| {
            tracing::error!(upstream = %upstream, error = %e, "Failed to send TCP request");
            ResponseCode::ServFail
        })?;

        // Read response: 2-byte length prefix then message
        let resp_len = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.read_u16(),
        )
        .await
        .map_err(|_| {
            tracing::warn!(upstream = %upstream, "TCP response timeout");
            ResponseCode::ServFail
        })?
        .map_err(|e| {
            tracing::error!(upstream = %upstream, error = %e, "Failed to read TCP response length");
            ResponseCode::ServFail
        })? as usize;

        let mut buf = vec![0u8; resp_len];
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.read_exact(&mut buf),
        )
        .await
        .map_err(|_| {
            tracing::warn!(upstream = %upstream, "TCP response body timeout");
            ResponseCode::ServFail
        })?
        .map_err(|e| {
            tracing::error!(upstream = %upstream, error = %e, "Failed to read TCP response body");
            ResponseCode::ServFail
        })?;

        Message::from_vec(&buf).map_err(|e| {
            tracing::error!(error = %e, "Failed to parse TCP response");
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
        self.config
            .zones
            .iter()
            .any(|z| !z.static_routes.is_empty())
    }

    /// Update config and matcher (for hot reload)
    pub async fn update_config(
        &mut self,
        new_config: Config,
        new_matcher: ZoneMatcher,
    ) -> anyhow::Result<()> {
        // Recreate cache if size changed, otherwise just clear
        if new_config.server.cache_size != self.config.server.cache_size {
            self.cache = Arc::new(DnsCache::new(new_config.server.cache_size));
        } else {
            self.cache.clear();
        }
        self.config = Arc::new(new_config);
        self.matcher = Arc::new(new_matcher);
        tracing::debug!("Handler config updated, cache cleared");
        Ok(())
    }
}

/// Compute cache TTL using the server → zone → global cascade.
fn resolve_cache_ttl(
    server_cfg: Option<&DnsServerConfig>,
    zone: Option<&ZoneConfig>,
    global: &ServerConfig,
    message: &Message,
) -> Duration {
    let min_ttl = server_cfg
        .and_then(|s| s.cache_min_ttl)
        .or(zone.and_then(|z| z.cache_min_ttl))
        .unwrap_or(global.cache_min_ttl);
    let max_ttl = server_cfg
        .and_then(|s| s.cache_max_ttl)
        .or(zone.and_then(|z| z.cache_max_ttl))
        .unwrap_or(global.cache_max_ttl);
    let negative_ttl = server_cfg
        .and_then(|s| s.cache_negative_ttl)
        .or(zone.and_then(|z| z.cache_negative_ttl))
        .unwrap_or(global.cache_negative_ttl);

    if message.response_code() == ResponseCode::NXDomain || message.answers().is_empty() {
        Duration::from_secs(negative_ttl)
    } else {
        let record_min = message
            .answers()
            .iter()
            .map(|r| r.ttl() as u64)
            .min()
            .unwrap_or(min_ttl);
        Duration::from_secs(record_min.clamp(min_ttl, max_ttl))
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

        // Check cache before forwarding
        if self.cache.is_enabled() {
            if let Some(cached) = self.cache.lookup(&qname, qtype) {
                tracing::debug!(qname = qname, qtype = ?qtype, "Cache hit");

                // Still add routes from cached response
                self.add_routes_from_response(&cached, &qname).await;

                // Use the current request's ID so the client matches the response
                let mut header = *cached.header();
                header.set_id(request.id());

                let builder = MessageResponseBuilder::from_message_request(request);
                let response_msg = builder.build(
                    header,
                    cached.answers().iter(),
                    cached.name_servers().iter(),
                    std::iter::empty(),
                    cached.additionals().iter(),
                );
                return response_handle.send_response(response_msg).await.unwrap();
            }
        }

        // Find matching zone and determine upstream servers + protocol
        let zone = self.matcher.find_zone(&qname);
        let (upstreams, protocol): (Vec<(SocketAddr, Option<&DnsServerConfig>)>, DnsProtocol) =
            match &zone {
                Some(z) if !z.dns_servers.is_empty() => {
                    tracing::debug!(
                        qname = qname,
                        zone = z.name,
                        servers = ?z.dns_servers.iter().map(|s| s.address).collect::<Vec<_>>(),
                        protocol = ?z.dns_protocol,
                        "Routing to zone DNS"
                    );
                    let ups = z.dns_servers.iter().map(|s| (s.address, Some(s))).collect();
                    (ups, z.dns_protocol)
                }
                _ => {
                    tracing::debug!(
                        qname = qname,
                        upstreams = ?self.config.server.default_upstream,
                        "Routing to default DNS"
                    );
                    let ups = self
                        .config
                        .server
                        .default_upstream
                        .iter()
                        .map(|&a| (a, None))
                        .collect();
                    (ups, DnsProtocol::Udp)
                }
            };

        // Sequential failover: try servers in order, fail only when all exhausted
        let mut last_err = ResponseCode::ServFail;
        let mut result: Option<(Message, Option<&DnsServerConfig>)> = None;
        for (i, (upstream, server_cfg)) in upstreams.iter().enumerate() {
            let res = match protocol {
                DnsProtocol::Udp => self.forward_query(request, *upstream).await,
                DnsProtocol::Tcp => self.forward_query_tcp(request, *upstream).await,
            };
            match res {
                Ok(response) => {
                    result = Some((response, *server_cfg));
                    break;
                }
                Err(rcode) => {
                    tracing::warn!(
                        qname = qname,
                        upstream = %upstream,
                        rcode = ?rcode,
                        remaining = upstreams.len() - i - 1,
                        "Upstream failed, trying next"
                    );
                    last_err = rcode;
                }
            }
        }

        match result {
            Some((response, server_cfg)) => {
                tracing::debug!(
                    qname = qname,
                    answers = response.answers().len(),
                    "Got response"
                );

                // Add routes for resolved IPs (async, don't wait)
                self.add_routes_from_response(&response, &qname).await;

                // Cache the response (skip ServFail)
                if self.cache.is_enabled() && response.response_code() != ResponseCode::ServFail {
                    let ttl = resolve_cache_ttl(
                        server_cfg,
                        zone.as_deref(),
                        &self.config.server,
                        &response,
                    );
                    self.cache.insert(&qname, qtype, response.clone(), ttl);
                }

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
            None => {
                tracing::error!(qname = qname, rcode = ?last_err, "All upstreams failed");
                let builder = MessageResponseBuilder::from_message_request(request);
                let response = builder.error_msg(request.header(), last_err);
                response_handle.send_response(response).await.unwrap()
            }
        }
    }
}
