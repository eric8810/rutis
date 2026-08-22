//! M2-4 完整 turn 验收:两轮流(tool-call 过线 → 宿主执行 → 回喂 → 终轮
//! 文本)、GenerateOptions→CallOptions 完整映射断言、usage/finish 逐字段
//! (L3)、console.log 注入(帧流不损坏,TS 侧)。宿主是 dsh 仓
//! `experiments/m2-host/llm-seam.spec.ts`(真 dsh LlmRuntime),vitest 退出
//! 码即 TS 侧结论;映射断言在 Rust 侧(记录器直读)。
//!
//! env:`DSH_ROOT`、`NODE`(可选);缺失即 panic(不静默跳过)。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aimux_core::content::ContentPart;
use aimux_core::error::AiMuxError;
use aimux_core::language_model::LanguageModel;
use aimux_core::options::CallOptions;
use aimux_core::result::{GenerateResult, StreamResult};
use aimux_core::stream_part::StreamPart;
use aimux_core::types::{FinishReason, FinishReasonUnified, TokenUsage, Usage};
use async_trait::async_trait;
use rutis_cordis::{Bridge, BridgeConfig, ExpectedHost, TcpWire};
use rutis_dsh::LlmSeam;
use serde_json::json;

/// 两轮 scripted:第一次(prompt 未含工具结果)产出 tool-call;第二次
/// (回喂后)产出终文本。记录每次收到的 CallOptions 供映射断言。
struct TwoTurnLlm {
    calls: Mutex<Vec<CallOptions>>,
}

#[async_trait]
impl LanguageModel for TwoTurnLlm {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn model_id(&self) -> &str {
        "two-turn"
    }

    async fn do_generate(&self, _options: &CallOptions) -> Result<GenerateResult, AiMuxError> {
        Err(AiMuxError::Other("stream-only".into()))
    }

    async fn do_stream(&self, options: &CallOptions) -> Result<StreamResult, AiMuxError> {
        self.calls
            .lock()
            .unwrap()
            .push(serde_json::from_value(serde_json::to_value(options).expect("encode")).expect("decode"));
        let fed_tool_result = options.prompt.iter().any(|m| {
            m.content.iter().any(|p| matches!(p, ContentPart::Text { text, .. } if text.contains("TOOL_RESULT:")))
        });
        let stream: std::pin::Pin<Box<dyn futures::Stream<Item = Result<StreamPart, AiMuxError>> + Send>> =
            if fed_tool_result {
                Box::pin(async_stream::stream! {
                yield Ok(StreamPart::TextDelta { id: "text-0".to_string(), delta: "all ".to_string(), provider_metadata: None });
                yield Ok(StreamPart::TextDelta { id: "text-0".to_string(), delta: "done".to_string(), provider_metadata: None });
                yield Ok(StreamPart::Finish {
                    finish_reason: FinishReason { unified: FinishReasonUnified::Stop, raw: None },
                    usage: Usage {
                        input_tokens: TokenUsage { total: Some(11), no_cache: Some(9), cache_read: Some(2), ..TokenUsage::default() },
                        output_tokens: TokenUsage { total: Some(7), ..TokenUsage::default() },
                        raw: None,
                    },
                    provider_metadata: None,
                });
                })
            } else {
                Box::pin(async_stream::stream! {
                yield Ok(StreamPart::ToolCall {
                    tool_call_id: "call-1".to_string(),
                    tool_name: "echo_tool".to_string(),
                    input: json!({ "text": "m2" }),
                    provider_executed: None,
                    dynamic: None,
                    thought_signature: None,
                    provider_metadata: None,
                });
                yield Ok(StreamPart::Finish {
                    finish_reason: FinishReason { unified: FinishReasonUnified::ToolCalls, raw: None },
                    usage: Usage::default(),
                    provider_metadata: None,
                });
                })
            };
        Ok(StreamResult { stream, request_body: None, response_headers: None })
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
async fn full_turn_with_tool_call_and_backfeed_end_to_end() {
    if std::env::var("RUTIS_SKIP_NODE_E2E").as_deref() == Ok("1") {
        eprintln!("RUTIS_SKIP_NODE_E2E=1 — skipping full turn e2e");
        return
    }
    let dsh_root =
        std::env::var("DSH_ROOT").expect("DSH_ROOT (deepseek-harness checkout) required");
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
    let llm = Arc::new(TwoTurnLlm { calls: Mutex::new(Vec::new()) });
    let seam = LlmSeam::new(llm.clone());
    let mut bridge = Bridge::start(
        Box::new(TcpWire::from_stream(stream)),
        BridgeConfig::default(),
        seam.hooks(),
        ExpectedHost::protocol(1),
        json!({ "services": ["llm"], "wfKinds": [], "scopes": [] }),
    );
    seam.attach(bridge.clone());
    bridge.ready().await.expect("handshake with vitest host");

    // TS 侧跑完整 turn(含 console.log 注入断言);vitest 退出码即结论。
    let status = tokio::time::timeout(Duration::from_secs(120), child.wait())
        .await
        .expect("host finishes within 120s")
        .expect("wait host");
    assert!(status.success(), "vitest host failed — see TS-side output above");

    // Rust 侧 L3 映射断言:记录的调用(it2 两轮 + it3 注入流一次)。
    let calls = llm.calls.lock().unwrap();
    assert_eq!(calls.len(), 3, "two-turn flow + noisy-stream call recorded");
    let first = serde_json::to_value(&calls[0]).expect("encode");
    assert_eq!(first["prompt"][0]["role"], "system", "system 映射: {first}");
    assert_eq!(first["prompt"][0]["content"][0]["text"], "You are the m2 acceptance host.");
    assert_eq!(first["prompt"][1]["role"], "user");
    assert_eq!(first["prompt"][1]["content"][0]["text"], "run the acceptance turn");
    assert_eq!(first["tools"][0]["type"], "function", "tools 映射: {first}");
    assert_eq!(first["tools"][0]["name"], "echo_tool");
    assert_eq!(first["tools"][0]["input_schema"]["type"], "object");
    let second = serde_json::to_value(&calls[1]).expect("encode");
    let backfed = second["prompt"]
        .as_array()
        .expect("prompt array")
        .iter()
        .any(|m| m["content"].to_string().contains("TOOL_RESULT: m2"));
    assert!(backfed, "工具结果回喂映射: {second}");
    drop(bridge);
}
