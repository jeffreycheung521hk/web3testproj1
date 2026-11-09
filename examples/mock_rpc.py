"""Offline synthetic RPC. Standard library only; never contacts a chain."""
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
        self.active = self.peak = 0
        self.guard = threading.Lock()
        self.faults = []
        self.delay = 0
        self.hold_after = None
        self.release = threading.Event()
        super().__init__(('127.0.0.1', port), Handler)


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
            fault = server.faults.pop(0) if server.faults else None
        try:
            if server.hold_after is not None and count > server.hold_after:
                server.release.wait(5)
            time.sleep(server.delay)
            if fault == 'disconnect':
                self.connection.shutdown(2)
                return
            if fault == 'timeout':
                time.sleep(.2)
            if isinstance(fault, int):
                self.send_response(fault)
                self.send_header('Retry-After', '1')
                self.end_headers()
                return
            if req['method'] == 'getSignatureStatuses':
                assert req['params'][1]['searchTransactionHistory'] is True
                assert len(req['params'][0]) <= 256
                result = {'context': {'slot': 123}, 'value': [server.fixtures[s]['status'] for s in req['params'][0]]}
            else:
                assert req['method'] == 'getTransaction'
                assert req['params'][1] == {'commitment':'finalized', 'encoding':'jsonParsed', 'maxSupportedTransactionVersion':0}
                result = server.fixtures[req['params'][0]]['transaction']
            body = json.dumps({'jsonrpc':'2.0', 'id':req['id'], 'result':result}).encode()
            if fault == 'malformed':
                body = b'{broken'
            if fault == 'large':
                body = b' ' * (2*1024*1024+1)
            if fault == 'permanent_rpc':
                body = json.dumps({'jsonrpc':'2.0','id':1,'error':{'code':-32602,'message':'secret-from-provider'}}).encode()
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
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--port', type=int, default=8899)
    args = p.parse_args()
    with Mock(args.port) as server:
        print(f'Synthetic offline mock listening on port {server.server_port}', flush=True)
        server.serve_forever()
