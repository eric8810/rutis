// Minimal real-Node bridge peer for the M2-1 end-to-end test: connects over
// TCP, speaks the line-framed JSON protocol, answers hello + svc/call, logs
// notifications to stdout (which must never corrupt frames — the channel is
// dedicated). Zero npm dependencies: node >=22 native only.
import net from 'node:net'

const port = Number(process.env.BRIDGE_PORT)
const sock = net.connect(port, '127.0.0.1')

let buf = ''
sock.setEncoding('utf8')
sock.on('data', (chunk) => {
  buf += chunk
  let nl
  while ((nl = buf.indexOf('\n')) >= 0) {
    const line = buf.slice(0, nl)
    buf = buf.slice(nl + 1)
    if (line.length === 0) continue
    handle(JSON.parse(line))
  }
})

function send(frame) {
  sock.write(JSON.stringify(frame) + '\n')
}

function handle(frame) {
  if (frame.type === 'req' && frame.method === 'hello') {
    send({
      type: 'res', id: frame.id, ok: true,
      result: {
        protocol: 1, base: 'min-cordis', baseSemver: '0.1.0', dshSemver: '0.1.1-rc.2',
        stack: ['node'], caps: { services: ['echo'], wfKinds: [], scopes: [] },
      },
    })
  } else if (frame.type === 'req' && frame.method === 'svc/call') {
    // Deliberate console.log on the request path: stdout is for logs, the
    // frame channel is dedicated — this must not corrupt anything.
    console.log('[echo-host] svc/call', JSON.stringify(frame.params))
    send({ type: 'res', id: frame.id, ok: true, result: { echoed: frame.params } })
  } else if (frame.type === 'req') {
    send({ type: 'res', id: frame.id, ok: false, error: { code: 'unhandled', message: `no ${frame.method}` } })
  } else if (frame.type === 'ntf') {
    console.log('[echo-host] ntf', frame.method)
    if (frame.method === 'evt/emit') {
      // Reflect events back so the Rust side's notify hook sees them.
      send({ type: 'ntf', method: 'evt/emit', params: frame.params })
    }
  }
}

sock.on('error', () => process.exit(1))
process.on('SIGTERM', () => process.exit(0))
