# platform component scope proposal

Exact rootfs manifest digest: 54ffb8242d0ddcb2594858b581d5d23c054feb4efeb7965c9eef090b66e90a83.
Exact archive/view digest: ee6b601b9f3dc04f1547fbe5394df62933dd20e1b57139a1c0fe3d6cd40f3f70.
Required labels: startup LD_BIND_NOW=1 before every exec; generated code forbidden;
writable executable memory forbidden; workloads controlled/trusted; loader
overrides forbidden. These are reviewed trusted-execution obligations, not
runtime enforcement claimed by the scanner. The prepared composition binds the
actual default SessionConfig (including pre-PID1 binding flags), argv, environment,
mounts and fixed ROM/SQL/config inputs. Only the separately reviewed named Nova A–E composition extends the default
session scope. No other custom session/command/extra process,
module or writable code substitution is supported. The OCI root remains writable.

This accepted refresh covers only the OCI platform archive and exact bundle:null compositions. The separate direct fixture archive and structured bundles are outside this component approval.
