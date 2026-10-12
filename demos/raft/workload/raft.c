// SPDX-License-Identifier: AGPL-3.0-or-later
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

/* Teaching subset: one log entry, three members, no membership changes.
 * The deliberate bug acknowledges a local append before quorum replication.
 * All messages are real Unix datagrams between separately supervised processes.
 */
enum { FOLLOWER, CANDIDATE, LEADER };
enum { HEARTBEAT, REQUEST_VOTE, VOTE, APPEND, ACK };
typedef struct {
  int type, from, term, value, index;
} Message;
static int me, fd, term = 1, role = FOLLOWER, voted = -1, value, index_value,
                   votes, acks, committed;
static unsigned sequence;
static double deadline, last_heartbeat;
static const char *names[] = {"A", "B", "C"};
static double now(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec + t.tv_nsec / 1e9;
}
static int exists(const char *path) { return access(path, F_OK) == 0; }
static void touch(const char *path) {
  int f = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
  if (f >= 0)
    close(f);
}
static void event(int line, const char *kind, const char *format, ...) {
  char message[400];
  va_list args;
  va_start(args, format);
  vsnprintf(message, sizeof(message), format, args);
  va_end(args);
  printf(
      "RAFT "
      "{\"time\":%.6f,\"node\":\"%s\",\"seq\":%u,\"file\":\"raft.c\",\"line\":%"
      "d,\"kind\":\"%s\",\"term\":%d,\"role\":%d,\"message\":\"%s\"}\n",
      now(), names[me], sequence++, line, kind, term, role, message);
  fflush(stdout);
}
#define EVENT(kind, ...) event(__LINE__, kind, __VA_ARGS__)
#define TRACE(...)                                                             \
  do {                                                                         \
    if (exists("/tmp/raft/trace"))                                             \
      EVENT("trace", __VA_ARGS__);                                             \
  } while (0)
