// SPDX-License-Identifier: AGPL-3.0-or-later
#include <dqlite.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv)
{
	dqlite_node *node = NULL;
	sigset_t stop;
	int sig;

	if (argc != 4) {
		fprintf(stderr, "usage: %s ID ADDRESS DIR\n", argv[0]);
		return 2;
	}
	signal(SIGPIPE, SIG_IGN);
	sigemptyset(&stop);
	sigaddset(&stop, SIGINT);
	sigaddset(&stop, SIGTERM);
	pthread_sigmask(SIG_BLOCK, &stop, NULL);

	if (dqlite_node_create(strtoull(argv[1], NULL, 10), argv[2], argv[3], &node) != 0 ||
	    dqlite_node_set_bind_address(node, argv[2]) != 0 ||
	    dqlite_node_set_network_latency_ms(node, 70) != 0 ||
	    dqlite_node_start(node) != 0) {
		fprintf(stderr, "dqlite node %s: %s\n", argv[1], dqlite_node_errmsg(node));
		return 1;
	}
	fprintf(stderr, "dqlite node %s serving %s\n", argv[1], argv[2]);
	sigwait(&stop, &sig);
	dqlite_node_stop(node);
	dqlite_node_destroy(node);
	return 0;
}
