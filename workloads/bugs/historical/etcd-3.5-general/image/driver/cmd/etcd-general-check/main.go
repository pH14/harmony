// SPDX-License-Identifier: AGPL-3.0-or-later

// etcd-general-check reads gen/ serializably from every member, then the
// clients' journals, and checks the owned keys of each member that answered
// against the acknowledged history, at that member's own read revision. It
// then compares HashKV across the members that answered at their lowest
// common revision. A member that does not answer is skipped; the check is
// inconclusive only when no member answers or the journals cannot be read.
package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	clientv3 "go.etcd.io/etcd/client/v3"

	"harmony-etcd-general/choice"
	"harmony-etcd-general/history"
)

const (
	requestTimeout = 15 * time.Second
	comparedID     = "etcd general check compared the members that answered"
	everyID        = "etcd general check compared every member"
	historyID      = "every etcd member holds an acknowledged history"
	hashID         = "etcd members agree on the key-value hash at a common revision"
)

type member struct {
	endpoint string
	revision int64
	values   map[string]history.Value
}

func main() {
	choice.DeclareReachable(comparedID)
	choice.DeclareReachable(everyID)
	choice.DeclareAlways(historyID)
	choice.DeclareAlways(hashID)
	if len(os.Args) < 3 {
		fmt.Fprintln(os.Stderr, "usage: etcd-general-check JOURNAL_DIR ENDPOINT...")
		os.Exit(2)
	}
	if err := run(os.Args[1], os.Args[2:], read, hashAt); err != nil {
		fmt.Fprintln(os.Stderr, err)
	}
}

func run(dir string, endpoints []string, read func(string) (member, error), hashAt func([]string, []member) ([]hash, error)) error {
	members := make([]member, 0, len(endpoints))
	answered := make([]string, 0, len(endpoints))
	for _, endpoint := range endpoints {
		m, err := read(endpoint)
		if err != nil {
			fmt.Fprintf(os.Stderr, "skipped %s: %v\n", endpoint, err)
			continue
		}
		members = append(members, m)
		answered = append(answered, endpoint)
	}
	if len(members) == 0 {
		return fmt.Errorf("inconclusive: no member answered")
	}
	journals, err := load(dir)
	if err != nil {
		return fmt.Errorf("inconclusive: journal: %w", err)
	}
	choice.Reached(comparedID)
	if len(members) == len(endpoints) {
		choice.Reached(everyID)
	}
	problems := checkMembers(journals, members)
	choice.Always(historyID, len(problems) == 0, strings.Join(problems, "; "))
	if len(members) > 1 {
		if hashes, err := hashAt(answered, members); err == nil {
			if problem, comparable := compareHashes(hashes); comparable {
				choice.Always(hashID, problem == "", problem)
			}
		}
	}
	if len(problems) == 0 {
		fmt.Printf("verified %d keys on %d of %d members\n", len(journals.Keys()), len(members), len(endpoints))
	}
	return nil
}

func read(endpoint string) (member, error) {
	cli, err := clientv3.New(clientv3.Config{Endpoints: []string{endpoint}, DialTimeout: requestTimeout})
	if err != nil {
		return member{}, err
	}
	defer cli.Close()
	ctx, cancel := context.WithTimeout(context.Background(), requestTimeout)
	defer cancel()
	response, err := cli.Get(ctx, "gen/", clientv3.WithPrefix(), clientv3.WithSerializable())
	if err != nil {
		return member{}, err
	}
	if response.Header == nil {
		return member{}, fmt.Errorf("no header")
	}
	m := member{endpoint: endpoint, revision: response.Header.Revision, values: map[string]history.Value{}}
	for _, kv := range response.Kvs {
		m.values[string(kv.Key)] = history.Value{Present: true, Data: string(kv.Value), ModRevision: kv.ModRevision}
	}
	return m, nil
}

func load(dir string) (history.Journals, error) {
	paths, err := filepath.Glob(filepath.Join(dir, "general-*"))
	if err != nil {
		return nil, err
	}
	sort.Strings(paths)
	journals := make(history.Journals, 0, len(paths))
	for _, path := range paths {
		file, err := os.Open(path)
		if err != nil {
			return nil, err
		}
		journal := history.NewJournal()
		err = journal.Read(file)
		file.Close()
		if err != nil {
			return nil, err
		}
		journals = append(journals, journal)
	}
	return journals, nil
}

func checkMembers(journal history.Journals, members []member) []string {
	var problems []string
	for _, m := range members {
		keys := map[string]bool{}
		for _, key := range journal.Keys() {
			keys[key] = true
		}
		for key := range m.values {
			keys[key] = true
		}
		sorted := make([]string, 0, len(keys))
		for key := range keys {
			sorted = append(sorted, key)
		}
		sort.Strings(sorted)
		for _, key := range sorted {
			if problem := history.Explain(journal.OnKey(key), m.revision, m.values[key]); problem != "" {
				problems = append(problems, fmt.Sprintf("%s %s: %s", m.endpoint, key, problem))
			}
		}
	}
	return problems
}

type hash struct {
	endpoint string
	hash     uint32
	compact  int64
	revision int64
}

func hashAt(endpoints []string, members []member) ([]hash, error) {
	revision := members[0].revision
	for _, m := range members {
		if m.revision < revision {
			revision = m.revision
		}
	}
	cli, err := clientv3.New(clientv3.Config{Endpoints: endpoints, DialTimeout: requestTimeout})
	if err != nil {
		return nil, err
	}
	defer cli.Close()
	var hashes []hash
	for _, endpoint := range endpoints {
		ctx, cancel := context.WithTimeout(context.Background(), requestTimeout)
		response, err := cli.HashKV(ctx, endpoint, revision)
		cancel()
		if err != nil {
			return nil, err
		}
		hashes = append(hashes, hash{endpoint: endpoint, hash: response.Hash, compact: response.CompactRevision, revision: revision})
	}
	return hashes, nil
}

func compareHashes(hashes []hash) (string, bool) {
	for _, h := range hashes[1:] {
		if h.compact != hashes[0].compact {
			return "", false
		}
	}
	for _, h := range hashes[1:] {
		if h.hash != hashes[0].hash {
			return fmt.Sprintf("HashKV at revision %d (compacted at %d): %s=%d, %s=%d",
				h.revision, h.compact, hashes[0].endpoint, hashes[0].hash, h.endpoint, h.hash), true
		}
	}
	return "", true
}
