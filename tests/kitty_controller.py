"""Private test controller, granted a dedicated Kitty socketpair at launch.

The production socket admits only set-colors. This background helper keeps its
separate inherited capability and never passes it to the Theme process.
"""
import base64
import json
import os
from pathlib import Path
import socket
import subprocess
import sys

endpoint, kitten = sys.argv[1:]
address = os.environ["KITTY_LISTEN_ON"]
assert address.startswith("fd:"), "controller must receive a dedicated socketpair"
descriptor = int(address[3:])
with socket.socket(socket.AF_UNIX) as listener:
    listener.bind(endpoint)
    os.chmod(endpoint, 0o600)
    listener.listen(1)
    listener.settimeout(120)
    try:
        while True:
            with listener.accept()[0] as connection:
                connection.settimeout(30)
                with connection.makefile("rwb") as stream:
                    request = json.loads(stream.readline(1 << 20))
                    if request["args"] == ["stop-controller"]:
                        break
                    data = request.get("stdin")
                    result = subprocess.run([kitten, "@", "--to", address, *request["args"]],
                        input=base64.b64decode(data) if data is not None else None,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                        pass_fds=(descriptor,), timeout=30)
                    response = {"returncode": result.returncode,
                        "stdout": base64.b64encode(result.stdout).decode(),
                        "stderr": base64.b64encode(result.stderr).decode()}
                    stream.write(json.dumps(response).encode() + b"\n")
                    stream.flush()
    finally:
        Path(endpoint).unlink(missing_ok=True)