static void save(void) {
  char path[80], tmp[80];
  snprintf(path, sizeof(path), "/tmp/raft/%s.state", names[me]);
  snprintf(tmp, sizeof(tmp), "%s.new", path);
  FILE *f = fopen(tmp, "w");
  if (!f)
    exit(2);
  fprintf(f, "%d %d %d %d %d\n", term, role, value, index_value, committed);
  fclose(f);
  rename(tmp, path);
}
static int send_message(int to, int type) {
  if (exists("/tmp/raft/partition") && (me == 0 || to == 0)) {
    TRACE("drop packet to %s: partition", names[to]);
    return -1;
  }
  struct sockaddr_un address = {.sun_family = AF_UNIX};
  snprintf(address.sun_path, sizeof(address.sun_path), "/tmp/raft/%s.sock",
           names[to]);
  Message m = {type, me, term, value, index_value};
  ssize_t n = sendto(fd, &m, sizeof(m), 0, (void *)&address, sizeof(address));
  return n == sizeof(m) ? 0 : -1;
}
static void broadcast(int type) {
  for (int i = 0; i < 3; i++)
    if (i != me)
      send_message(i, type);
}
static void reset_timer(void) { deadline = now() + .7 + me * .17; }
static void acknowledge(void) {
  if (exists("/tmp/raft/ack"))
    return;
  touch("/tmp/raft/ack");
  EVENT("acknowledged", "client write acknowledged: counter=1");
  TRACE("ack sent with replica acknowledgments=%d; required=2",
        __builtin_popcount((unsigned)acks));
}
static void append_client(void) {
  if (role != LEADER || me != 0 || value || !exists("/tmp/raft/write"))
    return;
  value = 1;
  index_value = 1;
  acks = 1;
  save();
  EVENT("append", "appended counter=1 to local log");
  broadcast(APPEND);
  if (!exists("/tmp/raft/require-quorum")) {
    acknowledge();
  }
}
static int check(void);
static void receive_message(Message m) {
  if (m.from < 0 || m.from >= 3)
    return;
  if (exists("/tmp/raft/partition") && (me == 0 || m.from == 0))
    return;
  if (m.term > term) {
    term = m.term;
    role = FOLLOWER;
    voted = -1;
    save();
    EVENT("term", "observed newer term; became follower");
  }
  if (m.term < term)
    return;
  if (m.type == HEARTBEAT || m.type == APPEND) {
    role = FOLLOWER;
    reset_timer();
    if (m.type == APPEND) {
      value = m.value;
      index_value = m.index;
      save();
      EVENT("replicated", "stored entry from %s", names[m.from]);
      send_message(m.from, ACK);
    }
  } else if (m.type == REQUEST_VOTE) {
    if ((voted < 0 || voted == m.from) && m.index >= index_value) {
      voted = m.from;
      reset_timer();
      save();
      send_message(m.from, VOTE);
      EVENT("vote", "granted vote to %s", names[m.from]);
    }
  } else if (m.type == VOTE && role == CANDIDATE) {
    votes |= 1 << m.from;
    if (__builtin_popcount((unsigned)votes) >= 2) {
      role = LEADER;
      save();
      EVENT("leader", "majority elected %s leader", names[me]);
      check();
      broadcast(HEARTBEAT);
      last_heartbeat = now();
    }
  } else if (m.type == ACK && role == LEADER) {
    acks |= 1 << m.from;
    if (__builtin_popcount((unsigned)acks) >= 2) {
      committed = index_value;
      save();
      TRACE("majority persisted entry; commit index=%d", committed);
      acknowledge();
    }
  }
}
static int node(int id) {
  me = id;
  role = id == 0 ? LEADER : FOLLOWER;
  voted = id == 0 ? 0 : -1;
  char path[80];
  snprintf(path, sizeof(path), "/tmp/raft/%s.sock", names[id]);
  unlink(path);
  fd = socket(AF_UNIX, SOCK_DGRAM | SOCK_NONBLOCK, 0);
  if (fd < 0)
    return 2;
  struct sockaddr_un address = {.sun_family = AF_UNIX};
  snprintf(address.sun_path, sizeof(address.sun_path), "%s", path);
  if (bind(fd, (void *)&address, sizeof(address)) < 0)
    return 2;
  reset_timer();
  save();
  EVENT("ready", "replica started");
  for (;;) {
    Message m;
    ssize_t n;
    while ((n = recv(fd, &m, sizeof(m), 0)) == (ssize_t)sizeof(m))
      receive_message(m);
    append_client();
    if (role == LEADER && now() - last_heartbeat > .1) {
      broadcast(HEARTBEAT);
      last_heartbeat = now();
      TRACE("sent heartbeats");
    }
    if (role != LEADER && now() > deadline) {
      term++;
      role = CANDIDATE;
      voted = me;
      votes = 1 << me;
      reset_timer();
      save();
      EVENT("election", "heartbeat timeout; requesting votes");
      broadcast(REQUEST_VOTE);
    }
    usleep(10000);
  }
}
static int state(int id, int *t, int *r, int *v, int *idx, int *commit) {
  char path[80];
  snprintf(path, sizeof(path), "/tmp/raft/%s.state", names[id]);
  FILE *f = fopen(path, "r");
  if (!f)
    return 0;
  int ok = fscanf(f, "%d %d %d %d %d", t, r, v, idx, commit) == 5;
  fclose(f);
  return ok;
}
static int check(void) {
  if (!exists("/tmp/raft/ack") || exists("/tmp/raft/violation"))
    return 0;
  for (int i = 1; i < 3; i++) {
    int t, r, v, idx, commit;
    if (state(i, &t, &r, &v, &idx, &commit) && r == LEADER && t > 1 && v != 1) {
      FILE *f = fopen("/dev/harmony", "w");
      if (f) {
        fputs("{\"antithesis_assert\":{\"hit\":true,\"must_hit\":true,\"assert_"
              "type\":\"always\",\"display_type\":\"Always\",\"message\":"
              "\"acknowledged write survives leader "
              "change\",\"condition\":false,\"id\":\"raft-ack-durability\","
              "\"location\":{\"class\":\"raft-demo\",\"function\":\"check\","
              "\"file\":\"raft.c\",\"begin_line\":0,\"begin_column\":0},"
              "\"details\":null}}\n",
              f);
        fclose(f);
      }
      touch("/tmp/raft/violation");
      int caller = me;
      me = i;
      EVENT("violation", "acknowledged counter=1 missing from new leader");
      me = caller;
      return 0;
    }
  }
  return 0;
}
int main(int argc, char **argv) {
  setvbuf(stdout, NULL, _IOLBF, 0);
  if (argc < 2)
    return 2;
  if (!strcmp(argv[1], "node") && argc == 3) {
    int id = atoi(argv[2]);
    return id >= 0 && id < 3 ? node(id) : 2;
  }
  if (!strcmp(argv[1], "setup")) {
    mkdir("/tmp/raft", 0755);
    return 0;
  }
  if (!strcmp(argv[1], "ready")) {
    for (int i = 0; i < 3; i++) {
      int t, r, v, x, c;
      if (!state(i, &t, &r, &v, &x, &c))
        return 1;
    }
    return 0;
  }
  if (!strcmp(argv[1], "check"))
    return check();
  if (!strcmp(argv[1], "partition")) {
    touch("/tmp/raft/partition");
    EVENT("fault", "isolate A from B and C");
    return 0;
  }
  if (!strcmp(argv[1], "heal")) {
    unlink("/tmp/raft/partition");
    EVENT("fault", "heal partition");
    return 0;
  }
  if (!strcmp(argv[1], "write")) {
    touch("/tmp/raft/write");
    EVENT("client", "submit counter=1 to leader A");
    return 0;
  }
  if (!strcmp(argv[1], "trace")) {
    touch("/tmp/raft/trace");
    puts("Detailed replication tracing enabled");
    return 0;
  }
  if (!strcmp(argv[1], "require-quorum")) {
    touch("/tmp/raft/require-quorum");
    puts("Require majority persistence before acknowledgment");
    return 0;
  }
  if (!strcmp(argv[1], "status")) {
    for (int i = 0; i < 3; i++) {
      int t, r, v, idx, c;
      if (state(i, &t, &r, &v, &idx, &c))
        printf("%s term=%d role=%s counter=%d lastIndex=%d commitIndex=%d\n",
               names[i], t,
               r == LEADER      ? "leader"
               : r == CANDIDATE ? "candidate"
                                : "follower",
               v, idx, c);
    }
    return 0;
  }
  fputs("Commands: status, trace, require-quorum, partition, heal, write\n",
        stderr);
  return 2;
}
