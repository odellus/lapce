#!/usr/bin/env python3
"""Minimal ACP echo agent for integration tests — no asyncio, no SDK.

Reads JSON-RPC from stdin, writes responses to stdout, logs to stderr.
Echoes prompt text back as agent_message_chunk, then responds with end_turn.
"""
import json
import sys
import uuid


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        method = msg.get("method")
        msg_id = msg.get("id")

        if method == "initialize":
            resp = {"jsonrpc": "2.0", "id": msg_id, "result": {"protocolVersion": 1}}
            sys.stdout.write(json.dumps(resp) + "\n")
            sys.stdout.flush()

        elif method == "session/new":
            sid = uuid.uuid4().hex
            resp = {"jsonrpc": "2.0", "id": msg_id, "result": {"sessionId": sid}}
            sys.stdout.write(json.dumps(resp) + "\n")
            sys.stdout.flush()

        elif method == "session/prompt":
            params = msg.get("params", {})
            session_id = params.get("sessionId", "unknown")
            prompt_blocks = params.get("prompt", [])

            # Echo each text block as agent_message_chunk
            for block in prompt_blocks:
                text = block.get("text", "") if isinstance(block, dict) else ""
                if text:
                    notif = {
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": text},
                            },
                        },
                    }
                    sys.stdout.write(json.dumps(notif) + "\n")
                    sys.stdout.flush()

            # Respond with end_turn
            resp = {"jsonrpc": "2.0", "id": msg_id, "result": {"stopReason": "end_turn"}}
            sys.stdout.write(json.dumps(resp) + "\n")
            sys.stdout.flush()

        elif method == "session/cancel":
            # Just acknowledge — no response expected for notifications
            pass

        else:
            # Generic response for unknown methods
            if msg_id is not None:
                resp = {"jsonrpc": "2.0", "id": msg_id, "result": {}}
                sys.stdout.write(json.dumps(resp) + "\n")
                sys.stdout.flush()


if __name__ == "__main__":
    main()
