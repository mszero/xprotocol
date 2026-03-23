# XProtocol

分布式服务通信框架 - 基于 pingora 构建的高性能、可靠性服务间通信库。

## 特性

- **多协议支持**: Unix Domain Socket (UDS)、TCP、TLS、gRPC
- **连接池管理**: 基于 pingora 的高性能连接池，支持多种策略
- **服务发现**: 内置负载均衡 (RoundRobin/Random/一致性哈希)
- **流式处理**: 支持 Server-Sent Events 风格的流式响应
- **断路器**: 内置断路器模式，防止级联故障
- **指标监控**: P50/P95/P99 延迟、错误率、活动连接数
- **优雅关闭**: 支持信号处理的优雅停机
- **Webhook**: 服务事件通知
- **Schema 自省**: 运行时服务能力发现
- **多编码**: JSON + MessagePack (可选)
- **安全认证**: Token/API Key 认证 + 权限管理
- **服务端限流**: 基于客户端的速率限制
- **配置管理**: TOML/YAML 配置文件支持
- **告警系统**: 阈值告警 + 自定义回调

## 快速开始

### 安装

```toml
[dependencies]
protocol = "0.1"
tokio = { version = "1", features = ["full"] }
serde_json = "1"
```

### 服务端

```rust
use protocol::{ServiceHandler, ServiceRequest, ServiceResponse, UdsServer};
use std::sync::Arc;

struct MyService;

#[protocol::async_trait]
impl ServiceHandler for MyService {
    fn service_name(&self) -> &str {
        "my-service"
    }

    async fn handle(&self, request: ServiceRequest) -> ServiceResponse {
        ServiceResponse::success(&request, serde_json::json!({
            "message": "Hello from server!"
        }))
    }
}

#[tokio::main]
async fn main() {
    let server = UdsServer::new("/tmp/my-service.sock");
    server.run(Arc::new(MyService)).await.unwrap();
}
```

### 客户端

```rust
use protocol::{UdsClient, ServiceRequest};

#[tokio::main]
async fn main() {
    let mut client = UdsClient::new("/tmp/my-service.sock");
    let response = client
        .call("my-service", "method", serde_json::json!({"key": "value"}))
        .await
        .unwrap();
    println!("Response: {:?}", response);
}
```

## 核心概念

### ServiceHandler

服务处理器接口，所有业务逻辑都通过实现此 trait 来处理：

```rust
#[protocol::async_trait]
pub trait ServiceHandler: Send + Sync {
    fn service_name(&self) -> &str;
    
    async fn on_handshake(&self, request: HandshakeRequest) -> HandshakeResponse;
    
    async fn handle(&self, request: ServiceRequest) -> ServiceResponse;
    
    fn health_status(&self) -> HealthStatus;
}
```

### 协议消息

```rust
// 请求消息
pub struct ServiceRequest {
    pub request_id: String,
    pub correlation_id: String,
    pub service_name: String,
    pub method: String,
    pub payload: serde_json::Value,
    pub topic: Option<String>,
}

// 响应消息
pub struct ServiceResponse {
    pub request_id: String,
    pub correlation_id: String,
    pub status: String,
    pub payload: serde_json::Value,
    pub error: Option<String>,
}
```

### 线协议

二进制长度前缀协议，格式:
```
[4 bytes: length][1 byte: type][1 byte: version][N bytes: payload]
```

## 模块架构

```
src/
├── error.rs           # 错误类型定义
├── protocol.rs        # 核心协议消息
├── wire.rs            # 线协议实现
├── handler.rs         # ServiceHandler trait
├── server.rs         # UDS 服务器 (含限流、连接限制)
├── client.rs         # UDS 客户端 + 可靠连接
├── pool.rs           # 连接池实现
├── tcp.rs            # TCP 服务器/客户端
├── socket.rs         # Socket 路径管理
├── session.rs        # 会话管理
├── streaming.rs      # 流式处理
├── schema.rs         # Schema 自省
├── webhook.rs        # Webhook 通知
├── discovery.rs      # 服务发现 + 负载均衡
├── circuit_breaker.rs  # 断路器
├── shutdown.rs       # 优雅关闭
├── runtime.rs        # Tokio 运行时配置
├── utils.rs          # 工具类
├── grpc.rs           # gRPC 支持
├── tls.rs            # TLS 支持
├── auth.rs           # 认证/授权
├── metrics.rs        # 监控指标 (P99、直方图)
├── config.rs         # 配置管理 (TOML/YAML)
└── wire.rs           # 线协议
```

