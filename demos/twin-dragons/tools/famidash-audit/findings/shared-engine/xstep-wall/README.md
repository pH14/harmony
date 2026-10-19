# Xstep wall escape: covered by the combined engine repairs

Fresh original-ROM Dissonance round 76 finds a ship entering a closed filled
column late in the built-in Xstep level. Independent FCEUmm verifies the full
4731-frame original input. Bank-aware reads first sample the `X_NO_OUTLINE`
interior at frame 4716 and continue through frame 4731. The earlier body warning
at 4714 is two frames ahead of the actual collision probe.

The retained release shared-history V2 build kills this input at frame 4716 in
QuickNES and FCEUmm, preserving every preceding gameplay observation and resetting
after 40 released frames. No Xstep source tiles change. This is an additional
original-ROM witness covered by the existing combined generic engine repairs;
it does not establish which individual repair suffices on pristine upstream.

The full-round closed-component signal for this location is therefore a body
warning before a correctly committed death. Other Xstep component signals still
need actual-probe and source review. This one is not evidence of a surviving
escape on the repaired build, and earns no clean-round credit by itself.
