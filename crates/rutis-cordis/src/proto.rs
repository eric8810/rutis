//! cordis 桥协议词汇(设计 v3.2 §三之二):hello 基座面、能力集、事件
//! 分发模式、waterfall kind、装载仲裁。本文件是 cordis 概念的 Rust 面,
//! 零 dsh 知识——dsh 词汇(dshSemver、dsh 服务集、会话字段语义)在
//! `rutis-dsh` crate。

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::rpc::ProtoError;

// ---------------------------------------------------------------------------
// 握手基座面(§三 规则 1 的基座级)
// ---------------------------------------------------------------------------

/// 宿主在 `hello` 里申报的基座能力集。dsh 部署附加的 `dsh` 节由
/// `rutis-dsh` 解析(serde 容忍未知字段,基座不认它)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PeerCaps {
    #[serde(default)]
    pub services: BTreeSet<String>,
    #[serde(default, rename = "wfKinds")]
    pub wf_kinds: Vec<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl PeerCaps {
    /// 装载期能力集求差(§三 规则 1):`injects` 中宿主没有的服务。
    pub fn missing_services<'a>(&self, injects: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        injects
            .into_iter()
            .filter(|name| !self.services.contains(*name))
            .map(str::to_owned)
            .collect()
    }

    /// 从宿主 hello 原始参数解析基座能力集(`ready()` 返回的就是原始参数)。
    pub fn from_hello_params(params: &Value) -> Result<PeerCaps, ProtoError> {
        serde_json::from_value(
            params.get("caps").cloned().unwrap_or_else(|| serde_json::json!({})),
        )
        .map_err(|e| ProtoError::Wire(format!("hello caps malformed: {e}")))
    }
}

/// 宿主 `hello` 参数的基座形状。
#[derive(Debug, Clone, Deserialize)]
pub struct HelloCaps {
    pub protocol: u32,
    pub base: String,
    #[serde(rename = "baseSemver")]
    pub base_semver: String,
    #[serde(default)]
    pub stack: Vec<String>,
    #[serde(default)]
    pub caps: PeerCaps,
}

/// 握手扩展校验:拿宿主 hello 的**原始参数**(含基座不认的节点,如 dsh
/// 部署的 `dsh` 节)做上层校验。两级握手的挂点——`rutis-dsh` 用它校验
/// dsh 节,失败同样算握手失败。
pub type HelloVerify = Arc<dyn Fn(&Value) -> Result<(), String> + Send + Sync>;

/// Rust 侧对宿主的握手期望(错配在握手期报错,§三 规则 1)。基座级:
/// `protocol` 精确、`base` ∈ min-cordis|cordis;dshSemver 属 dsh 面,经
/// `verify` 挂入。
#[derive(Clone)]
pub struct ExpectedHost {
    pub protocol: u32,
    pub base: Option<String>,
    pub verify: Option<HelloVerify>,
}

impl ExpectedHost {
    pub fn protocol(protocol: u32) -> ExpectedHost {
        ExpectedHost { protocol, base: None, verify: None }
    }
}

impl std::fmt::Debug for ExpectedHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExpectedHost")
            .field("protocol", &self.protocol)
            .field("base", &self.base)
            .field("verify", &self.verify.as_ref().map(|_| ".."))
            .finish()
    }
}

// ---------------------------------------------------------------------------
// 事件申报(§三 规则 5:mode 三值,cordis 事件总线四分发语义)
// ---------------------------------------------------------------------------

/// 事件分发模式。v1 底座方向只消费 emit;parallel/serial 为锈化波预留,
/// 不得静默降级为 ntf。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvtMode {
    Emit,
    Parallel,
    Serial,
}

/// `evt/on` 申报里的一条事件订阅。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvtDeclaration {
    pub name: String,
    pub mode: EvtMode,
}

// ---------------------------------------------------------------------------
// waterfall 申报(§五 类型学:kind 三型,v1.1 帧字段即刻预留)
// ---------------------------------------------------------------------------

/// waterfall 监听的三种型:decide(否决链)、around(环绕链,续延点拿真
/// 终态)、stream(流式伴随)。锈化波启用,协议字段现在冻结。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WfKind {
    Decide,
    Around,
    Stream,
}

/// `wf/register` 申报里的一条 waterfall 监听。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WfDeclaration {
    pub name: String,
    pub kind: WfKind,
}

// ---------------------------------------------------------------------------
// 同名仲裁(§三 规则 3)
// ---------------------------------------------------------------------------

/// 已装载插件的记账:同 id 同 entry 重载幂等;同 id 异 entry 拒绝后者并
/// 指名已有者。宿主侧行为的 Rust 侧镜像,桥据此仲裁 `plugin/load`。
#[derive(Debug, Default)]
pub struct PluginLedger {
    entries: HashMap<String, String>,
}

impl PluginLedger {
    /// 申报一次装载。`(id, entry)` 与已有相同 → 幂等通过;`id` 已被别的
    /// entry 占用 → [`ProtoError::DuplicatePlugin`] 指名已有者。
    pub fn load(&mut self, plugin_id: &str, entry: &str) -> Result<(), ProtoError> {
        match self.entries.get(plugin_id) {
            Some(existing) if existing == entry => Ok(()),
            Some(existing) => Err(ProtoError::DuplicatePlugin {
                plugin_id: plugin_id.to_owned(),
                existing_entry: existing.clone(),
                attempted_entry: entry.to_owned(),
            }),
            None => {
                self.entries.insert(plugin_id.to_owned(), entry.to_owned());
                Ok(())
            }
        }
    }

    pub fn unload(&mut self, plugin_id: &str) -> bool {
        self.entries.remove(plugin_id).is_some()
    }

    pub fn entry(&self, plugin_id: &str) -> Option<&str> {
        self.entries.get(plugin_id).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