## 高级功能

### 连接池

```rust
// 基础连接池
let pool = UdsClientPool::new("/tmp/service.sock", 100); // max 100 并发

// 生产级连接池 (含断路器)
let pool = ProductionPool::new("/tmp/service.sock");
let config = PoolConfig {
    max_concurrent: 100,
    request_timeout: Duration::from_secs(30),
    circuit_breaker_threshold: 5,
    ..Default::default()
};
let pool = ProductionPool::with_config("/tmp/service.sock", config);

// 基于 Topic 的串行池
let pool = TopicBasedPool::new("/tmp/service.sock", 100);
pool.call_with_topic("service", "method", payload, Some("order-123")).await;
```

### 服务发现与负载均衡

```rust
use protocol::{ServiceDiscovery, LoadBalancingStrategy, ServiceEndpoint};

let sd = ServiceDiscovery::new()
    .with_strategy(LoadBalancingStrategy::ConsistentHash);

sd.register(ServiceEndpoint::new("user-service", "127.0.0.1:8001").with_weight(10));
sd.register(ServiceEndpoint::new("user-service", "127.0.0.1:8002").with_weight(5));

// 选择后端
if let Some(backend) = sd.select("user-service") {
    println!("Selected: {:?}", backend);
}

// 一致性哈希 (相同 key 路由到相同后端)
if let Some(backend) = sd.select_with_key("user-service", b"user-123") {
    println!("Selected for user-123: {:?}", backend);
}
```

### 流式响应

```rust
// 服务端
#[protocol::async_trait]
impl StreamingHandler for MyService {
    async fn handle_streaming(
        &self,
        request: ServiceRequest,
        mode: StreamMode,
    ) -> Vec<StreamMessage> {
        vec![
            StreamMessage::new(&request.request_id, 0, serde_json::json!({"status": "started"})),
            StreamMessage::new(&request.request_id, 1, serde_json::json!({"status": "processing"})),
            StreamMessage::new(&request.request_id, 2, serde_json::json!({"result": "done"})),
        ]
    }
}

// 客户端
let messages = client
    .call_streaming("service", "method", payload, StreamMode::Result)
    .await
    .unwrap();
```

### 优雅关闭

```rust
use protocol::{GracefulShutdown, shutdown_signal, run_with_graceful_shutdown};

let graceful = GracefulShutdown::new();

// 触发关闭
graceful.trigger();

// 或使用 Ctrl+C
tokio::spawn(shutdown_signal());

// 运行带优雅关闭的服务器
run_with_graceful_shutdown(handler, server.run()).await;
```

### Webhook 通知

```rust
use protocol::{WebhookConfig, WebhookService, WebhookEvent};

let webhook = WebhookService::new()
    .with_webhook(
        WebhookConfig::new("https://hooks.example.com/webhook")
            .with_timeout(30)
            .with_retry_policy(WebhookRetryPolicy::exponential(3))
    );

webhook.notify_request_received("req-123", "service", "method").await;
webhook.notify_response_sent("req-123", "service", "success", 50).await;
```

### 断路器

```rust
use protocol::{CircuitBreaker, CircuitState};

let cb = CircuitBreaker::new(5).with_timeout(Duration::from_secs(30));

cb.record_failure();
cb.record_failure();

if cb.state() == CircuitState::Open {
    println!("Circuit opened!");
}

cb.can_execute()?; // 如果打开则返回错误
```

### Schema 自省

```rust
use protocol::{ServiceSchema, ServiceCapability};

let schema = ServiceSchema::new("my-service", "1.0")
    .with_input_schema(serde_json::json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" }
        }
    }))
    .with_capabilities(vec![
        ServiceCapability {
            name: "streaming".to_string(),
            description: "支持流式响应".to_string(),
            version: Some("1.0".to_string()),
        }
    ])
    .with_streaming_modes(vec![StreamMode::Result, StreamMode::Messages]);
```

## 配置

### ServerConfig

