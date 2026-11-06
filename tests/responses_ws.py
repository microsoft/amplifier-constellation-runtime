"""Transport shim for existing Responses content fixtures (stdlib only).

Keeps their SSE content generators for Azure/vLLM and exposes the same content
through a real WebSocket connection for native OpenAI tests. Steering lifecycle
has separate event-driven fixtures in steering.py; this shim does not emulate it.
"""
import base64, hashlib, http.client, json, struct

class ResponsesWebSocketMixin:
    def _ws_send(self, value):
        data=json.dumps(value).encode()
        header=bytes([0x81, len(data)]) if len(data)<126 else (b'\x81\x7e'+struct.pack('!H',len(data)) if len(data)<65536 else b'\x81\x7f'+struct.pack('!Q',len(data)))
        self.wfile.write(header+data);self.wfile.flush()
    def _ws_read(self):
        h=self.rfile.read(2)
        if len(h)!=2 or h[0]&15==8:return None
        n=h[1]&127
        if n==126:n=struct.unpack('!H',self.rfile.read(2))[0]
        elif n==127:n=struct.unpack('!Q',self.rfile.read(8))[0]
        if n>24*1024*1024:raise ValueError('fixture frame limit')
        mask=self.rfile.read(4) if h[1]&128 else b'\0'*4
        raw=self.rfile.read(n)
        payload=bytes(v^mask[i%4] for i,v in enumerate(raw))
        if h[0]&15==9:
            if not getattr(self.server,'ignore_pings',False):
                self.wfile.write(bytes([0x8a,len(payload)])+payload);self.wfile.flush()
            return self._ws_read()
        if h[0]&15==10:return self._ws_read()
        return json.loads(payload)
    def do_GET(self):
        if self.headers.get('Upgrade','').lower()!='websocket':return super().do_GET()
        self.protocol_version='HTTP/1.1'
        accept=base64.b64encode(hashlib.sha1((self.headers['Sec-WebSocket-Key']+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
        self.send_response(101);self.send_header('Upgrade','websocket');self.send_header('Connection','Upgrade');self.send_header('Sec-WebSocket-Accept',accept);self.end_headers()
        self.close_connection=True
        saved={};number=0
        try:
            while (body:=self._ws_read()) is not None:
                assert body.pop('type')=='response.create','Use the dedicated steering fixture'
                previous=body.pop('previous_response_id',None)
                if previous:body['input']=saved[previous]+body['input']
                body['stream']=True
                number+=1;rid=f'fixture-response-{number}'
                self._ws_send({'type':'response.created','response':{'id':rid,'previous_response_id':previous}})
                headers={'Content-Type':'application/json'}
                for key in ['Authorization','api-key','x-api-key','x-goog-api-key','anthropic-version','anthropic-beta']:
                    if self.headers.get(key):headers[key]=self.headers[key]
                conn=http.client.HTTPConnection('127.0.0.1',self.server.server_port,timeout=15)
                conn.request('POST',self.path,json.dumps(body),headers)
                response=conn.getresponse();raw=response.read().decode();status=response.status;conn.close()
                if status!=200:
                    self._ws_send({'type':'error','error':{'code':'fixture_error'}});return
                terminal=False
                for line in raw.splitlines():
                    if not line.startswith('data: '):continue
                    event=json.loads(line[6:])
                    if event.get('type') in ['response.completed','response.incomplete']:
                        event['response']['id']=rid;saved[rid]=body['input']+event['response']['output'];terminal=True
                    self._ws_send(event)
                if not terminal:return
        except (BrokenPipeError,ConnectionResetError,ConnectionAbortedError):pass
