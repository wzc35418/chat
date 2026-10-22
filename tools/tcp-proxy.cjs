// Minimal TCP proxy for fault injection: pass | delay | blackhole.
//
//   node tools/tcp-proxy.cjs <listenPort> <targetHost> <targetPort> <mode> [delayMs]
//
// Modes:
//   pass      forward bytes both ways (baseline)
//   delay     forward after delayMs (adds latency, still works)
//   blackhole accept the connection and then send NOTHING (simulates a partition where
//             the TCP handshake succeeds but the peer is unreachable)
//
// Prints one line per accepted connection so tests can assert it actually accepted.
const net = require('node:net');

const [listenPort, targetHost, targetPortRaw, mode = 'pass', delayRaw = '0'] = process.argv.slice(2);
if (!listenPort || !targetHost || !targetPortRaw) {
  console.error('usage: tcp-proxy.cjs <listenPort> <targetHost> <targetPort> <pass|delay|blackhole> [delayMs]');
  process.exit(2);
}
const targetPort = Number(targetPortRaw);
const delayMs = Number(delayRaw) || 0;
let accepted = 0;

const server = net.createServer((client) => {
  accepted += 1;
  console.log('ACCEPT ' + accepted + ' mode=' + mode);
  client.on('error', () => client.destroy());
  if (mode === 'blackhole') {
    // Accept and hold: never read, never forward, never close. The client must time out.
    client.pause();
    return;
  }
  const upstream = net.connect({ host: targetHost, port: targetPort }, () => {
    const wire = () => {
      client.pipe(upstream);
      upstream.pipe(client);
    };
    if (mode === 'delay' && delayMs > 0) { setTimeout(wire, delayMs); } else { wire(); }
  });
  upstream.on('error', () => { client.destroy(); });
  client.on('close', () => upstream.destroy());
  upstream.on('close', () => client.destroy());
});

server.listen(Number(listenPort), '127.0.0.1', () => {
  console.log('LISTEN ' + listenPort + ' -> ' + targetHost + ':' + targetPort + ' mode=' + mode + (mode === 'delay' ? ' delay=' + delayMs + 'ms' : ''));
});

process.on('SIGTERM', () => { server.close(() => process.exit(0)); });
process.on('SIGINT', () => { server.close(() => process.exit(0)); });
