# film — the visible replay, rendered by the core itself

`film` answers `(reproducer, Moment) → what the screen showed`. It is the
resolution layer's showpiece: given a discovered game reproducer (task 86's
Super Mario Bros. workload, or any successor over the same seams), it produces
the pictures the guest displayed — **without ever re-running the game with
rendering turned on**, which the one-reproducer rule (`docs/LAYERS.md`) forbids:
extra render instructions would change the guest instruction stream, so the
`Moment`s and every `state_hash` would diverge and the thing filmed would no
longer be the thing the searcher found.

Instead film is a **pure observation query over the one timeline**, in four
passes:

1. **plan** — derive a `FilmPlan` from the reproducer's recorded trace: the
   `REG_FRAME` frame clock gives the `Moment`s, the billboard address registers
   give the read window (chunked to the task-80 length cap). The plan is a
   serializable, inspectable artifact — the query itself is replayable.
2. **project** — over the task-82 session client: materialize the reproducer
   once, then per frame `read` the billboard, **verify its header** (a
   frame-counter mismatch is a hard error), store the capture, and `run` to the
   next frame. Only observation/navigation verbs are sent, so the filmed replay
   is **hash-neutral** — the same timeline the searcher found.
3. **render** — each capture through the `FrameRenderer` seam. `CoreReplay` (the
   one production impl) loads the capture's savestate into the *same
   commit-pinned libretro core* and runs exactly one frame with the recorded
   joypad byte. The picture is the core's own rendering — **1:1 by
   construction**, no reconstruction whose fidelity could be doubted.
4. **output** — a PPM (P6) frame sequence and a contact sheet, with zero new
   dependencies. The committed artifact is a `blake3` digest; rendered game
   frames are never committed (they are the publisher's imagery).

## Build features

| Feature | Pulls | What |
|---|---|---|
| *(default)* | — | the library: plan / project / render seam / stamp renderer / output / mock. No `unsafe`, no core, no ROM. |
| `cli` | `clap` | the `film` driver binary. |
| `core-replay` | `libc` | the real `CoreReplay` renderer — the libretro C-ABI FFI. Compile-checked everywhere; only useful on the box. |

All `unsafe` lives behind the `core-replay` feature and the `FrameRenderer` seam.
The frontend-callback `unsafe` (pixel decode, input) is exercised under Miri with
synthetic buffers — the documented invocation is `cargo +nightly-2026-06-16 miri
test -p film --features core-replay`; only the `dlopen`/`retro_*` FFI (which Miri
cannot execute) sits behind the `CoreReplay::load` seam.

## The `film` binary

```sh
# derive a plan from a REG_FRAME trace (JSON: [{ "frame": u32, "moment": u64 }, …])
film plan --trace trace.json --gpa 0x2000000 --len 30720 \
     --clip-frames 100 400 --stride 2 -o clip.plan.json

# render a captured bundle to PPM frames + a contact sheet (prints blake3 hashes)
film render --bundle clip.bundle.json --out-dir frames/ --contact-cols 12 --core-replay

# end-to-end demo against the in-crate mock server (no core, no ROM)
film demo --frames 300 --out-dir demo/ --contact-cols 20
```

The **capture** pass (`film()`) needs a live control-transport server; on the box
the foreman wires a real socket `Server` (post-80/82 merge) and calls `film()`,
then `film render` renders the resulting bundle. `film demo` shows the whole loop
on a laptop against the mock.

### ROM / core provisioning (box)

The core and ROM are never committed or fetched by this repo (same discipline as
task 86). `CoreReplay::from_env` reads:

- `HARMONY_SMB_CORE` — path to the commit-pinned libretro core `.so`,
- `HARMONY_SMB_ROM` — path to the user-supplied ROM dump.

When either is unset, rendering **SKIPs loudly** (falls back to the stamp
renderer with a warning) rather than silently producing nothing.

## Encoding video (outside the repo)

Video encoding stays out of the repo. Turn a PPM sequence into a video with
ffmpeg:

```sh
# frames/frame-0000.ppm, frame-0001.ppm, …  →  clip.mp4 (60 fps, NES pixels)
ffmpeg -framerate 60 -i frames/frame-%04d.ppm \
       -vf "scale=iw*3:ih*3:flags=neighbor" -pix_fmt yuv420p clip.mp4
```

## Gates

Portable (macOS + Linux, no ROM/core): `FilmPlan` derivation proptested,
header-verification corruption classes, the projector driven end-to-end against
the mock server with the stamp renderer, PPM/contact-sheet goldens. The one live
box gate (render determinism + hash-neutrality on real KVM) is handed to the
foreman — see `IMPLEMENTATION.md`.
