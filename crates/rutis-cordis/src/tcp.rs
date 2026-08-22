//! TCP 传输(设计 §二"专用通道传帧"的 M2 实现)。
//!
//! 行帧 JSON over loopback TCP:每帧一行 `serde_json` + `\n`(JSON 字符串
//! 内的换行必然被转义,分隔安全)。stdout/stderr 全部留给宿主与插件日志
//! ——通道专用,不与 `console.log` 竞争,这是评审 §C.1 裁决的本质。
//! POSIX fd 3 与 unix socket 的原生实现在 Linux lane 补充;传输语义
//! (行帧、断连即宿主死亡)与此处一致。
//!
//! 读半由泵独占(`Wire::recv` 的约定),写半加锁供并发 `request` 串行化。
//! 单行设 32MiB 上限:JSON 帧不可能合法超过它,超限判线格式损坏而非
//! 无界缓冲。内部状态在 `Arc` 里,`Wire` 的 `'static` future 经 clone
//! 持有(与 `MemoryWire` 同一手法)。

use std::sync::Arc;

use rutis::BoxFuture;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::rpc::{Frame, ProtoError, Wire};

const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;

struct TcpInner {
    write: Mutex<OwnedWriteHalf>,
    read: Mutex<BufReader<OwnedReadHalf>>,
}

/// 一条已建立的 TCP 连接即一个 `Wire`。
pub struct TcpWire {
    inner: Arc<TcpInner>,
}

impl TcpWire {
    pub async fn connect(addr: &str) -> Result<TcpWire, ProtoError> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|e| ProtoError::Wire(format!("connect {addr}: {e}")))?;
        Ok(TcpWire::from_stream(stream))
    }

    pub fn from_stream(stream: TcpStream) -> TcpWire {
        let (read, write) = stream.into_split();
        TcpWire {
            inner: Arc::new(TcpInner {
                write: Mutex::new(write),
                read: Mutex::new(BufReader::new(read)),
            }),
        }
    }
}

impl TcpInner {
    async fn send_frame(&self, frame: &Frame) -> Result<(), ProtoError> {
        let line = serde_json::to_string(frame)
            .map_err(|e| ProtoError::Wire(format!("frame encode: {e}")))?;
        let mut write = self.write.lock().await;
        write
            .write_all(line.as_bytes())
            .await
            .map_err(|e| ProtoError::Wire(format!("tcp write: {e}")))?;
        write.write_all(b"\n").await.map_err(|e| ProtoError::Wire(format!("tcp write: {e}")))?;
        write.flush().await.map_err(|e| ProtoError::Wire(format!("tcp flush: {e}")))
    }

    /// 手动按缓冲切行:`read_line` 无界,这里以 32MiB 守卫代替。
    async fn recv_frame(&self) -> Option<Frame> {
        use tokio::io::AsyncBufReadExt;
        let mut read = self.read.lock().await;
        let mut line = Vec::new();
        loop {
            // fill_buf 的借用先在块内结清(consume 需要 mut),再消费。
            let (newline_at, filled) = {
                let buf = match read.fill_buf().await {
                    Ok(buf) => buf,
                    Err(_) => return None,
                };
                if buf.is_empty() {
                    // EOF:连接关闭 = 宿主死亡。
                    return None
                }
                let found = buf.iter().position(|&b| b == b'\n');
                match found {
                    Some(i) => {
                        line.extend_from_slice(&buf[..i]);
                        (Some(i + 1), buf.len())
                    }
                    None => {
                        line.extend_from_slice(buf);
                        (None, buf.len())
                    }
                }
            };
            match newline_at {
                Some(end) => {
                    read.consume(end);
                    let text = String::from_utf8(line).ok()?;
                    return serde_json::from_str(&text).ok()
                }
                None => {
                    read.consume(filled);
                    if line.len() > MAX_LINE_BYTES {
                        return None
                    }
                }
            }
        }
    }
}

impl Wire for TcpWire {
    fn send(&self, frame: Frame) -> BoxFuture<'static, Result<(), ProtoError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move { inner.send_frame(&frame).await })
    }

    fn recv(&self) -> BoxFuture<'static, Option<Frame>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move { inner.recv_frame().await })
    }
}