```rust
ServerConfig {
    request_timeout: Duration::from_secs(30),
    idle_timeout: Duration::from_secs(300),
    max_request_size: 16 * 1024 * 1024,  // 16MB
    webhook_url: None,
    enable_streaming: true,
    enable_schema_introspection: true,
}
```

### ConnectionConfig

```rust
ConnectionConfig {
    connect_timeout: Duration::from_secs(5),
    request_timeout: Duration::from_secs(30),
    idle_timeout: Duration::from_secs(300),
    heartbeat_interval: Duration::from_secs(30),
    max_reconnect_attempts: 5,
    reconnect_delay: Duration::from_secs(1),
}
```

## 消息类型

| 类型 | 值 | 描述 |
|------|-----|------|
| MSG_HANDSHAKE | 0x01 | 握手请求 |
| MSG_HANDSHAKE_ACK | 0x02 | 握手响应 |
| MSG_REQUEST | 0x10 | 服务请求 |
| MSG_RESPONSE | 0x11 | 服务响应 |
| MSG_STREAM_REQUEST | 0x12 | 流式请求 |
| MSG_STREAM_START | 0x13 | 流开始 |
| MSG_STREAM_CHUNK | 0x14 | 流数据块 |
| MSG_STREAM_END | 0x15 | 流结束 |
| MSG_ERROR | 0x1F | 错误消息 |
| MSG_PING | 0x40 | 心跳 |
| MSG_PONG | 0x41 | 心跳响应 |
| MSG_CLOSE | 0x50 | 关闭连接 |
| MSG_SCHEMA_REQUEST | 0x60 | Schema 请求 |
| MSG_SCHEMA_RESPONSE | 0x61 | Schema 响应 |

## 性能优化

### 运行时配置

```rust
use protocol::{ProtocolRuntime, RuntimeType};

let runtime = ProtocolRuntime::new()
    .with_type(RuntimeType::MultiThreadNoWorkStealing)
    .with_threads(8);

runtime.run(async {
    // 你的代码
}).await;
```

### 指标监控

```rust
let metrics = pool.metrics();
println!("Total requests: {}", metrics.total_requests);
println!("Success rate: {:.2}%", metrics.success_rate);
println!("Active: {}", metrics.active_requests);
```

### 增强监控 (P99 直方图)

```rust
use protocol::{MetricsCollector, ServiceMetrics, AlertManager};

let collector = MetricsCollector::new();
let alert_manager = AlertManager::new();

// 添加告警规则
alert_manager.add_rule(AlertRule {
    name: "high_latency".to_string(),
    metric: "my-service".to_string(),
    condition: AlertCondition::LatencyP99Above(1000),
    cooldown: Duration::from_secs(60),
});

// 检查告警
alert_manager.check(&collector);

// 获取 P99 延迟
let metrics = collector.get_or_create_service("my-service");
let snapshot = metrics.snapshot();
println!("P99 latency: {}ms", snapshot.latency_p99_ms);
```

### 安全认证

```rust
use protocol::{TokenAuthenticator, ApiKeyAuthenticator, CompositeAuthenticator, AuthContext};

let authenticator = CompositeAuthenticator::new()
    .add(TokenAuthenticator::new())
    .add(ApiKeyAuthenticator::new());

let ctx = authenticator.authenticate(&Credentials {
    client_id: "client-1".to_string(),
    token: Some("my-token".to_string()),
    api_key: None,
})?;
```

### 配置文件

```rust
use protocol::{Config, ConfigBuilder};

// 从文件加载
let config = Config::from_file("config.toml")?;

// 或使用构建器
let config = ConfigBuilder::new()
    .with_server_config(ServerConfig {
        max_connections: 10000,
        ..Default::default()
    })
    .build();

// TOML/YAML 序列化
let toml = config.to_toml()?;
```

## 依赖

核心依赖:
- `tokio` - 异步运行时
- `pingora` - 负载均衡与连接池
- `serde` - 序列化
- `uuid` - ID 生成
- `tracing` - 日志追踪
- `reqwest` - HTTP 客户端

可选依赖:
- `msgpack` - MessagePack 编码
- `tonic` - gRPC 支持
- `native-tls` / `rustls` - TLS 支持

## 许可证

MIT
