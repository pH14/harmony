// SPDX-License-Identifier: AGPL-3.0-or-later
#define main raft_main
#include "../raft.c"
#undef main
#include <assert.h>

int main(void) {
  assert(mkdir("/tmp/raft", 0755) == 0);
  touch("/tmp/raft/ack");
  FILE *other = fopen("/tmp/raft/B.state", "w");
  assert(other);
  fputs("2 2 0 0 0\n", other);
  fclose(other);
  me = 2;
  role = LEADER;
  term = 3;
  assert(check() == 0);
  assert(exists("/tmp/raft/violation"));
  assert(me == 2);
  return 0;
}
