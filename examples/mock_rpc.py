"""Offline synthetic RPC with deterministic fault controls for acceptance tests."""
import argparse
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


class Mock(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, port=0):
        self.fixtures = json.loads(Path(__file__).with_name('fixtures.json').read_text())['transactions']
        self.calls = []
        self.genesis_hash = '11111111111111111111111111111111'
        self.active = self.peak = 0
        self.guard = threading.Lock()
        self.faults = []
        self.method_faults = {}
        self.always_faults = {}
        self.signature_faults = {}
        self.fail_after = None
        self.failure_after = 503
        self.retry_after = '1'
        self.delay = 0
        self.hold_after = None
        self.release = threading.Event()
        super().__init__(('127.0.0.1', port), Handler)

    def select_fault(self, request, count):
        """Called under guard; each queued fault is consumed by one request."""
        method = request['method']
        if self.faults:
            return self.faults.pop(0)
        queued = self.method_faults.get(method)
        if queued:
            return queued.pop(0)
        if method == 'getTransaction':
            signature_fault = self.signature_faults.get(request['params'][0])
            if signature_fault is not None:
                return signature_fault
        if self.fail_after is not None and count > self.fail_after:
            return self.failure_after
        return self.always_faults.get(method)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        req = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        server = self.server
        with server.guard:
            server.active += 1
            server.peak = max(server.peak, server.active)
            server.calls.append((time.monotonic(), req))
            count = len(server.calls)
            fault = server.select_fault(req, count)
        try:
            if server.hold_after is not None and count > server.hold_after:
                server.release.wait(10)
            time.sleep(server.delay)
            if fault == 'disconnect':
                self.connection.shutdown(2)
                return
            if fault == 'timeout':
                time.sleep(.2)
            if isinstance(fault, int):
                self.send_response(fault)
                self.send_header('Retry-After', server.retry_after)
                self.end_headers()
                return
            if req['method'] == 'getGenesisHash':
                result = server.genesis_hash
            elif req['method'] == 'getSignatureStatuses':
                assert req['params'][1]['searchTransactionHistory'] is True
                assert len(req['params'][0]) <= 256
                result = {'context': {'slot': 123}, 'value': [server.fixtures[s]['status'] for s in req['params'][0]]}
                if fault == 'status_shape':
                    result['value'] = []
            else:
                assert req['method'] == 'getTransaction'
                assert req['params'][1] == {
                    'commitment': 'finalized', 'encoding': 'jsonParsed', 'maxSupportedTransactionVersion': 0,
                }
                result = server.fixtures[req['params'][0]]['transaction']
            body = json.dumps({'jsonrpc': '2.0', 'id': req['id'], 'result': result}).encode()
            if fault == 'malformed':
                body = b'{broken'
            if fault == 'large':
                body = b' ' * (2 * 1024 * 1024 + 1)
            if fault == 'permanent_rpc':
                body = json.dumps({
                    'jsonrpc': '2.0', 'id': 1,
                    'error': {'code': -32602, 'message': 'secret-from-provider'},
                }).encode()
            if fault == 'unsupported_version':
                body = json.dumps({
                    'jsonrpc': '2.0', 'id': 1,
                    'error': {'code': -32015, 'message': 'secret-from-provider'},
                }).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            with server.guard:
                server.active -= 1


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=8899)
    args = parser.parse_args()
    with Mock(args.port) as server:
        print(f'Synthetic offline mock listening on port {server.server_port}', flush=True)
        server.serve_forever()
