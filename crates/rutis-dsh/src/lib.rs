//! rutis-dsh:dsh 桥的 dsh 面(设计:docs/design-dsh-bridge-2026-08-21.md v3.2)。
//!
//! 层次纪律:本 crate 是 **dsh 关系的唯一所在**——dshSemver、dsh 服务集、
//! 会话字段(sessionId/turnId)语义、llm 缝(aimux)与事件类型映射/替身表。
//! 骑在 [`rutis_cordis`] 基座桥上;与 `rutis-agent` 互不依赖。
//!
//! - [`dsh 节`](#structs):hello 的 `dsh` 节解析、校验与装载期求差(M1)。
//! - [`llm`]:llm 缝——TS 侧 dsh adapter 的 stream 过线 → aimux,chunk 以
//!   ntf 流回传(M2-3)。

pub mod llm;

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;

use rutis_cordis::ExpectedHost;

pub use llm::LlmSeam;

/// dsh 部署在 hello 里附加的 `dsh` 节(§三 规则 1 v3.2:纯 cordis 宿主
/// 不带此节)。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DshNode {
    #[serde(rename = "dshSemver")]
    pub dsh_semver: String,
    #[serde(default)]
    pub services: BTreeSet<String>,
}

impl DshNode {
    /// 装载期能力集求差(§三 规则 1):`injects` 中 dsh 部署没有的服务。
    pub fn missing_services<'a>(&self, injects: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        injects
            .into_iter()
            .filter(|name| !self.services.contains(*name))
            .map(str::to_owned)
            .collect()
    }
}

/// 从宿主 hello 原始参数解析 `dsh` 节;缺失即非 dsh 部署。
pub fn parse_dsh_node(hello_params: &Value) -> Result<DshNode, String> {
    let node = hello_params
        .get("dsh")
        .ok_or_else(|| "hello has no dsh node (not a dsh deployment)".to_string())?;
    serde_json::from_value(node.clone())
        .map_err(|e| format!("hello dsh node malformed: {e}"))
}

/// dsh 面的握手期望。`dsh_semver` 为 `Some` 时精确比对(版本声明制的
/// 第一道闸:对齐 `dsh-v0.1.0-rc.7` 一类 tag)。
#[derive(Debug, Clone)]
pub struct ExpectedDsh {
    pub dsh_semver: Option<String>,
}

impl ExpectedDsh {
    pub fn strict(dsh_semver: impl Into<String>) -> ExpectedDsh {
        ExpectedDsh { dsh_semver: Some(dsh_semver.into()) }
    }

    pub fn any() -> ExpectedDsh {
        ExpectedDsh { dsh_semver: None }
    }

    /// 两级握手的组装点:产出基座桥的 [`ExpectedHost`],`verify` 钩子完成
    /// dsh 节校验——dsh 节缺失或 dshSemver 漂移都算**握手期失败**。
    pub fn base_expectation(&self) -> ExpectedHost {
        let pin = self.dsh_semver.clone();
        let mut expected = ExpectedHost::protocol(1);
        expected.verify = Some(Arc::new(move |params| {
            let node = parse_dsh_node(params)?;
            if let Some(pin) = &pin {
                if node.dsh_semver != *pin {
                    return Err(format!(
                        "dshSemver mismatch: host declared {}, bridge pins {pin}",
                        node.dsh_semver
                    ))
                }
            }
            Ok(())
        }));
        expected
    }
}
