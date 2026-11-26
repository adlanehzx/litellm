import argparse

from mcp.server.mcpserver import MCPServer

server = MCPServer("Math")


@server.tool()
def add(a: int, b: int) -> int:
    """Add two numbers."""
    return a + b


@server.tool()
def multiply(a: int, b: int) -> int:
    """Multiply two numbers."""
    return a * b


parser = argparse.ArgumentParser()
parser.add_argument("--transport")
parser.add_argument("--host")
parser.add_argument("--port", type=int)
args = parser.parse_args()
server.run("streamable-http", host=args.host, port=args.port, stateless_http=True, json_response=True)
