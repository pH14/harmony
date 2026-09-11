// SPDX-License-Identifier: AGPL-3.0-or-later

// etcd-writer keeps four independent client connections busy putting unique
// keys through the cluster. It is deliberately built separately from the
// Antithesis-instrumented etcd server.
package main

import (
	"context"
	"fmt"
	"io"
	"log"
	"os"
	"sync"

	clientv3 "go.etcd.io/etcd/client/v3"
)

const journalPath = "/tmp/etcd/journal/acked"

var clusterEndpoints = []string{
	"http://127.0.0.1:2379",
	"http://127.0.0.1:2381",
	"http://127.0.0.1:2383",
}

func main() {
	journal, err := os.OpenFile(journalPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		log.Fatal(err)
	}
	defer journal.Close()

	var journalMu sync.Mutex
	for worker := 1; worker <= 4; worker++ {
		client, err := clientv3.New(clientv3.Config{Endpoints: clusterEndpoints})
		if err != nil {
			log.Fatal(err)
		}
		go writeForever(worker, client, journal, &journalMu)
	}

	select {}
}

func writeForever(worker int, client *clientv3.Client, journal *os.File, journalMu *sync.Mutex) {
	for sequence := 1; ; sequence++ {
		key, value := entry(worker, sequence)

		if _, err := client.Put(context.Background(), key, value); err != nil {
			continue
		}

		err := appendJournal(journal, journalMu, key, value)
		if err != nil {
			log.Fatal(err)
		}
	}
}

func entry(worker, sequence int) (string, string) {
	return fmt.Sprintf("museum/%d/key-%d", worker, sequence),
		fmt.Sprintf("value-%d-%d", worker, sequence)
}

func appendJournal(journal *os.File, journalMu *sync.Mutex, key, value string) error {
	line := key + "\t" + value + "\n"
	journalMu.Lock()
	defer journalMu.Unlock()
	n, err := io.WriteString(journal, line)
	if err == nil && n != len(line) {
		return io.ErrShortWrite
	}
	return err
}
