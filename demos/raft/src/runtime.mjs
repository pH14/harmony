// SPDX-License-Identifier: AGPL-3.0-or-later
import { ConsoleFilter } from "./console-filter.mjs";
const script = (src) =>
  new Promise((resolve, reject) => {
    const s = document.createElement("script");
    s.src = src;
    s.onload = resolve;
    s.onerror = () => reject(new Error(`Could not load ${src}`));
    document.head.append(s);
  });
export async function boot(container, status, onReport = () => {}) {
  if (!crossOriginIsolated) {
    if (!("serviceWorker" in navigator))
      throw new Error(
        "This browser cannot start the isolated Linux runtime. Recorded evidence remains available.",
      );
    await navigator.serviceWorker.register("./isolate-sw.js", { scope: "./" });
    await navigator.serviceWorker.ready;
    sessionStorage.setItem("harmony-start-linux", "1");
    location.reload();
    return;
  }
  const css = document.createElement("link");
  css.rel = "stylesheet";
  css.href = "vendor/xterm.css";
  document.head.append(css);
  await script("vendor/xterm.js");
  await script("vendor/xterm-pty.js");
  const term = new window.Terminal({
    cols: 105,
    rows: 24,
    convertEol: true,
    theme: { background: "#080e0f", foreground: "#c9ded1" },
    fontSize: 12,
  });
  term.open(container);
  const { master, slave } = window.openpty();
  term.loadAddon(master);
  let output = "",
    waiting,
    ready = false,
    busy = false,
    interactive = false,
    shellReady = false;
  const filter = new ConsoleFilter();
  const originalWrite = term.write.bind(term);
  term.write = (data, ...args) => {
    const text =
      typeof data === "string" ? data : new TextDecoder().decode(data);
    output += text;
    const shown = filter.write(text);
    if (interactive && /(?:^|[\r\n])# $/.test(output)) {
      shellReady = true;
      status("Guest shell ready · exit saves this branch");
    }
    if (shown) originalWrite(shown, ...args);
    else if (typeof args.at(-1) === "function") queueMicrotask(args.at(-1));
    if (waiting) waiting();
  };
  const waitFor = (predicate, timeout = 600000) =>
    new Promise((resolve, reject) => {
      const timer =
        timeout === null
          ? undefined
          : setTimeout(() => {
              waiting = undefined;
              reject(
                new Error(
                  "The live Linux command timed out. Its terminal output is preserved.",
                ),
              );
            }, timeout);
      waiting = () => {
        if (predicate(output)) {
          clearTimeout(timer);
          waiting = undefined;
          resolve();
        }
      };
      waiting();
    });
  const base = new URL("public/runtime/", location.href);
  const Module = (window.Module = {
    arguments: [
      "-nographic",
      "-M",
      "pc",
      "-m",
      "1024M",
      "-accel",
      "tcg,tb-size=128",
      "-L",
      "/pack-rom",
      "-nic",
      "none",
      "-kernel",
      "/pack-kernel/vmlinuz-virt",
      "-initrd",
      "/pack-initramfs/browser.cpio.gz",
      "-append",
      "console=ttyS0 rdinit=/init nohz=off",
    ],
    locateFile: (p) => new URL(p, base).href,
    mainScriptUrlOrBlob: new URL("out.js", base).href,
    pty: slave,
    preRun: [],
  });
  status("Downloading Linux and Harmony…");
  const response = await fetch(new URL("browser.cpio.gz", base));
  if (!response.ok) throw new Error("The live runtime image is unavailable.");
  const bytes = new Uint8Array(await response.arrayBuffer());
  await script(new URL("load-kernel.js", base).href);
  await script(new URL("load-rom.js", base).href);
  Module.preRun.push((m) => {
    m.FS_createPath("/", "pack-initramfs", true, true);
    m.FS_createDataFile(
      "/pack-initramfs",
      "browser.cpio.gz",
      bytes,
      true,
      true,
      true,
    );
  });
  const { default: init } = await import(new URL("out.js", base).href);
  await init(Module);
  const oldPoll = Module.TTY.stream_ops.poll;
  Module.TTY.stream_ops.poll = function (stream, timeout) {
    if (!slave.readable)
      return (slave.readable ? 1 : 0) | (slave.writable ? 4 : 0);
    return oldPoll.call(stream, timeout);
  };
  status("Booting real Linux locally…");
  await waitFor((s) => s.includes("harmony-linux# "), 180000);
  const run = async (cli, name) => {
    if (busy) throw new Error("A live command is already running.");
    busy = true;
    interactive = cli.includes(" --shell ");
    shellReady = false;
    status(cli);
    const start = output.length;
    const wrapped = `printf '\\036HARMONY_EXEC\\n'; ${cli}; printf '\\036HARMONY_BEGIN\\n'; cat .harmony/runs/${name}/report.json; printf '\\036HARMONY_END\\n'`;
    filter.begin();
    originalWrite(`\r\n$ ${cli}\r\n`);
    term.input(wrapped + "\r", true);
    try {
      await waitFor(
        (s) =>
          s.slice(start).includes("\x1eHARMONY_END") &&
          s.slice(s.lastIndexOf("\x1eHARMONY_END")).includes("harmony-linux# "),
        interactive ? null : 600000,
      );
      const text = output.slice(start).replace(/\r/g, "");
      const begin = text.indexOf("\x1eHARMONY_BEGIN\n");
      const end = text.lastIndexOf("\x1eHARMONY_END");
      if (begin < 0 || end < 0)
        throw new Error("Linux did not return a complete CLI report.");
      const report = JSON.parse(
        text.slice(begin + "\x1eHARMONY_BEGIN\n".length, end),
      );
      onReport(name, report);
      status("Ready · commands execute in this browser");
    } finally {
      busy = false;
      interactive = false;
      shellReady = false;
    }
  };
  await run(
    "harmony debug run --actions /demo/actions.json --name failure",
    "failure",
  );
  ready = true;
  return {
    get ready() {
      return ready && !busy;
    },
    send(text) {
      if (!shellReady) return;
      term.input(text, true);
      term.focus();
    },
    async command(cli) {
      const name = cli.match(/--name ([a-z][a-z0-9-]*)$/)?.[1];
      if (!name) throw new Error("A named branch is required.");
      await run(cli, name);
    },
    term,
  };
}
