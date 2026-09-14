# platform component scope proposal

Exact rootfs manifest digest: 1a757c74f9cf2d930387f928511f1186742febe0639e17414f2b79c52316b532.
Exact archive/view digest: 733555f781a62f5a2e4d37ae9297efe1c1a347c395586e2cc24b1e38ddf353e3.
Required labels: startup LD_BIND_NOW=1 before every exec; generated code forbidden;
writable executable memory forbidden; workloads controlled/trusted; loader
overrides forbidden. These are reviewed trusted-execution obligations, not
runtime enforcement claimed by the scanner. The prepared composition binds the
actual default SessionConfig (including pre-PID1 binding flags), argv, environment,
mounts and fixed ROM/SQL/config inputs. Only the separately reviewed named Nova A–E composition extends the default
session scope. No other custom session/command/extra process,
module or writable code substitution is supported. The OCI root remains writable.
