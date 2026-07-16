# SMB exploration report (task 86)

- ROM sha256: `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`
- Faults **off** the whole time: `FaultPolicy::none()`, buggify off — the `quiet` arm.
- Branch budget (identical for every configuration): 32
- Rollout deadline (v-time ns past the sealed base): 2000000000
- Input shaping: window 12 frames, x-bucket 128 px, alphabet `RIGHT:56,RIGHT+B:56,RIGHT+A:48,RIGHT+A+B:48,A:16,LEFT:12,DOWN:12,NEUTRAL:8`

## Configurations

| configuration | seeds | distinct cells (median) | cells IQR [q1, q3] | depth (median) | depth IQR [q1, q3] |
|---|---|---|---|---|---|
| pure-random baseline | 20 | 29.5 | [28.0, 32.5] | 0.0 | [0.0, 0.0] |
| selector v1 (attribution) | 20 | 27.5 | [26.0, 29.5] | 0.0 | [0.0, 0.0] |

## Verdict

**INCOMPLETE** — missing configuration(s): signal — the verdict needs both sides.
