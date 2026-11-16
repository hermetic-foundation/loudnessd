#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Disposable PipeWire qualification. Never connects to the desktop server."""

import json
import math
import os
from pathlib import Path
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time


def main():
    binary = str(Path(sys.argv[1]).resolve())
    processes = []
    with tempfile.TemporaryDirectory(prefix="loudnessd-volume-") as directory:
        root = Path(directory)
        env = dict(os.environ, PIPEWIRE_RUNTIME_DIR=directory, XDG_RUNTIME_DIR=directory,
                   PIPEWIRE_REMOTE="pipewire-0", XDG_CONFIG_HOME=str(root / "config"),
                   XDG_STATE_HOME=str(root / "state"), XDG_CACHE_HOME=str(root / "cache"))
        config = root / "loudnessd.toml"
        config.write_text('[defaults]\nplayback = true\ncapture = false\n[playback]\ntarget_lufs = -30.0\n')

        def start(arguments, **kwargs):
            log = open(root / f"process-{len(processes)}.log", "wb")
            process = subprocess.Popen(arguments, env=env, stdout=log, stderr=log, **kwargs)
            processes.append(process)
            return process

        def run(*arguments):
            return subprocess.check_output(arguments, env=env, text=True, timeout=5)

        def wait_for(check, timeout=8):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                value = check()
                if value:
                    return value
                time.sleep(0.05)
            raise AssertionError("condition timed out")

        def status():
            return json.loads(run(binary, "msg", "status-json"))

        def command(*args):
            result = run(binary, "msg", *map(str, args))
            assert result.strip() == "ok", result

        def node_props(node):
            return next(item["info"]["params"]["Props"][0] for item in json.loads(run("pw-dump"))
                        if item["id"] == node)

        def near(value, expected, epsilon=1e-5):
            assert abs(value - expected) < epsilon, (value, expected)

        try:
            start(["pipewire"])
            wait_for(lambda: (root / "pipewire-0").is_socket())
            start(["wireplumber", "--profile", "policy"])
            time.sleep(1)
            run("pw-cli", "create-node", "adapter", "{ factory.name = support.null-audio-sink node.name = volume-test-sink media.class = Audio/Sink object.linger = true node.always-process = true audio.position = [ FL FR ] }")
            samples = b"".join(struct.pack("<ff", *(2 * [0.04 * math.sin(2 * math.pi * 997 * i / 48000)])) for i in range(4800))
            player = start(["pw-cat", "--playback", "--raw", "--format", "f32", "--rate", "48000", "--channels", "2", "--target", "volume-test-sink", "--properties", "application.id=loudnessd.volume-test", "-"], stdin=subprocess.PIPE)

            def feed():
                try:
                    while player.poll() is None:
                        player.stdin.write(samples)
                        player.stdin.flush()
                except (BrokenPipeError, ValueError):
                    pass
                finally:
                    try:
                        player.stdin.close()
                    except (BrokenPipeError, ValueError):
                        pass

            threading.Thread(target=feed, daemon=True).start()
            daemon = start([binary, "--daemon", "--config", str(config)])
            wait_for(lambda: (root / "loudnessd.sock").is_socket())
            stream = wait_for(lambda: next((s for s in status()["streams"] if s["route"] == "healthy"), None))
            node = stream["node_id"]
            assert stream["post_volume_supported"] is True
            assert stream["user_volume"] is None and stream["user_muted"] is None
            run("pw-cli", "set-param", str(node), "Props", "{ channelVolumes = [ 0.125 0.125 ] mute = true softVolumes = [ 8.0 8.0 ] softMute = false }")
            time.sleep(2)
            contaminated = status()["streams"][0]
            assert contaminated["gain_db"] < -10, contaminated
            command("volume", node, 0.5)
            props = node_props(node)
            print("unity Props:", {key: props[key] for key in ["volume", "mute", "channelVolumes", "softMute", "softVolumes"]}, flush=True)
            assert props["mute"] is False
            assert props["softMute"] is False
            assert props["channelVolumes"] == [1.0, 1.0]
            assert props["softVolumes"] == [1.0, 1.0]
            stream = status()["streams"][0]
            near(stream["user_volume"], 0.5)
            assert stream["user_muted"] is True
            command("mute", node, "off")
            time.sleep(4)
            before = status()["streams"][0]
            assert before["gain_db"] > contaminated["gain_db"] + 10, (contaminated, before)
            command("volume", node, 1.0)
            time.sleep(4)
            after = status()["streams"][0]
            near(after["source_lufs"], before["source_lufs"], 0.1)
            assert after["output_lufs"] - before["output_lufs"] > 15, (before, after)
            assert abs(after["gain_db"] - before["gain_db"]) < 1.0, (before, after)
            command("volume", node, 0.6)
            config.write_text('[defaults]\nplayback = true\ncapture = false\n[playback]\ntarget_lufs = -29.0\n')
            command("reload")
            wait_for(lambda: any(s["route"] == "healthy" and s["user_volume"] is not None for s in status()["streams"]))
            near(status()["streams"][0]["user_volume"], 0.6)
            command("mute", node, "on")
            command("disable")
            props = node_props(node)
            near(props["channelVolumes"][0], 0.6 ** 3)
            near(props["softVolumes"][0], 0.6 ** 3)
            assert props["mute"] is True and props["softMute"] is True
            assert status()["managed"] == 0
            command("enable")
            wait_for(lambda: any(s["route"] == "healthy" for s in status()["streams"]))
            command("volume", node, 0.7)
            command("mute", node, "off")
            daemon.kill()
            daemon.wait(timeout=5)
            daemon = start([binary, "--daemon", "--config", str(config)])
            time.sleep(3)
            assert daemon.poll() is None
            props = node_props(node)
            near(props["channelVolumes"][0], 0.7 ** 3)
            assert props["mute"] is False
            wait_for(lambda: any(s["route"] == "healthy" for s in status()["streams"]))
            command("mute", node, "on")
            near(status()["streams"][0]["user_volume"], 0.7)
            command("volume", node, 0.8)
            command("mute", node, "on")
            run("pw-cli", "send-command", str(node), "Pause", "{}")
            time.sleep(0.2)
            daemon.send_signal(signal.SIGTERM)
            assert daemon.wait(timeout=5) == 0
            run("pw-cli", "send-command", str(node), "Start", "{}")
            props = node_props(node)
            near(props["channelVolumes"][0], 0.8 ** 3)
            assert props["mute"] is True
            print("PASS: unity handoff, logical mute preservation, history reset, independent source/gain/output, reload, bypass, crash restart, mute-first migration, paused shutdown", flush=True)
        except BaseException:
            for log in root.glob("*.log"):
                print(f"--- {log.name} ---\n{log.read_text(errors='replace')[-8000:]}", file=sys.stderr)
            raise
        finally:
            for process in reversed(processes):
                if process.poll() is None:
                    process.send_signal(signal.SIGCONT)
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)


if __name__ == "__main__":
    main()
