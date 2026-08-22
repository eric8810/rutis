//! M2-3 llm 缝端到端:Rust 侧 LlmSeam(ChunkedLlm scripted 模型)↔ 真 dsh
//! `LlmRuntime`(vitest 宿主进程)。宿主注册桥 adapter 并消费
//! `ctx.llm.stream(...)`,vitest 退出码即验收结论。TS 侧的断言(块序列/
//! 文本重组/finish)在 dsh 仓 `experiments/m2-host/llm-seam.spec.ts`。
//!
//! env:`DSH_ROOT`、`NODE`(可选);缺失即 panic(不静默跳过)。

use std::sync::Arc;
use std::time::Duration;

use aimux_core::error::AiMuxError;
use aimux_core::language_model::LanguageModel;
use aimux_core::options::CallOptions;
use aimux_core::result::{GenerateResult, StreamResult};
use aimux_core::stream_part::StreamPart;
use aimux_core::types::{FinishReason, FinishReasonUnified, Usage};
use async_trait::async_trait;
use rutis_cordis::{Bridge, BridgeConfig, ExpectedHost, TcpWire};
use rutis_dsh::LlmSeam;
use serde_json::json;

struct ChunkedLlm;

#[async_trait]
impl LanguageModel for ChunkedLlm {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn model_id(&self) -> &str {
        "chunked"
    }

    async fn do_generate(&self, _options: &CallOptions) -> Result<GenerateResult, AiMuxError> {
        Err(AiMuxError::Other("stream-only".into()))
    }

    async fn do_stream(&self, _options: &CallOptions) -> Result<StreamResult, AiMuxError> {
        let deltas: Vec<&str> = vec!["hel", "lo ", "m2"];
        let stream = async_stream::stream! {
            yield Ok(StreamPart::StreamStart { warnings: Vec::new() });
            for delta in deltas {
                yield Ok(StreamPart::TextDelta { id: "text-0".to_string(), delta: delta.to_string(), provider_metadata: None });
            }
            yield Ok(StreamPart::Finish {
                finish_reason: FinishReason { unified: FinishReasonUnified::Stop, raw: None },
                usage: Usage::default(),
                provider_metadata: None,
            });
        };
        Ok(StreamResult { stream: Box::pin(stream), request_body: None, response_headers: None })
    }
}

fn node_binary() -> String {
    if let Ok(node) = std::env::var("NODE") {
        return node
    }
    let output = std::process::Command::new("where")
        .arg("node")
        .output()
        .or_else(|_| std::process::Command::new("which").arg("node").output())
        .expect("locate node");
    assert!(output.status.success(), "node not found (set NODE or PATH)");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .expect("node path")
        .to_owned()
}

#[tokio::test]
async fn llm_seam_end_to_end_with_real_dsh_llm_runtime() {
    if std::env::var("RUTIS_SKIP_NODE_E2E").as_deref() == Ok("1") {
        eprintln!("RUTIS_SKIP_NODE_E2E=1 — skipping llm seam e2e");
        return
    }
    let dsh_root = std::env::var("DSH_ROOT").expect("DSH_ROOT (deepseek-harness checkout) required");
    let node = node_binary();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let vitest = format!("{dsh_root}/node_modules/vitest/vitest.mjs");
    let mut child = tokio::process::Command::new(&node)
        .arg(vitest)
        .arg("run")
        .arg("--config")
        .arg(format!("{dsh_root}/experiments/m2-host/vitest.config.ts"))
        .env("BRIDGE_PORT", port.to_string())
        .current_dir(&dsh_root)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("spawn vitest host");

    let (stream, _) = tokio::time::timeout(Duration::from_secs(30), listener.accept())
        .await
        .expect("host connects within 30s")
        .expect("accept");
    let seam = LlmSeam::new(Arc::new(ChunkedLlm));
    let mut bridge = Bridge::start(
        Box::new(TcpWire::from_stream(stream)),
        BridgeConfig::default(),
        seam.hooks(),
        ExpectedHost::protocol(1),
        json!({ "services": ["llm"], "wfKinds": [], "scopes": [] }),
    );
    seam.attach(bridge.clone());
    bridge.ready().await.expect("handshake with vitest host");

    // 宿主跑完两个断言(vitest 退出码 0)即验收通过;宿主死亡先到则失败。
    let status = tokio::time::timeout(Duration::from_secs(120), child.wait())
        .await
        .expect("host finishes within 120s")
        .expect("wait host");
    assert!(status.success(), "vitest host failed — see TS-side output above");
    drop(bridge);
}
