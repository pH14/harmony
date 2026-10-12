// SPDX-License-Identifier: AGPL-3.0-or-later
import { ConsoleFilter } from "./console-filter.mjs";
import {
  CommandSession,
  ConsoleDecoder,
  complete,
  report,
} from "./command-session.mjs";
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
    if (sessionStorage.getItem("harmony-isolation-attempt"))
      throw new Error(
        "This browser could not isolate the Linux runtime. Open this page directly in desktop Chrome; recorded playback remains available.",
      );
    sessionStorage.setItem("harmony-isolation-attempt", "1");
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
  sessionStorage.removeItem("harmony-isolation-attempt");
  const css = document.createElement("link");
  css.rel = "stylesheet";
  css.href = "vendor/xterm.css";
  document.head.append(css);
  await script("vendor/xterm.js");
  await script("vendor/xterm-pty.js");
  const term = new window.Terminal({
    disableStdin: true,
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
    interactive = false,
    shellReady = false;
  const filter = new ConsoleFilter();
  const session = new CommandSession(),
    decoder = new ConsoleDecoder();
  const input = (text) => {
    const disabled = term.options.disableStdin;
    term.options.disableStdin = false;
    term.input(text, true);
    term.options.disableStdin = disabled;
  };
  const originalWrite = term.write.bind(term);
  term.write = (data, ...args) => {
    const text = decoder.decode(data);
    output += text;
    const shown = filter.write(text);
    if (interactive && /(?:^|[\r\n])# $/.test(output)) {
      shellReady = true;
      term.options.disableStdin = false;
      status("Guest shell ready · exit saves this branch");
    }
    if (filter.mode === "json") {
      shellReady = false;
      term.options.disableStdin = true;
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
    const id = session.start();
    term.options.disableStdin = true;
    interactive = cli.includes(" --shell ");
    shellReady = false;
    status(cli);
    const start = output.length;
    const wrapped = `printf '\\036HARMONY_EXEC_${id}\\n'; ${cli}; printf '\\036HARMONY_BEGIN_${id}\\n'; cat .harmony/runs/${name}/report.json; printf '\\036HARMONY_END_${id}\\n'`;
    filter.begin(id);
    originalWrite(`\r\n$ ${cli}\r\n`);
    input(wrapped + "\r");
    try {
      await waitFor(
        (s) => complete(s.slice(start).replace(/\r/g, ""), id),
        interactive ? null : 600000,
      );
      const value = report(output.slice(start).replace(/\r/g, ""), id);
      session.finish(id);
      onReport(name, value);
      status("Ready · commands execute in this browser");
    } catch (error) {
      session.fail(error);
      status(session.error);
      throw new Error(session.error);
    } finally {
      term.options.disableStdin = true;
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
      return ready && session.ready;
    },
    get error() {
      return session.error;
    },
    send(text) {
      if (!shellReady) return;
      input(text);
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
