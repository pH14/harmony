# postgres exact selector control-flow review proposal

The artifact digests and every XGETBV site are enumerated in postgres-selectors.json.
All retained instruction chains explicitly zero ECX, preserve it through the
bounded whitelist, and contain no observed direct entry bypassing zeroing.
The reviewed interpretation is ordinary compiler/runtime feature-detection
control flow: calls target ABI function entries, not interior XGETBV bytes.
No arbitrary interior function pointers, corrupted control transfers, generated
code or runtime code mutation are supported. This indirect-entry exclusion is
a stated trusted-program assumption for the exact workload/platform code,
not a claim that static linear decoding proves arbitrary binary safety.

- `/bin/busybox` SHA256 `dfcabe58212e7b2d8cabeac17a779f8572a4f97836bbcc35c3c45a98d28f1de0`: 0x4b5859 (7 proof instructions)
- `/lib/x86_64-linux-gnu/libcrypto.so.3` SHA256 `8bb5f3fdffe280d4453eb79a4663c2c47af70b7c247fe2e94e2da703cee1fd3d`: 0x28a4e0 (1 proof instructions)
- `/lib/x86_64-linux-gnu/libgcc_s.so.1` SHA256 `30c61ab012a4241bed033725a09b61f5fdd3bb7df95ee852d0b096520524c7af`: 0x4c7d (1 proof instructions)
- `/lib/x86_64-linux-gnu/libxxhash.so.0` SHA256 `7dd49b353facbee50c371d0559ce29db5afee5ac33249ca5c7068420fb642762`: 0xd2c3 (1 proof instructions)
- `/lib64/ld-linux-x86-64.so.2` SHA256 `c8438e4fde1934e61c88311633f00949ff645d5c04cdb8671fa3d78164d2f307`: 0x163e1 (1 proof instructions), 0x19c0f (1 proof instructions)
- `/usr/lib/postgresql/17/bin/postgres` SHA256 `6468a969338215cb3912cf9c0b894bdbbd37b9a709926db078e9a5bf8bdc3e16`: 0x69b5c2 (1 proof instructions)
