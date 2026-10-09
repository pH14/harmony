// SPDX-License-Identifier: AGPL-3.0-or-later
export const TOUR_KEY = "harmony.nova.tour.v1";
export function tourSeen(storage = () => localStorage) {
  try {
    return storage().getItem(TOUR_KEY) === "seen";
  } catch {
    return false;
  }
}
export function rememberTour(storage = () => localStorage) {
  try {
    storage().setItem(TOUR_KEY, "seen");
  } catch {}
}
export function tourPosition(rects, width, height, viewport) {
  const margin = 12,
    gap = 16;
  const clamp = (n, max) => Math.max(margin, Math.min(n, max - margin));
  const anchor = rects[0];
  if (!anchor)
    return {
      x: (viewport.width - width) / 2,
      y: (viewport.height - height) / 2,
    };
  const middle = anchor.left + (anchor.width - width) / 2;
  const candidates = [
    { x: middle, y: anchor.bottom + gap },
    { x: middle, y: anchor.top - height - gap },
    { x: anchor.left - width - gap, y: anchor.top },
    { x: anchor.right + gap, y: anchor.top },
    { x: margin, y: margin },
    { x: viewport.width - width - margin, y: margin },
    { x: margin, y: viewport.height - height - margin },
    { x: viewport.width - width - margin, y: viewport.height - height - margin },
  ].map((p) => ({
    x: clamp(p.x, viewport.width - width),
    y: clamp(p.y, viewport.height - height),
  }));
  const overlap = (p) =>
    rects.reduce(
      (sum, r) =>
        sum +
        Math.max(
          0,
          Math.min(p.x + width, r.right + gap) - Math.max(p.x, r.left - gap),
        ) *
          Math.max(
            0,
            Math.min(p.y + height, r.bottom + gap) - Math.max(p.y, r.top - gap),
          ),
      0,
    );
  return candidates.sort((a, b) => overlap(a) - overlap(b))[0];
}
export class GuidedTour {
  constructor({ steps, beforeStep, onStart, onClose }) {
    Object.assign(this, { steps, beforeStep, onStart, onClose });
    this.epoch = 0;
    this.dialog = document.createElement("dialog");
    this.dialog.id = "guided-tour";
    this.dialog.setAttribute("aria-labelledby", "tour-title");
    this.dialog.setAttribute("aria-describedby", "tour-copy");
    this.dialog.innerHTML = `<svg class="tour-shade" aria-hidden="true"><defs><mask id="tour-mask"><rect width="100%" height="100%" fill="white"/><g id="tour-holes"></g></mask></defs><rect width="100%" height="100%" fill="rgba(8,10,12,.74)" mask="url(#tour-mask)"/><g id="tour-rings"></g></svg><section class="tour-card"><div class="tour-top"><span id="tour-progress"></span><button id="tour-skip">Skip tour</button></div><div aria-live="polite" aria-atomic="true"><h2 id="tour-title"></h2><p id="tour-copy"></p></div><p id="tour-loading" role="status" hidden>Opening a retained route…</p><div class="tour-bottom"><button id="tour-back">Back</button><div><button id="tour-try" hidden>🎮 Try playing</button><button id="tour-next">Next</button></div></div></section>`;
    document.body.append(this.dialog);
    this.find = (id) => this.dialog.querySelector(`#${id}`);
    this.card = this.dialog.querySelector(".tour-card");
    this.find("tour-skip").onclick = () => this.finish();
    this.find("tour-back").onclick = () => this.go(this.index - 1);
    this.find("tour-next").onclick = () =>
      !this.ready
        ? this.go(this.index)
        : this.index === steps.length - 1
          ? this.finish()
          : this.go(this.index + 1);
    this.find("tour-try").onclick = () => this.finish("play");
    this.dialog.addEventListener("cancel", (e) => {
      e.preventDefault();
      this.finish();
    });
    document.addEventListener("focusin", (e) => {
      if (this.open && this.interactive && !this.allowed.some((node) => node.contains(e.target)))
        this.find("tour-next").focus({ preventScroll: true });
    });
    document.addEventListener("keydown", (e) => {
      if (!this.open || !this.interactive) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        this.finish();
      } else if (e.key === "Tab") {
        const controls = this.allowed.flatMap((node) => [...node.querySelectorAll("button,input,select,[tabindex]")])
          .filter((node) => !node.matches(":disabled") && node.tabIndex >= 0 && node.getClientRects().length);
        if (!controls.length) return;
        const index = controls.indexOf(document.activeElement), direction = e.shiftKey ? -1 : 1;
        e.preventDefault();
        controls[(index + direction + controls.length) % controls.length].focus({ preventScroll: true });
      }
    }, true);
    this.dialog.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        this.finish();
      }
    });
  }
  restoreInteraction() {
    for (const [node, inert] of this.disabled || []) node.inert = inert;
    this.disabled = [];
    this.interactive = false;
    this.allowed = [this.dialog];
    this.dialog.classList.remove("interactive");
  }
  setInteraction(elements = []) {
    this.restoreInteraction();
    this.interactive = elements.length > 0;
    this.allowed = [this.dialog, ...elements];
    this.dialog.classList.toggle("interactive", this.interactive);
    if (this.dialog.matches(":modal") === this.interactive) {
      this.dialog.close();
      if (this.interactive) this.dialog.show();
      else this.dialog.showModal();
    }
    if (!this.interactive) return;
    const restrict = (node) => {
      if (this.allowed.includes(node)) return;
      if (this.allowed.some((allowed) => node.contains(allowed))) {
        for (const child of node.children) restrict(child);
      } else {
        this.disabled.push([node, node.inert]);
        node.inert = true;
      }
    };
    for (const child of document.body.children) restrict(child);
  }
  get open() {
    return this.dialog.open;
  }
  start() {
    if (this.open) return;
    this.returnFocus = document.activeElement;
    this.onStart();
    rememberTour();
    this.dialog.showModal();
    this.go(0);
    const follow = () => {
      if (!this.open) return;
      this.position();
      this.animation = requestAnimationFrame(follow);
    };
    follow();
  }
  async go(index) {
    const epoch = ++this.epoch;
    this.index = index;
    this.ready = false;
    this.dialog.dataset.step = index;
    this.targets = [];
    this.setInteraction();
    const step = this.steps[index];
    this.find("tour-title").textContent = step.title;
    this.find("tour-copy").textContent = step.copy;
    this.find("tour-progress").textContent =
      `Guided tour · ${index + 1} / ${this.steps.length}`;
    this.find("tour-back").hidden = index === 0;
    this.find("tour-try").hidden = true;
    this.find("tour-next").disabled = true;
    this.find("tour-next").textContent =
      index === this.steps.length - 1 ? "Explore on your own" : "Next";
    this.find("tour-loading").hidden = index === 0;
    this.find("tour-loading").textContent = "Opening a retained route…";
    this.card.setAttribute("aria-busy", "true");
    this.position();
    try {
      await this.beforeStep(index, () => epoch === this.epoch && this.open);
      if (epoch !== this.epoch || !this.open) return;
      this.targets = step.targets;
      this.setInteraction(step.interactive?.() || []);
      const first = this.targets()[0];
      if (first?.scrollIntoView)
        first.scrollIntoView({
          block: "nearest",
          inline: "nearest",
          behavior: "instant",
        });
      this.ready = true;
      this.find("tour-loading").hidden = true;
      this.find("tour-try").hidden = index !== this.steps.length - 1;
    } catch {
      if (epoch !== this.epoch || !this.open) return;
      this.find("tour-loading").hidden = false;
      this.find("tour-loading").textContent =
        "A route isn’t ready yet. Try again, or skip and return later.";
      this.find("tour-next").textContent = "Try again";
    }
    this.find("tour-next").disabled = false;
    this.card.setAttribute("aria-busy", "false");
    this.position();
    this.find("tour-next").focus({ preventScroll: true });
  }
  position() {
    const viewport = { width: innerWidth, height: innerHeight };
    const rects = (typeof this.targets === "function" ? this.targets() : [])
      .filter(Boolean)
      .map((target) => {
        const r = target.getBoundingClientRect
          ? target.getBoundingClientRect()
          : target;
        const left = Math.max(8, r.left - 6),
          top = Math.max(8, r.top - 6);
        const right = Math.min(viewport.width - 8, r.right + 6),
          bottom = Math.min(viewport.height - 8, r.bottom + 6);
        return {
          left,
          top,
          right,
          bottom,
          width: right - left,
          height: bottom - top,
        };
      })
      .filter((r) => r.width > 0 && r.height > 0);
    this.card.style.width = `${Math.min(340, viewport.width - 24)}px`;
    const { width, height } = this.card.getBoundingClientRect();
    const p = tourPosition(rects, width, height, viewport);
    this.card.style.left = `${p.x}px`;
    this.card.style.top = `${p.y}px`;
    const shape = (r) =>
      `x="${r.left}" y="${r.top}" width="${r.width}" height="${r.height}" rx="5"`;
    const signature = JSON.stringify(rects);
    if (signature !== this.signature) {
      this.signature = signature;
      this.find("tour-holes").innerHTML = rects
        .map((r) => `<rect ${shape(r)} fill="black"/>`)
        .join("");
      this.find("tour-rings").innerHTML = rects
        .map(
          (r) =>
            `<rect ${shape(r)} fill="none" stroke="#cad6da" stroke-width="1.5"/>`,
        )
        .join("");
    }
  }
  async finish(intent) {
    if (!this.open) return;
    const epoch = ++this.epoch,
      returnFocus = this.returnFocus,
      closingFocus = document.activeElement;
    cancelAnimationFrame(this.animation);
    this.dialog.close();
    this.restoreInteraction();
    await this.onClose(intent);
    if (
      intent !== "play" &&
      epoch === this.epoch &&
      !this.open &&
      (document.activeElement === document.body ||
        document.activeElement === returnFocus ||
        document.activeElement === closingFocus)
    )
      returnFocus?.focus({ preventScroll: true });
  }
}
