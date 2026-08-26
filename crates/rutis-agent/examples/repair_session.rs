//! 一次性修复工具:规整损坏/超大的 session 文件。
//!
//! 做的事(与每次 LLM 请求前的在线规整同一套逻辑):
//!   1) 丢弃孤儿 tool_result(前面无配对 tool_call);
//!   2) 剥离 assistant 里未紧邻 tool_result 的悬空 tool_call;
//!   3) 裁掉尾部连续无 assistant 回复的 user(自主续跑失败死循环的残留)。
//!
//! 用法:
//!   cargo run -p rutis-agent --example repair_session [路径]
//!   # 默认路径: .rutis/session.json
//!
//! 运行前会备份原文件为 `<路径>.bak`,写回失败不影响原文件(备份仍在)。
//! 修复后历史结构合法;若仍偏大,agent 首次请求超限时会自动 compact 收敛。
fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".rutis/session.json".to_string());
    let p = std::path::Path::new(&path);

    if !p.exists() {
        eprintln!("session 文件不存在: {path}");
        std::process::exit(1);
    }

    // 读一次算 before(只读,不改)
    let probe = rutis_agent::Session::restore(p);
    let before = probe.messages().len();

    // 备份
    let bak = p.with_extension("json.bak");
    match std::fs::copy(p, &bak) {
        Ok(_) => println!("已备份 → {}", bak.display()),
        Err(e) => {
            eprintln!("备份失败,中止:{e}");
            std::process::exit(1);
        }
    }

    let mut s = probe;
    let (b, a) = s.sanitize();
    match s.persist(p) {
        Ok(()) => {
            println!("session 规整完成: {b} → {a} 条消息(before={before})");
            if a == 0 {
                println!("提示:规整后历史为空(原文件全是悬挂 user / 孤儿),已从干净状态开始。");
            }
        }
        Err(e) => {
            eprintln!("写回失败,原文件已被备份到 {}:{e}", bak.display());
            std::process::exit(1);
        }
    }
}
