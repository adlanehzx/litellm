import asyncio
import json
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from tempfile import TemporaryDirectory

import httpx2
import yaml
from mcp import ClientSession
from mcp.client.streamable_http import streamable_http_client
from mcp.shared.exceptions import MCPError

from tests._master_key import MASTER_KEY
from tests.integration.mcp.test_proxy_mcp_e2e import _math_http_server, _start_proxy_server
from tests.integration.mcp import test_proxy_mcp_e2e as rig_helpers

rig_helpers.MCP_SERVER_SCRIPT = Path(__file__).with_name("math_peer.py")


class EmbeddingEndpoint(BaseHTTPRequestHandler):
    calls = []

    def do_POST(self):
        assert self.path == "/v1/embeddings", self.path
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        inputs = request["input"]
        self.calls.append(len(inputs))
        if len(inputs) > 1:
            time.sleep(32)
        result = json.dumps({
            "object": "list",
            "model": request["model"],
            "data": [
                {"object": "embedding", "index": index, "embedding": [1.0, 0.0]}
                for index, _ in enumerate(inputs)
            ],
            "usage": {"prompt_tokens": len(inputs), "total_tokens": len(inputs)},
        }).encode()
        try:
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(result)))
            self.end_headers()
            self.wfile.write(result)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *_args):
        pass


async def search(url, mode):
    statuses = []
    async def record(response):
        statuses.append(response.status_code)
    async with httpx2.AsyncClient(
        headers={"Authorization": f"Bearer {MASTER_KEY}"},
        timeout=httpx2.Timeout(10, read=75),
        event_hooks={"response": [record]},
    ) as http:
        async with streamable_http_client(url + "/mcp/proxy", http_client=http) as (read, write):
            async with ClientSession(read, write) as session:
                await session.initialize()
                tools = await session.list_tools()
                assert "search_tools" in {tool.name for tool in tools.tools}
                for state in (("cold",) if mode == "before" else ("cold", "warm")):
                    statuses.clear()
                    started = time.monotonic()
                    try:
                        result = await session.call_tool("search_tools", {"query": "add"})
                    except MCPError as exc:
                        print(f"{state}: HTTP {statuses}, MCP {exc.error.code}, {time.monotonic() - started:.2f}s", flush=True)
                        assert mode == "before" and 504 in statuses, statuses
                    else:
                        assert mode == "after" and not result.is_error, result
                        hits = json.loads(result.content[0].text)
                        assert any(hit["name"] == "math-add" for hit in hits), hits
                        assert 200 in statuses, statuses
                        print(f"{state}: HTTP {statuses}, {len(hits)} ranked tools, {time.monotonic() - started:.2f}s", flush=True)


def main():
    mode = sys.argv[1]
    os.environ["LITELLM_MASTER_KEY"] = MASTER_KEY
    with TemporaryDirectory() as directory, _math_http_server(0) as math_url, ThreadingHTTPServer(
        ("127.0.0.1", 0), EmbeddingEndpoint
    ) as embeddings:
        thread = threading.Thread(target=embeddings.serve_forever, daemon=True)
        thread.start()
        config = Path(directory) / "config.yaml"
        config.write_text(yaml.safe_dump({
            "general_settings": {"master_key": "os.environ/LITELLM_MASTER_KEY"},
            "litellm_settings": {"mcp_tool_search": {"embedding_model": "local-embeddings"}},
            "model_list": [{
                "model_name": "local-embeddings",
                "litellm_params": {
                    "model": "openai/text-embedding-3-small",
                    "api_base": f"http://127.0.0.1:{embeddings.server_port}/v1",
                    "api_key": "embeddings-test",
                    "timeout": 70,
                },
            }],
            "mcp_servers": {"math": {"transport": "http", "url": math_url + "/mcp", "allow_all_keys": True}},
        }))
        rig, proxy, proxy_thread, sock = _start_proxy_server(str(config))
        try:
            asyncio.run(search(rig.url, mode))
            print(f"embedding request sizes: {EmbeddingEndpoint.calls}", flush=True)
            assert len(EmbeddingEndpoint.calls) == (1 if mode == "before" else 2)
            assert EmbeddingEndpoint.calls[0] > 1
            if mode == "after":
                assert EmbeddingEndpoint.calls[1] == 1
        finally:
            proxy.should_exit = True
            proxy_thread.join(timeout=12)
            sock.close()
            embeddings.shutdown()
            thread.join(timeout=5)


if __name__ == "__main__":
    main()
