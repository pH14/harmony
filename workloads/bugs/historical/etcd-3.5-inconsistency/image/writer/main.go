// SPDX-License-Identifier: AGPL-3.0-or-later

// etcd-writer keeps four independent client connections busy putting unique
// keys through the cluster. It is deliberately built separately from the
// Antithesis-instrumented etcd server.
package main

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"log"
	"os"
	"regexp"
	"strconv"
	"sync"
	"time"

	clientv3 "go.etcd.io/etcd/client/v3"
)

const journalPath = "/tmp/etcd/journal/acked"

const workers = 4

// Sequence numbers are zero padded so journal and member keys have a stable
// byte order.
const sequenceDigits = 12

// A put that is not acknowledged within this bound is retried on the same key.
// Without a deadline, a request already dispatched to a member that is then
// killed holds its worker until the client's own teardown notices.
const putTimeout = time.Second

var clusterEndpoints = []string{
	"http://127.0.0.1:2379",
	"http://127.0.0.1:2381",
	"http://127.0.0.1:2383",
}
var entryPattern = regexp.MustCompile(
	`^museum/([0-9]+)/key-([0-9]+)\tvalue-([0-9]+)-([0-9]+)\t([0-9]+)$`,
)

func main() {
	resumed, err := resumeSequences(journalPath)
	if err != nil {
		log.Fatal(err)
	}

	journal, err := os.OpenFile(journalPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		log.Fatal(err)
	}
	defer journal.Close()

	var journalMu sync.Mutex
	for worker := 1; worker <= workers; worker++ {
		client, err := clientv3.New(clientv3.Config{Endpoints: clusterEndpoints})
		if err != nil {
			log.Fatal(err)
		}
		go writeForever(worker, resumed[worker], client, journal, &journalMu)
	}

	select {}
}

// resumeSequences reports the highest sequence each worker has already had
// acknowledged. A restarted writer continues past those, so it cannot rewrite
// a key an earlier incarnation recorded and repair a value the bug has lost.
func resumeSequences(path string) (map[int]int, error) {
	highest := make(map[int]int, workers)
	file, err := os.Open(path)
	if err != nil {
		if os.IsNotExist(err) {
			return highest, nil
		}
		return nil, err
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 0, 64*1024), 1024*1024)
	for scanner.Scan() {
		worker, sequence, _, ok := parseEntry(scanner.Text())
		if !ok {
			// Workers append concurrently, so a journal left behind by a
			// killed writer can end in a partial record.
			continue
		}
		if sequence > highest[worker] {
			highest[worker] = sequence
		}
	}
	if err := scanner.Err(); err != nil {
		return nil, err
	}
	return highest, nil
}

// parseEntry reports the worker and sequence of one complete journal record.
func parseEntry(line string) (int, int, int64, bool) {
	fields := entryPattern.FindStringSubmatch(line)
	if fields == nil {
		return 0, 0, 0, false
	}
	if fields[1] != fields[3] || fields[2] != fields[4] {
		return 0, 0, 0, false
	}
	worker, err := strconv.Atoi(fields[1])
	if err != nil || worker < 1 || worker > workers {
		return 0, 0, 0, false
	}
	sequence, err := strconv.Atoi(fields[2])
	if err != nil || sequence < 1 {
		return 0, 0, 0, false
	}
	revision, err := strconv.ParseInt(fields[5], 10, 64)
	if err != nil || revision < 1 {
		return 0, 0, 0, false
	}
	return worker, sequence, revision, true
}

func writeForever(
	worker, resumed int,
	client *clientv3.Client,
	journal *os.File,
	journalMu *sync.Mutex,
) {
	for sequence := resumed + 1; ; sequence++ {
		key, value := entry(worker, sequence)

		// Retry the same key until it is acknowledged. Advancing to the next
		// key on failure would leave the sequence with holes the oracle cannot
		// attribute; rewriting an earlier key could repair a lost value.
		var revision int64
		for {
			ctx, cancel := context.WithTimeout(context.Background(), putTimeout)
			response, err := client.Put(ctx, key, value)
			cancel()
			if err == nil && response != nil && response.Header != nil && response.Header.Revision > 0 {
				revision = response.Header.Revision
				break
			}
		}

		if err := appendJournal(journal, journalMu, key, value, revision); err != nil {
			log.Fatal(err)
		}
	}
}

func entry(worker, sequence int) (string, string) {
	return fmt.Sprintf("museum/%d/key-%0*d", worker, sequenceDigits, sequence),
		fmt.Sprintf("value-%d-%0*d", worker, sequenceDigits, sequence)
}

func appendJournal(journal *os.File, journalMu *sync.Mutex, key, value string, revision int64) error {
	if revision < 1 {
		return fmt.Errorf("revision must be positive")
	}
	line := fmt.Sprintf("%s\t%s\t%d\n", key, value, revision)
	journalMu.Lock()
	defer journalMu.Unlock()
	n, err := io.WriteString(journal, line)
	if err == nil && n != len(line) {
		return io.ErrShortWrite
	}
	return err
}
