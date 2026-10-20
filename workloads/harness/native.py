#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Run a Harmony fault-workload image natively in a container, without Harmony.

The script follows the image's /etc/harmony/bundle the way the platform
supervisor does: setup, supervised nodes that restart after they die, a ready
probe, one workload driver and a serial check loop. It injects random kills,
pauses and bundle hooks from a seeded generator and stops at the first failing
Always or Unreachable assertion the image writes to its SDK output. At the end
it runs the heal hooks and a final check.

The container has no /dev/harmony, so the script deletes the image's
libvoidstar.so inside the container. Without it the Antithesis Go SDK in
instrumented binaries falls back to its local handler instead of aborting.
"""

import argparse
import json
import math
import os
import random
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

NODE_EXIT = "workload node ends only by a fault the search injected"
MOUNT = "/run/native"
SDK_DIR = MOUNT + "/antithesis"
PID_DIR = MOUNT + "/pids"
VOIDSTAR = "/usr/lib/libvoidstar.so"
HOOK_PROBABILITY = 0.3


def tokenize(line):
    words, current, is_open, quoted = [], [], False, False
    chars = iter(line)
    for c in chars:
        if c == '"':
            quoted, is_open = not quoted, True
        elif c == "\\" and quoted:
            escaped = next(chars, None)
            if escaped is None:
                raise ValueError(f"bad escape in bundle line {line!r}")
            current.append(escaped if escaped in '"\\' else "\\" + escaped)
            is_open = True
        elif c.isspace() and not quoted:
            if is_open:
                words.append("".join(current))
                current, is_open = [], False
        else:
            current.append(c)
            is_open = True
    if quoted:
        raise ValueError(f"unterminated quote in bundle line {line!r}")
    if is_open:
        words.append("".join(current))
    return words


def parse_bundle(text):
    bundle = {"nodes": [], "hooks": [], "setup": None, "ready": None, "workload": None, "check": None}
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        words = tokenize(line)
        kind, rest = words[0], words[1:]
        if kind == "node":
            bundle["nodes"].append((rest[0], rest[1:]))
        elif kind in ("setup", "ready", "workload", "check"):
            bundle[kind] = rest
        elif kind == "hook":
            bundle["hooks"].append((int(rest[0]), rest[1:]))
        else:
            raise ValueError(f"unknown bundle line {line!r}")
    return bundle


def duration(rng):
    return math.exp(rng.uniform(math.log(0.01), math.log(10.0)))


class Run:
    def __init__(self, args):
        self.args = args
        self.tool = args.tool or ("podman" if shutil.which("podman") else "docker")
        self.rng = random.Random(args.seed)
        self.name = f"harmony-native-{os.getpid()}-{args.seed}"
        if args.out:
            Path(args.out).mkdir(parents=True, exist_ok=True)
        self.out = Path(tempfile.mkdtemp(prefix="harmony-native-", dir=args.out))
        self.done = threading.Event()
        self.lock = threading.Lock()
        self.result = None
        self.generation = 0
        self.counts = {"kill": 0, "stop": 0, "kill_all": 0, "hooks": 0, "natural_deaths": 0, "checks": 0,
                       "check_failures": 0}
        self.passed = set()
        self.last_check = None
        self.start = None
        self.probe = None
        self.hooks = []
        self.quiet = threading.Event()

    def seed(self):
        with self.lock:
            return str(self.rng.getrandbits(63))

    def disturb(self):
        with self.lock:
            self.generation += 1

    def docker(self, *argv, **kwargs):
        return subprocess.run([self.tool, *argv], **kwargs)

    def exec_argv(self, argv, env=None):
        command = [self.tool, "exec"]
        for key, value in {"ANTITHESIS_OUTPUT_DIR": SDK_DIR, "HARNESS_SEED": self.seed(), **(env or {})}.items():
            command += ["-e", f"{key}={value}"]
        return command + [self.name, *argv]

    def finish(self, result):
        with self.lock:
            if self.result is None:
                self.result = result
        self.done.set()

    def elapsed(self):
        return 0.0 if self.start is None else round(time.monotonic() - self.start, 3)

    def write_record(self, record):
        line = json.dumps(record, separators=(",", ":")) + "\n"
        with open(self.out / "antithesis" / "sdk.jsonl", "a") as sink:
            sink.write(line)

    def tail(self):
        path = self.out / "antithesis" / "sdk.jsonl"
        with open(path) as sink:
            pending = ""
            while not self.done.is_set():
                chunk = sink.readline()
                if not chunk:
                    time.sleep(0.05)
                    continue
                pending += chunk
                if not pending.endswith("\n"):
                    continue
                line, pending = pending, ""
                try:
                    record = json.loads(line).get("antithesis_assert")
                except json.JSONDecodeError:
                    continue
                if not record or not record.get("hit"):
                    continue
                kind, must_hit = record.get("assert_type"), record.get("must_hit", True)
                ident = record.get("id") or record.get("message")
                if kind == "always" and not record.get("condition"):
                    self.finish({"result": "violation", "elapsed_s": self.elapsed(), "assertion": ident,
                                 "details": record.get("details")})
                elif kind == "reachability" and not must_hit:
                    self.finish({"result": "violation", "elapsed_s": self.elapsed(), "assertion": ident,
                                 "details": record.get("details")})
                elif kind in ("sometimes", "reachability") and record.get("condition", True):
                    self.passed.add(ident)

    def node_loop(self, index, name, argv, state):
        pid_file = self.out / "pids" / f"{name}.pid"
        wrapper = ["setsid", "-w", "sh", "-c", f'echo $$ > {PID_DIR}/{name}.pid; exec "$@"', "sh", *argv]
        while not self.done.is_set():
            with state["lock"]:
                state["injected"] = False
                state["pid"] = None
            pid_file.unlink(missing_ok=True)
            process = subprocess.Popen(self.exec_argv(wrapper), stdout=subprocess.DEVNULL,
                                       stderr=subprocess.DEVNULL)
            deadline = time.monotonic() + 10
            while not pid_file.exists() and process.poll() is None and time.monotonic() < deadline:
                time.sleep(0.01)
            if pid_file.exists():
                with state["lock"]:
                    state["pid"] = int(pid_file.read_text().strip() or 0) or None
            status = process.wait()
            with state["lock"]:
                injected = state["injected"]
                state["pid"] = None
                restart_at = state["restart_at"]
            if self.done.is_set():
                return
            self.disturb()
            if not injected:
                with self.lock:
                    self.counts["natural_deaths"] += 1
                if status != 0 and self.start is not None:
                    self.write_record({"antithesis_assert": {
                        "assert_type": "always", "display_type": "AlwaysOrUnreachable", "id": NODE_EXIT,
                        "message": NODE_EXIT, "hit": True, "must_hit": False, "condition": False,
                        "location": {"file": "native.py", "function": "node exit", "class": "",
                                     "begin_line": 0, "begin_column": 0},
                        "details": {"node": index, "exit": f"status {status}"}}})
                restart_at = time.monotonic() + 0.1
            while time.monotonic() < restart_at and not self.done.is_set():
                time.sleep(0.01)
            self.disturb()

    def signal(self, state, sig):
        with state["lock"]:
            pid = state["pid"]
        if pid is None:
            return False
        try:
            self.docker("exec", self.name, "sh", "-c", f"kill -s {sig} -- -{pid}",
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
        except subprocess.TimeoutExpired:
            return False
        return True

    def kill_all(self, states, busy_until, resumes, hold):
        now = time.monotonic()
        pids = []
        for index, state in enumerate(states):
            with state["lock"]:
                if state["pid"] is None:
                    continue
                state["injected"] = True
                state["restart_at"] = now + hold
                pids.append(f"-{state['pid']}")
            busy_until[index] = now + hold + 0.5
            resumes.pop(index, None)
        if not pids:
            return
        try:
            self.docker("exec", self.name, "sh", "-c", f"kill -s KILL -- {' '.join(pids)}",
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
        except subprocess.TimeoutExpired:
            return
        with self.lock:
            self.counts["kill_all"] += 1

    def kill_all_once(self, states):
        if not self.done.wait(self.args.kill_all_after):
            self.kill_all(states, [0.0] * len(states), {}, self.args.kill_all_hold)

    def fault_loop(self, states):
        busy_until = [0.0] * len(states)
        resumes = {}
        next_fault = time.monotonic() + duration(self.rng)
        while not self.done.is_set() and not self.quiet.is_set():
            now = time.monotonic()
            for index, at in list(resumes.items()):
                if now >= at:
                    if self.signal(states[index], "CONT"):
                        self.disturb()
                    del resumes[index]
            if now >= next_fault:
                self.inject(states, busy_until, resumes, now)
                next_fault = now + duration(self.rng)
            wake = min([next_fault, *resumes.values()])
            self.quiet.wait(max(0.0, wake - time.monotonic()))
        for index in resumes:
            self.signal(states[index], "CONT")

    def run_hook(self, argv, fault=True):
        try:
            subprocess.run(self.exec_argv(argv), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
        except subprocess.TimeoutExpired:
            return
        if not fault:
            return
        self.disturb()
        with self.lock:
            self.counts["hooks"] += 1

    def inject(self, states, busy_until, resumes, now):
        if self.hooks and self.rng.random() < HOOK_PROBABILITY:
            self.run_hook(self.rng.choice(self.hooks)[1])
            return
        if self.rng.random() < self.args.kill_all_probability:
            if all(now >= until for until in busy_until):
                self.kill_all(states, busy_until, resumes, duration(self.rng))
            return
        index = self.rng.randrange(len(states))
        action = self.rng.choice(("kill", "stop"))
        hold = duration(self.rng)
        if now < busy_until[index] or states[index]["pid"] is None:
            return
        busy_until[index] = now + hold + 0.5
        if action == "kill":
            with states[index]["lock"]:
                states[index]["injected"] = True
                states[index]["restart_at"] = now + hold
            if self.signal(states[index], "KILL"):
                with self.lock:
                    self.counts["kill"] += 1
        elif self.signal(states[index], "STOP"):
            self.disturb()
            resumes[index] = now + hold
            with self.lock:
                self.counts["stop"] += 1

    def check_once(self, argv):
        with self.lock:
            generation = self.generation
        command = self.exec_argv(argv, env={"HARMONY_DISTURBANCE_GENERATION": str(generation)})
        try:
            finished = subprocess.run(command, capture_output=True, text=True, timeout=120)
        except subprocess.TimeoutExpired:
            with self.lock:
                self.counts["check_failures"] += 1
            return None
        with self.lock:
            self.counts["checks"] += 1
            if finished.returncode != 0:
                self.counts["check_failures"] += 1
                return None
            if finished.stdout.strip():
                self.last_check = finished.stdout.strip().splitlines()[-1]
                return self.last_check
        return None

    def final_check(self, argv):
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline and not self.done.is_set():
            line = self.check_once(argv)
            try:
                if line and json.loads(line).get("read") == "ok":
                    return
            except json.JSONDecodeError:
                pass
            time.sleep(1)

    def check_loop(self, argv):
        while not self.done.is_set() and not self.quiet.is_set():
            self.check_once(argv)

    def run(self):
        image = self.args.image
        text = self.docker("run", "--rm", "--entrypoint", "cat", image, "/etc/harmony/bundle",
                           capture_output=True, text=True, check=True).stdout
        bundle = parse_bundle(text)
        self.hooks = bundle["hooks"]
        (self.out / "antithesis").mkdir()
        (self.out / "pids").mkdir()
        (self.out / "antithesis" / "sdk.jsonl").touch()
        limits = ["--cpuset-cpus", self.args.cpuset_cpus] if self.args.cpuset_cpus else []
        if self.args.memory:
            limits += ["--memory", self.args.memory]
        self.docker("run", "-d", "--init", "--privileged", "--name", self.name, *limits,
                    "--tmpfs", "/tmp:rw,exec,mode=1777,size=8g",
                    "-v", f"{self.out}:{MOUNT}", "--entrypoint", "sleep", image, "infinity",
                    check=True, stdout=subprocess.DEVNULL)
        threads = []
        try:
            self.docker("exec", self.name, "rm", "-f", VOIDSTAR, check=True)
            if bundle["setup"]:
                subprocess.run(self.exec_argv(bundle["setup"]), check=True)
            threading.Thread(target=self.tail, daemon=True).start()
            states = []
            for index, (name, argv) in enumerate(bundle["nodes"]):
                state = {"lock": threading.Lock(), "pid": None, "injected": False, "restart_at": 0.0}
                states.append(state)
                thread = threading.Thread(target=self.node_loop, args=(index, name, argv, state), daemon=True)
                thread.start()
                threads.append(thread)
            if bundle["ready"]:
                deadline = time.monotonic() + self.args.ready_timeout
                while subprocess.run(self.exec_argv(bundle["ready"]), stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL).returncode != 0:
                    if time.monotonic() > deadline:
                        self.finish({"result": "error", "error": "ready probe never passed"})
                        return self.report()
                    time.sleep(0.5)
            self.start = time.monotonic()
            workload = None
            if bundle["workload"]:
                with open(self.out / "workload.log", "w") as log:
                    workload = subprocess.Popen(self.exec_argv(bundle["workload"]), stdout=subprocess.DEVNULL,
                                                stderr=log)
            background = []
            if bundle["check"]:
                background.append(threading.Thread(target=self.check_loop, args=(bundle["check"],), daemon=True))
            if not self.args.no_faults:
                background.append(threading.Thread(target=self.fault_loop, args=(states,), daemon=True))
            if self.args.kill_all_after is not None:
                threading.Thread(target=self.kill_all_once, args=(states,), daemon=True).start()
            for thread in background:
                thread.start()
            if not self.done.wait(self.args.duration):
                self.quiet.set()
                for thread in background:
                    thread.join(timeout=150)
                for _, argv in self.hooks:
                    if argv[-1:] == ["heal"]:
                        self.run_hook(argv, fault=False)
                if bundle["check"]:
                    self.final_check(bundle["check"])
                    time.sleep(0.5)
                self.finish({"result": "clean", "elapsed_s": self.elapsed()})
            if workload is not None and workload.poll() is not None:
                self.result["workload_exit"] = workload.returncode
            if self.args.final_probe:
                self.probe = self.docker("exec", self.name, *tokenize(self.args.final_probe),
                                         capture_output=True, text=True).stdout.strip()
            return self.report()
        finally:
            self.done.set()
            self.collect()
            if not self.args.keep:
                self.docker("kill", self.name, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                self.docker("rm", "-f", self.name, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            else:
                print(f"kept container {self.name}", file=sys.stderr)

    def collect(self):
        self.docker("exec", self.name, "sh", "-c", f"mkdir -p {MOUNT}/tmp && cp -a /tmp/. {MOUNT}/tmp/",
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def report(self):
        result = dict(self.result)
        result.update({
            "image": self.args.image,
            "seed": self.args.seed,
            "faults": not self.args.no_faults,
            "counts": self.counts,
            "generation": self.generation,
            "sometimes_passed": sorted(self.passed),
            "output": str(self.out),
        })
        if self.args.kill_all_after is not None:
            result["kill_all_after"] = self.args.kill_all_after
            result["kill_all_hold"] = self.args.kill_all_hold
        if self.args.kill_all_probability:
            result["kill_all_probability"] = self.args.kill_all_probability
        if self.probe is not None:
            try:
                result["probe"] = json.loads(self.probe.splitlines()[-1])
            except (json.JSONDecodeError, IndexError):
                result["probe"] = self.probe
        if self.last_check:
            try:
                last = json.loads(self.last_check)
                result["last_check"] = {key: last.get(key) for key in ("read", "ops", "decided")}
            except json.JSONDecodeError:
                pass
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("image")
    parser.add_argument("--for", dest="duration", type=float, default=600.0, metavar="SECONDS")
    parser.add_argument("--seed", type=int, default=None)
    parser.add_argument("--no-faults", action="store_true")
    parser.add_argument("--tool", choices=("podman", "docker"))
    parser.add_argument("--out", default=None, help="directory for run output (default: system temp)")
    parser.add_argument("--ready-timeout", type=float, default=180.0)
    parser.add_argument("--keep", action="store_true", help="keep the container after the run")
    parser.add_argument("--kill-all-after", type=float, default=None, metavar="SECONDS",
                        help="SIGKILL every node at this time after ready and restart them after --kill-all-hold")
    parser.add_argument("--kill-all-hold", type=float, default=1.0, metavar="SECONDS")
    parser.add_argument("--kill-all-probability", type=float, default=0.0, metavar="P",
                        help="chance that a random fault kills every node instead of one")
    parser.add_argument("--cpuset-cpus", default=None, help="CPUs for the container, as in docker run")
    parser.add_argument("--memory", default=None, help="memory limit for the container, as in docker run")
    parser.add_argument("--final-probe", default=None, metavar="COMMAND",
                        help="command run in the container after the run; its last stdout line goes in the result")
    args = parser.parse_args()
    if args.seed is None:
        args.seed = random.SystemRandom().getrandbits(32)
    result = Run(args).run()
    print(json.dumps(result, sort_keys=True))
    return 1 if result.get("result") == "violation" else (2 if result.get("result") == "error" else 0)


if __name__ == "__main__":
    sys.exit(main())
