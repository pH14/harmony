// SPDX-License-Identifier: AGPL-3.0-or-later

// etcd-reader writes one member's view of the key spans named on a bounds
// file, as the alternating key and value lines etcdctl prints.
//
// The oracle needs one range per writer worker per member on every check, and
// it runs on the guest's single processor. Spawning a client process for each
// of those ranges costs more than the reads do, and the cost lands exactly
// when a member has just restarted and the oracle is trying to decide whether
// it is behind or has lost data. One process holding one connection answers
// the whole member.
package main

import (
	"bufio"
	"context"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	clientv3 "go.etcd.io/etcd/client/v3"
)

// Sequence numbers are zero padded so a key's byte order matches its numeric
// order, which is what makes one range per worker exact.
const sequenceDigits = 12

// A member that is down must be reported as unreadable rather than waited on:
// the oracle stays silent either way, and a slow answer takes the processor
// the member needs to come back.
const requestTimeout = time.Second

func main() {
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: etcd-reader <endpoint> <bounds-file>")
		os.Exit(2)
	}
	endpoint, boundsPath := os.Args[1], os.Args[2]

	bounds, err := readBounds(boundsPath)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}

	client, err := clientv3.New(clientv3.Config{
		Endpoints:   []string{endpoint},
		DialTimeout: requestTimeout,
	})
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	defer client.Close()

	out := bufio.NewWriter(os.Stdout)
	for _, span := range bounds {
		from := key(span.worker, span.low)
		// A range end is exclusive.
		to := key(span.worker, span.high+1)
		ctx, cancel := context.WithTimeout(context.Background(), requestTimeout)
		response, err := client.Get(ctx, from,
			clientv3.WithRange(to), clientv3.WithSerializable())
		cancel()
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		for _, pair := range response.Kvs {
			fmt.Fprintf(out, "%s\n%s\n", pair.Key, pair.Value)
		}
	}
	if err := out.Flush(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

type span struct {
	worker    string
	low, high int
}

// readBounds parses the `worker low high` lines the oracle wrote for the
// current window. A malformed line is an error rather than a skipped span, so
// a member is never reported complete over less than the window asked for.
func readBounds(path string) ([]span, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()

	var bounds []span
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		fields := strings.Fields(scanner.Text())
		if len(fields) == 0 {
			continue
		}
		if len(fields) != 3 {
			return nil, fmt.Errorf("malformed bounds line %q", scanner.Text())
		}
		low, err := strconv.Atoi(fields[1])
		if err != nil {
			return nil, err
		}
		high, err := strconv.Atoi(fields[2])
		if err != nil {
			return nil, err
		}
		bounds = append(bounds, span{worker: fields[0], low: low, high: high})
	}
	return bounds, scanner.Err()
}

func key(worker string, sequence int) string {
	return fmt.Sprintf("museum/%s/key-%0*d", worker, sequenceDigits, sequence)
}
