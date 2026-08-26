/**
 * §十.4 崩溃恢复验收(锁 f82fbcf 的修复):
 * bridge 通道断连必须 NEVER 崩进程 — 桥 socket 的 error/close 是 handled
 * (markDisconnected),不是 unhandled error。断连后 dead()=true,onDead 触发。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import net from 'node:net'
import { connectBridge } from '../src/bridge.js'

/** 假 bridge TCP 服务:hello 应答后立刻 destroy(模拟崩溃/断连)。 */
function fakeDshThenDrop(port: number): Promise<void> {
  return new Promise((resolve, reject) => {
    const server = net.createServer(sock => {
      sock.setEncoding('utf8')
      let buf = ''
      sock.on('data', (c: string) => {
        buf += c
        let nl: number
        while ((nl = buf.indexOf('\n')) >= 0) {
          const line = buf.slice(0, nl); buf = buf.slice(nl + 1)
          if (!line.trim()) continue
          const frame = JSON.parse(line) as { type: string; id: number }
          if (frame.type === 'req' && frame.id === 1) {
            sock.write(JSON.stringify({ type: 'res', id: 1, result: { ok: true } }) + '\n')
            sock.destroy()
            server.close()
            resolve()
          }
        }
      })
      sock.on('error', () => {})
    })
    server.listen(port, '127.0.0.1', () => {})
    server.on('error', reject)
  })
}

test('bridge channel loss never crashes the process; dead() fires', async () => {
  const port = 61999
  const dropPromise = fakeDshThenDrop(port)
  const conn = connectBridge(port)

  await conn.helloDone
  let deadFired = 0
  conn.onDead(() => { deadFired++ })

  await dropPromise
  await new Promise(r => setTimeout(r, 100))

  assert.equal(conn.dead(), true, '通道断连后 dead() 应为 true')
  assert.equal(deadFired, 1, '断连应触发 onDead 恰好一次')
  assert.ok(true, '进程存活:bridge channel loss 未崩溃宿主')
})
