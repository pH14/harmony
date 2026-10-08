// SPDX-License-Identifier: AGPL-3.0-or-later
const markers = {
  start: "\x1eHARMONY_EXEC",
  json: "\x1eHARMONY_BEGIN",
  end: "\x1eHARMONY_END",
};
export class ConsoleFilter {
  mode = "visible";
  buffer = "";
  skipNewline = false;
  begin() {
    this.mode = "start";
    this.buffer = "";
    this.skipNewline = false;
  }
  write(text) {
    this.buffer += text;
    let shown = "";
    for (;;) {
      if (this.skipNewline) {
        const end = /^\r*\n/.exec(this.buffer);
        if (end) this.buffer = this.buffer.slice(end[0].length);
        else if (/^\r*$/.test(this.buffer)) return shown;
        this.skipNewline = false;
      }
      const marker =
        this.mode === "start"
          ? markers.start
          : this.mode === "json"
            ? markers.end
            : markers.json;
      const index = this.buffer.indexOf(marker);
      if (index >= 0) {
        if (this.mode === "visible") shown += this.buffer.slice(0, index);
        this.buffer = this.buffer.slice(index + marker.length);
        this.mode = this.mode === "visible" ? "json" : "visible";
        this.skipNewline = true;
        continue;
      }
      let keep = 0;
      for (let n = 1; n < marker.length; n++)
        if (this.buffer.endsWith(marker.slice(0, n))) keep = n;
      const complete = this.buffer.slice(0, this.buffer.length - keep);
      if (this.mode === "visible") shown += complete;
      this.buffer = keep ? this.buffer.slice(-keep) : "";
      return shown;
    }
  }
}
