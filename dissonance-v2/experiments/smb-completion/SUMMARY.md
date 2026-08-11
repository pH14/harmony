# SMB completion experiment summary

Status: in progress.

- Base commit: `8f2b522c26c6f192f2db45a430bec03ed447cad7`
- Starting champion: M12 restart control, seed `0x5eed_dc00`, 500 executions, max-x bucket 62; input SHA-256 `f5ea861ef624b4033684a13e077e6faa66ebfdef0681d276153424bb63413aee`; complete observation SHA-256 `d00f3b0760b1cf58fdb86827b25faa59e50b2010695195da2583ff11080e3095`; exact no-model replay verified.
- Accepted H1 champion: snapshot-backed quality-diversity suffix search, held-out seed `0x5eed_e104`, 5,000 executions, max-x bucket 177; input SHA-256 `92c879eea15988b8818f5a1d5b02a834a6761829506261ffe2acecfbf7af1b83`; complete observation SHA-256 `3c61407dab5eb6cff52bb5438617adf704a5e132c744eb5bf89d9450fc5f63f3`; full no-model campaign replay verified.
- Accepted H1 scale milestone: seed `0x5eed_e104`, 20,000 executions, reached the 1-1 flag at execution 5,758; byte-identical full-report replay SHA-256 `359db709fd50ec0e3eefb980383d2d2c33810d787281aa9b66eaa0fe945c0136`.
- Accepted H3 champion: generic stratified short/long temporal coverage, held-out seed `0x5eed_e102`, reached 1-2 at execution 273 and reproduced a complete 5,000-execution campaign exactly; input SHA-256 `c866fc96ad7aa3ab4a2711cb52ee54a66ff961fa410d0d12f69cbdb23c446af3`; observation SHA-256 `70f9c3952c06f92da6d4e2f05373a263963c55687ff681eb7e03c1ed07e67310`; report SHA-256 `6b4e401946ba8bf49911a5f7fba7e2ba22bb30d53ee01e7730f47e1a64b5adc1`.
- Winning input: pending.
- Final victory/credits replay: pending.
- Warp used: pending.
- Necessary architectural changes: pending measured experiments.
