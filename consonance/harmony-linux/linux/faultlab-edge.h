// SPDX-License-Identifier: AGPL-3.0-or-later
// Edge counting for a workload compiled with -fsanitize-coverage=trace-pc,
// and the reach markers the instrumented SQLite source calls. See
// faultlab-edge.c.
#ifndef FAULTLAB_EDGE_H
#define FAULTLAB_EDGE_H

// Reads the pause point from the environment; call once at start.
// FAULTLAB_EDGE names an edge count, FAULTLAB_PAUSE_AT and FAULTLAB_PAUSE_K
// the k-th passage of a reach marker, FAULTLAB_EDGE_SLEEP_US the length,
// FAULTLAB_REACH_ALL logs every marker passage, FAULTLAB_JITTER_FILE names
// the file the fault agent writes to switch random pauses on, and
// FAULTLAB_JITTER_LOG logs each random pause with its distance past the
// latest FAULTLAB_PAUSE_AT passage.
void faultlab_edge_init(void);

// Reports that control reached a named state, once per process per name.
void faultlab_reach(const char *name);

#define ANT_REACH(name) faultlab_reach(name)

#endif
