# Hardware testing

Hardware tests run manually on separately provisioned, qualified hosts. This
public repository must not dispatch jobs to home-network self-hosted runners.
There is no repository-managed hardware runner or reserved CPU allocation.

Before running an ignored hardware test, verify its documented CPU, kernel,
patched KVM, perf, and guest-image requirements. Reserve an idle CPU allocation
on the chosen host and adapt any example taskset CPU numbers to that allocation.
Restore stock KVM modules after testing when the test requires patched modules.
The scripts/box-gates.sh path in older test comments is historical; it is not
an available CI entry point.

The det-cfl-v1 string identifies a frozen Coffee Lake CPU contract, not an
available server or SSH destination. It remains in serialized contracts and
fixtures because changing it changes contract identity. The msr1 host class in
acceptance manifests likewise does not register or connect a GitHub runner.
