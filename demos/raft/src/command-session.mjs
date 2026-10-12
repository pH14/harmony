// SPDX-License-Identifier: AGPL-3.0-or-later
export class CommandSession {
  sequence = 0;
  active = null;
  error = null;
  get ready() {
    return this.active === null && !this.error;
  }
  start() {
    if (!this.ready)
      throw new Error(this.error || "A live command is already running.");
    return (this.active = String(++this.sequence));
  }
  finish(id) {
    if (this.active !== id || this.error)
      throw new Error("The live session must be reloaded.");
    this.active = null;
  }
  fail(error) {
    this.error = `${error.message} Reload the page before starting another live command.`;
  }
}
export function markers(id = "") {
  const suffix = id ? `_${id}` : "";
  return {
    start: `\x1eHARMONY_EXEC${suffix}`,
    json: `\x1eHARMONY_BEGIN${suffix}`,
    end: `\x1eHARMONY_END${suffix}`,
  };
}
export function complete(output, id) {
  const m = markers(id);
  const end = output.indexOf(m.end + "\n");
  return (
    end >= 0 && output.slice(end + m.end.length).includes("harmony-linux# ")
  );
}
export function report(output, id) {
  const m = markers(id),
    begin = output.indexOf(m.json + "\n"),
    end = output.indexOf(m.end + "\n");
  if (begin < 0 || end < begin)
    throw new Error("Linux did not return this command’s complete CLI report.");
  return JSON.parse(output.slice(begin + m.json.length + 1, end));
}
export class ConsoleDecoder {
  decoder = new TextDecoder();
  decode(data) {
    return typeof data === "string"
      ? data
      : this.decoder.decode(data, { stream: true });
  }
}
