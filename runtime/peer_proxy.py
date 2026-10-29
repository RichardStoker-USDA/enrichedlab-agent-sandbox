#!/usr/bin/env python3
"""Source-local TCP relay through OpenShell's inspected HTTP proxy."""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import hmac
import ipaddress
import os
import ssl
from pathlib import Path
from urllib.parse import urlsplit

MAX_PROXY_HEADER = 16_384


def proxy_address() -> tuple[str, int]:
    raw = (
        os.environ.get("HTTPS_PROXY")
        or os.environ.get("HTTP_PROXY")
        or os.environ.get("https_proxy")
        or os.environ.get("http_proxy")
    )
    if not raw:
        raise RuntimeError("OpenShell proxy environment is missing")
    parsed = urlsplit(raw)
    if (
        parsed.scheme != "http"
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
    ):
        raise RuntimeError("OpenShell proxy URL must be uncredentialed HTTP")
    return parsed.hostname, parsed.port or 80


async def copy_stream(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
    try:
        while data := await reader.read(65_536):
            writer.write(data)
            await writer.drain()
    finally:
        if writer.can_write_eof():
            try:
                writer.write_eof()
                await writer.drain()
            except (ConnectionError, OSError):
                pass


async def open_tunnel(
    proxy_host: str,
    proxy_port: int,
    target_host: str,
    target_port: int,
    tls_ca: str | None = None,
    tls_token: str | None = None,
) -> tuple[asyncio.StreamReader, asyncio.StreamWriter]:
    proxy_reader, proxy_writer = await asyncio.wait_for(
        asyncio.open_connection(proxy_host, proxy_port, limit=MAX_PROXY_HEADER), timeout=10
    )
    parsed_target = ipaddress.ip_address(target_host) if target_host != "host.openshell.internal" else None
    authority_host = f"[{target_host}]" if parsed_target is not None and parsed_target.version == 6 else target_host
    authority = f"{authority_host}:{target_port}"
    request = (
        f"CONNECT {authority} HTTP/1.1\r\n"
        f"Host: {authority}\r\n"
        "Proxy-Connection: keep-alive\r\n\r\n"
    )
    proxy_writer.write(request.encode("ascii"))
    await proxy_writer.drain()
    response = await asyncio.wait_for(proxy_reader.readuntil(b"\r\n\r\n"), timeout=10)
    status = response.split(b"\r\n", 1)[0].split()
    if len(status) < 2 or status[1] != b"200":
        proxy_writer.close()
        await proxy_writer.wait_closed()
        raise RuntimeError("OpenShell proxy denied the peer target")
    if tls_ca is not None:
        token = Path(tls_token).read_text(encoding="ascii").strip()
        if len(token) != 64:
            raise RuntimeError("peer capability is invalid")
        try:
            token_bytes = bytes.fromhex(token)
        except ValueError as error:
            raise RuntimeError("peer capability is invalid") from error
        proxy_writer.write(b"PEER/1\n")
        await proxy_writer.drain()
        challenge_line = await asyncio.wait_for(proxy_reader.readline(), timeout=10)
        challenge_parts = challenge_line.strip().split()
        if (
            len(challenge_parts) != 2
            or challenge_parts[0] != b"CHALLENGE"
            or len(challenge_parts[1]) != 64
        ):
            raise RuntimeError("peer challenge is invalid")
        try:
            challenge = bytes.fromhex(challenge_parts[1].decode("ascii"))
        except (UnicodeDecodeError, ValueError) as error:
            raise RuntimeError("peer challenge is invalid") from error
        response = hmac.new(token_bytes, challenge, hashlib.sha256).hexdigest()
        proxy_writer.write(f"RESPONSE {response}\n".encode("ascii"))
        await proxy_writer.drain()
        start_tls = await asyncio.wait_for(proxy_reader.readline(), timeout=10)
        if start_tls != b"STARTTLS\n":
            raise RuntimeError("peer capability was rejected")
        context = ssl.create_default_context(cafile=tls_ca)
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        await asyncio.wait_for(
            proxy_writer.start_tls(context, server_hostname=target_host), timeout=10
        )
        acknowledgement = await asyncio.wait_for(proxy_reader.readline(), timeout=10)
        if acknowledgement != b"OK\n":
            proxy_writer.close()
            await proxy_writer.wait_closed()
            raise RuntimeError("peer capability was rejected")
    return proxy_reader, proxy_writer


async def handle_connection(
    client_reader: asyncio.StreamReader,
    client_writer: asyncio.StreamWriter,
    proxy_host: str,
    proxy_port: int,
    target_host: str,
    target_port: int,
    tls_ca: str | None,
    tls_token: str | None,
) -> None:
    proxy_writer: asyncio.StreamWriter | None = None
    try:
        proxy_reader, proxy_writer = await open_tunnel(
            proxy_host,
            proxy_port,
            target_host,
            target_port,
            tls_ca,
            tls_token,
        )
        outbound = asyncio.create_task(copy_stream(client_reader, proxy_writer))
        inbound = asyncio.create_task(copy_stream(proxy_reader, client_writer))
        done, pending = await asyncio.wait(
            {outbound, inbound}, return_when=asyncio.FIRST_COMPLETED
        )
        for task in pending:
            task.cancel()
        await asyncio.gather(*done, *pending, return_exceptions=True)
    except (asyncio.IncompleteReadError, ConnectionError, OSError, RuntimeError):
        pass
    finally:
        client_writer.close()
        if proxy_writer is not None:
            proxy_writer.close()
        await asyncio.gather(
            client_writer.wait_closed(),
            *([proxy_writer.wait_closed()] if proxy_writer is not None else []),
            return_exceptions=True,
        )


async def run(args: argparse.Namespace) -> None:
    proxy_host, proxy_port = proxy_address()
    if args.probe:
        _, writer = await open_tunnel(
            proxy_host,
            proxy_port,
            args.target_host,
            args.target_port,
            args.tls_ca,
            args.tls_token,
        )
        writer.close()
        await writer.wait_closed()
        return
    handler = lambda reader, writer: handle_connection(
        reader,
        writer,
        proxy_host,
        proxy_port,
        args.target_host,
        args.target_port,
        args.tls_ca,
        args.tls_token,
    )
    server = await asyncio.start_server(
        handler,
        host="127.0.0.1",
        port=args.listen_port,
        start_serving=True,
    )
    ready_file = Path(args.ready_file)
    ready_file.parent.mkdir(parents=True, exist_ok=True)
    ready_file.write_text("ready\n", encoding="utf-8")
    os.chmod(ready_file, 0o600)

    async def wait_for_stop() -> None:
        stop_file = Path(args.stop_file)
        while not stop_file.exists():
            await asyncio.sleep(0.1)

    async with server:
        serving = asyncio.create_task(server.serve_forever())
        await wait_for_stop()
        server.close()
        await server.wait_closed()
        serving.cancel()
        await asyncio.gather(serving, return_exceptions=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen-port", type=int, choices=range(1, 65_536))
    parser.add_argument("--target-host", required=True)
    parser.add_argument("--target-port", type=int, choices=range(1, 65_536), required=True)
    parser.add_argument("--ready-file")
    parser.add_argument("--stop-file")
    parser.add_argument("--tls-ca")
    parser.add_argument("--tls-token")
    parser.add_argument("--probe", action="store_true")
    args = parser.parse_args()
    if not args.probe and (
        args.listen_port is None or args.ready_file is None or args.stop_file is None
    ):
        parser.error(
            "--listen-port, --ready-file, and --stop-file are required unless --probe is set"
        )
    if (args.tls_ca is None) != (args.tls_token is None):
        parser.error("--tls-ca and --tls-token must be provided together")
    if args.target_host != "host.openshell.internal":
        try:
            ipaddress.ip_address(args.target_host)
        except ValueError:
            parser.error("--target-host must be host.openshell.internal or an IP literal")
    return args


if __name__ == "__main__":
    asyncio.run(run(parse_args()))
