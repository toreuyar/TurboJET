import json
from http.server import BaseHTTPRequestHandler, HTTPServer

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass

    def do_POST(self):
        ln = int(self.headers.get('Content-Length', 0))
        body = json.loads(self.rfile.read(ln) or b'{}')
        model = body.get('model', 'mock')
        # Interactive stages must end with the completion marker to finish in one turn.
        has_output_file = any('output.md' in (m.get('content') or '') or '.turbojet' in (m.get('content') or '') for m in body.get('messages', []))
        marker = "::TURBOJET:DONE::" if has_output_file or True else ""
        text = "Intake complete: the task is to add a greeting endpoint. Requirements captured. " + marker
        if body.get('stream'):
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            chunk = {"id": "mock1", "object": "chat.completion.chunk", "created": 0, "model": model,
                     "choices": [{"index": 0, "delta": {"role": "assistant", "content": text}, "finish_reason": None}]}
            self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
            self.wfile.write(b"data: [DONE]\n\n")
        else:
            resp = {"id": "mock1", "object": "chat.completion", "created": 0, "model": model,
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}
            out = json.dumps(resp).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(out)))
            self.end_headers()
            self.wfile.write(out)

HTTPServer(('127.0.0.1', 9101), H).serve_forever()